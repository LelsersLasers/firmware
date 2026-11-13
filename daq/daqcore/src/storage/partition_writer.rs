use crate::{
    log_parse::{auto_annotate, consts, correlate, parse},
    storage, superdbc, time,
};
use arrow::{
    array::{Float64Builder, Int64Builder, StringBuilder, StringDictionaryBuilder, UInt32Builder},
    datatypes::{DataType, Field, Int32Type, Schema},
    record_batch::RecordBatch,
};
use parquet::{
    arrow::ArrowWriter,
    basic::{Compression, ZstdLevel},
    file::properties::{EnabledStatistics, WriterProperties},
    format::SortingColumn,
};
use sha2::Digest as _;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs, io, path,
    sync::Arc,
};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignalManifestEntry {
    pub signal_id: storage::SignalId,
    pub unit: String,
    pub is_discrete: bool,
    pub sample_count: u64,
    pub min_utc_ms: i64,
    pub max_utc_ms: i64,
    pub min_val: f64,
    pub max_val: f64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PartitionManifest {
    pub partition_id: String,
    pub track_day_id: String,
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    pub clock_mapping: correlate::ClockMapping,
    pub superdbc_hash: String,
    pub raw_log_hashes: Vec<parse::SourceLogFile>,
    pub signals: Vec<SignalManifestEntry>,
    pub annotations: Vec<auto_annotate::Annotation>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImportSummary {
    pub log_files_parsed: usize,
    pub boot_epochs_detected: usize,
    pub partitions_written: usize,
    pub partitions_skipped_existing: usize,
    pub manifests: Vec<PartitionManifest>,
}

#[derive(Debug)]
pub enum ImportError {
    Io(io::Error),
    Parquet(parquet::errors::ParquetError),
    Arrow(arrow::error::ArrowError),
    Json(serde_json::Error),
    Registry(storage::RegistryError),
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Parquet(e) => write!(f, "Parquet error: {e}"),
            Self::Arrow(e) => write!(f, "Arrow error: {e}"),
            Self::Json(e) => write!(f, "JSON serialization error: {e}"),
            Self::Registry(e) => write!(f, "Registry error: {e}"),
        }
    }
}

impl std::error::Error for ImportError {}

impl From<io::Error> for ImportError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<parquet::errors::ParquetError> for ImportError {
    fn from(value: parquet::errors::ParquetError) -> Self {
        Self::Parquet(value)
    }
}

impl From<arrow::error::ArrowError> for ImportError {
    fn from(value: arrow::error::ArrowError) -> Self {
        Self::Arrow(value)
    }
}

impl From<serde_json::Error> for ImportError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

impl From<storage::RegistryError> for ImportError {
    fn from(value: storage::RegistryError) -> Self {
        Self::Registry(value)
    }
}

#[derive(Clone)]
struct RawSample {
    utc_ms: i64,
    ticks_ms: u32,
    physical: f64,
    raw_int: i64,
    enum_label: Option<String>,
}

struct SignalColumnAccumulator {
    unit: String,
    is_discrete: bool,
    is_fault_flag: bool,
    samples: Vec<RawSample>,
}

struct OverviewBucketRow {
    bucket_utc_ms: i64,
    min_val: f64,
    max_val: f64,
    mean_val: f64,
    count: u32,
}

struct StateIntervalRow {
    start_utc_ms: i64,
    end_utc_ms: i64,
    raw_int: i64,
    label: String,
}

fn format_track_day_id(utc_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(utc_ms)
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "1970-01-01".to_owned())
}

fn compute_partition_id(
    start_utc_ms: i64,
    end_utc_ms: i64,
    sub_partition_idx: usize,
    slice: &[(i64, parse::ParsedMessage)],
    raw_logs: &[parse::SourceLogFile],
    superdbc_hash: &str,
) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(start_utc_ms.to_le_bytes());
    hasher.update(end_utc_ms.to_le_bytes());
    hasher.update((sub_partition_idx as u64).to_le_bytes());
    hasher.update((slice.len() as u64).to_le_bytes());
    hasher.update(superdbc_hash.as_bytes());
    // Only include whole-file SHA-256 hashes for immutable RTC-named files;
    // `log-0.log` is appended to across subsequent MCU boots on the same SD card.
    for file in raw_logs {
        if file.rtc_timestamp_ms.is_some() {
            hasher.update(file.sha256_hex.as_bytes());
        }
    }
    if let Some((_, first_msg)) = slice.first() {
        hasher.update(first_msg.timestamp.to_le_bytes());
        hasher.update(first_msg.decoded.name().as_bytes());
    }
    if let Some((_, last_msg)) = slice.last() {
        hasher.update(last_msg.timestamp.to_le_bytes());
        hasher.update(last_msg.decoded.name().as_bytes());
    }
    // Sample up to 64 evenly-spaced messages across the slice so distinct epochs
    // within the same multi-boot file (e.g. log-0.log) never collide.
    let step = (slice.len() / 64).max(1);
    for (_, msg) in slice.iter().step_by(step) {
        hasher.update(msg.timestamp.to_le_bytes());
        hasher.update(msg.decoded.name().as_bytes());
    }
    let digest = hasher.finalize();
    let mut prefix = String::with_capacity(12);
    for byte in &digest[..6] {
        use std::fmt::Write as _;
        let _ = write!(&mut prefix, "{byte:02x}");
    }
    format!("epoch_{start_utc_ms}_{prefix}")
}

/// Slice correlated boot epochs into <= 15-minute sub-partitions and write canonical
/// Parquet pyramids (`full_res.parquet`, `overview_10hz.parquet`, `overview_1hz.parquet`,
/// `states.parquet`, and `manifest.json`) into `<store_dir>/partitions/<partition_id>/`.
pub fn write_correlated_epochs_to_store(
    store_dir: &path::Path,
    log_files: &[parse::SourceLogFile],
    correlated_epochs: Vec<correlate::CorrelationChunkResult>,
    database: &superdbc::database::Database,
) -> Result<ImportSummary, ImportError> {
    let partitions_root = store_dir.join("partitions");
    fs::create_dir_all(&partitions_root)?;
    let registry = storage::Registry::open_store(store_dir)?;

    let mut summary = ImportSummary {
        log_files_parsed: log_files.len(),
        boot_epochs_detected: correlated_epochs.len(),
        partitions_written: 0,
        partitions_skipped_existing: 0,
        manifests: Vec::new(),
    };

    for epoch in correlated_epochs {
        if epoch.parsed_msgs.is_empty() {
            continue;
        }

        let timed_msgs: Vec<(i64, parse::ParsedMessage)> = epoch
            .parsed_msgs
            .into_iter()
            .map(|msg| {
                let utc_ms = epoch.clock_mapping.ticks_to_utc_ms(msg.timestamp);
                (utc_ms, msg)
            })
            .collect();

        // Split into sub-partitions of at most MAX_PARTITION_SPAN_MS (15 minutes)
        let mut sub_partitions: Vec<Vec<(i64, parse::ParsedMessage)>> = Vec::new();
        let mut current_slice: Vec<(i64, parse::ParsedMessage)> = Vec::new();
        let mut slice_start_utc_ms: Option<i64> = None;

        for (utc_ms, msg) in timed_msgs {
            let start_ms = *slice_start_utc_ms.get_or_insert(utc_ms);
            if utc_ms.saturating_sub(start_ms) >= consts::MAX_PARTITION_SPAN_MS
                && !current_slice.is_empty()
            {
                sub_partitions.push(current_slice);
                current_slice = Vec::new();
                slice_start_utc_ms = Some(utc_ms);
            }
            current_slice.push((utc_ms, msg));
        }
        if !current_slice.is_empty() {
            sub_partitions.push(current_slice);
        }

        for (sub_idx, slice) in sub_partitions.into_iter().enumerate() {
            let (manifest, was_written) = write_single_partition(
                &partitions_root,
                sub_idx,
                &slice,
                &epoch.clock_mapping,
                log_files,
                database,
            )?;
            registry.register_manifest(&manifest, true)?;
            if was_written {
                summary.partitions_written += 1;
            } else {
                summary.partitions_skipped_existing += 1;
            }
            summary.manifests.push(manifest);
        }
    }

    Ok(summary)
}

fn write_single_partition(
    partitions_root: &path::Path,
    sub_partition_idx: usize,
    slice: &[(i64, parse::ParsedMessage)],
    clock_mapping: &correlate::ClockMapping,
    all_log_files: &[parse::SourceLogFile],
    database: &superdbc::database::Database,
) -> Result<(PartitionManifest, bool), ImportError> {
    let start_utc_ms = slice.first().map(|(t, _)| *t).unwrap_or(0);
    let end_utc_ms = slice.last().map(|(t, _)| *t).unwrap_or(start_utc_ms);

    let contributing_file_indices: BTreeSet<usize> =
        slice.iter().map(|(_, m)| m.source_file_idx).collect();
    let raw_log_hashes: Vec<parse::SourceLogFile> = contributing_file_indices
        .into_iter()
        .filter_map(|idx| all_log_files.get(idx).cloned())
        .collect();

    let superdbc_hash = database.content_hash().to_owned();
    let partition_id = compute_partition_id(
        start_utc_ms,
        end_utc_ms,
        sub_partition_idx,
        slice,
        &raw_log_hashes,
        &superdbc_hash,
    );

    let partition_dir = partitions_root.join(&partition_id);
    let manifest_path = partition_dir.join("manifest.json");

    // Idempotency check: if a valid manifest and all 4 Parquet files already exist, skip rewriting
    if manifest_path.exists()
        && partition_dir.join("full_res.parquet").exists()
        && partition_dir.join("overview_10hz.parquet").exists()
        && partition_dir.join("overview_1hz.parquet").exists()
        && partition_dir.join("states.parquet").exists()
        && let Ok(existing_bytes) = fs::read(&manifest_path)
        && let Ok(existing_manifest) = serde_json::from_slice::<PartitionManifest>(&existing_bytes)
        && existing_manifest.partition_id == partition_id
    {
        log::info!(
            "Partition {} already exists in store; skipping rewrite",
            partition_id
        );
        return Ok((existing_manifest, false));
    }

    fs::create_dir_all(&partition_dir)?;

    // Build per-signal sorted sample columns
    let mut columns: BTreeMap<storage::SignalId, SignalColumnAccumulator> = BTreeMap::new();

    for (utc_ms, msg) in slice {
        let msg_name = msg.decoded.name();
        let msg_def = database.message_by_key(msg.decoded.key());

        for (sig_name, sig_value) in msg.decoded.iter() {
            let signal_id = storage::format_signal_id(msg_name, sig_name);
            let acc = columns.entry(signal_id).or_insert_with(|| {
                let sig_def = msg_def.and_then(|m| m.signal(sig_name));
                let unit = sig_def.map(|s| s.unit().to_owned()).unwrap_or_default();
                let is_discrete = sig_def.is_some_and(|s| s.choices().next().is_some());
                let is_fault_flag = auto_annotate::is_fault_flag_signal(msg_name, sig_name);
                SignalColumnAccumulator {
                    unit,
                    is_discrete,
                    is_fault_flag,
                    samples: Vec::new(),
                }
            });

            let raw_int = sig_value
                .int_rounded()
                .clamp(i64::MIN as i128, i64::MAX as i128) as i64;

            acc.samples.push(RawSample {
                utc_ms: *utc_ms,
                ticks_ms: msg.timestamp,
                physical: sig_value.physical(),
                raw_int,
                enum_label: sig_value.enum_label().map(str::to_owned),
            });
        }
    }

    // Ensure each signal's samples are ordered by (utc_ms, ticks_ms)
    for acc in columns.values_mut() {
        acc.samples
            .sort_by_key(|sample| (sample.utc_ms, sample.ticks_ms));
    }

    // Write the 4 Parquet files atomically
    write_full_res_parquet(&partition_dir.join("full_res.parquet"), &columns)?;
    write_overview_parquet(
        &partition_dir.join("overview_10hz.parquet"),
        &columns,
        consts::BUCKET_10HZ_MS,
    )?;
    write_overview_parquet(
        &partition_dir.join("overview_1hz.parquet"),
        &columns,
        consts::BUCKET_1HZ_MS,
    )?;
    write_states_parquet(&partition_dir.join("states.parquet"), &columns)?;

    // Build signal manifest entries and run auto-annotation detectors
    let track_day_id = format_track_day_id(start_utc_ms);
    let updated_at_ms = time::Time::now().unix_millis();

    let borrowed_slice: Vec<(i64, &parse::ParsedMessage)> =
        slice.iter().map(|(t, m)| (*t, m)).collect();
    let speed_samples = auto_annotate::extract_speed_samples(&borrowed_slice);
    let mut annotations = auto_annotate::detect_motion_stints(
        &speed_samples,
        &partition_id,
        &track_day_id,
        updated_at_ms,
    );
    annotations.extend(auto_annotate::detect_fault_event_messages(
        &borrowed_slice,
        &track_day_id,
        updated_at_ms,
    ));

    let mut signal_entries = Vec::with_capacity(columns.len());
    for (signal_id, acc) in &columns {
        if acc.samples.is_empty() {
            continue;
        }
        let min_utc_ms = acc.samples.first().map(|s| s.utc_ms).unwrap_or(0);
        let max_utc_ms = acc.samples.last().map(|s| s.utc_ms).unwrap_or(0);
        let mut min_val = f64::INFINITY;
        let mut max_val = f64::NEG_INFINITY;
        for sample in &acc.samples {
            if sample.physical.is_finite() {
                min_val = min_val.min(sample.physical);
                max_val = max_val.max(sample.physical);
            }
        }
        if !min_val.is_finite() {
            min_val = 0.0;
            max_val = 0.0;
        }

        if acc.is_fault_flag {
            let raw_pairs: Vec<(i64, i64)> =
                acc.samples.iter().map(|s| (s.utc_ms, s.raw_int)).collect();
            annotations.extend(auto_annotate::detect_signal_fault_transitions(
                signal_id,
                &raw_pairs,
                &track_day_id,
                updated_at_ms,
            ));
        }

        signal_entries.push(SignalManifestEntry {
            signal_id: signal_id.clone(),
            unit: acc.unit.clone(),
            is_discrete: acc.is_discrete,
            sample_count: acc.samples.len() as u64,
            min_utc_ms,
            max_utc_ms,
            min_val,
            max_val,
        });
    }

    annotations.sort_by_key(|a| (a.start_utc_ms, a.id.clone()));

    let manifest = PartitionManifest {
        partition_id: partition_id.clone(),
        track_day_id,
        start_utc_ms,
        end_utc_ms,
        clock_mapping: clock_mapping.clone(),
        superdbc_hash,
        raw_log_hashes,
        signals: signal_entries,
        annotations,
    };

    let tmp_manifest_path = partition_dir.join("manifest.json.tmp");
    let manifest_json = serde_json::to_vec_pretty(&manifest)?;
    fs::write(&tmp_manifest_path, manifest_json)?;
    fs::rename(&tmp_manifest_path, &manifest_path)?;

    log::info!(
        "Wrote partition {} ([{} .. {}], {} signals, {} auto-annotations)",
        partition_id,
        start_utc_ms,
        end_utc_ms,
        manifest.signals.len(),
        manifest.annotations.len()
    );

    Ok((manifest, true))
}

fn default_writer_properties(sorting_columns: Vec<SortingColumn>) -> WriterProperties {
    WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::default()))
        .set_statistics_enabled(EnabledStatistics::Page)
        .set_sorting_columns(Some(sorting_columns))
        .build()
}

fn write_full_res_parquet(
    target_path: &path::Path,
    columns: &BTreeMap<storage::SignalId, SignalColumnAccumulator>,
) -> Result<(), ImportError> {
    let schema = Arc::new(Schema::new(vec![
        Field::new(
            "signal_id",
            DataType::Dictionary(Box::new(DataType::Int32), Box::new(DataType::Utf8)),
            false,
        ),
        Field::new("utc_ms", DataType::Int64, false),
        Field::new("ticks_ms", DataType::UInt32, false),
        Field::new("value", DataType::Float64, false),
    ]));

    let total_rows: usize = columns.values().map(|c| c.samples.len()).sum();
    let mut signal_id_builder =
        StringDictionaryBuilder::<Int32Type>::with_capacity(total_rows, columns.len().max(1), 1024);
    let mut utc_ms_builder = Int64Builder::with_capacity(total_rows);
    let mut ticks_ms_builder = UInt32Builder::with_capacity(total_rows);
    let mut value_builder = Float64Builder::with_capacity(total_rows);

    for (signal_id, acc) in columns {
        for sample in &acc.samples {
            signal_id_builder.append_value(signal_id);
            utc_ms_builder.append_value(sample.utc_ms);
            ticks_ms_builder.append_value(sample.ticks_ms);
            value_builder.append_value(sample.physical);
        }
    }

    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(signal_id_builder.finish()),
            Arc::new(utc_ms_builder.finish()),
            Arc::new(ticks_ms_builder.finish()),
            Arc::new(value_builder.finish()),
        ],
    )?;

    write_record_batch_atomic(
        target_path,
        schema,
        &batch,
        default_writer_properties(vec![
            SortingColumn::new(0, false, false),
            SortingColumn::new(1, false, false),
        ]),
    )
}

fn build_overview_buckets(samples: &[RawSample], bucket_width_ms: i64) -> Vec<OverviewBucketRow> {
    let mut rows = Vec::new();
    let mut current_bucket_ms: Option<i64> = None;
    let mut min_val = f64::INFINITY;
    let mut max_val = f64::NEG_INFINITY;
    let mut sum_val = 0.0;
    let mut count = 0u32;

    for sample in samples {
        if !sample.physical.is_finite() {
            continue;
        }
        let bucket_ms = sample.utc_ms.div_euclid(bucket_width_ms) * bucket_width_ms;
        match current_bucket_ms {
            Some(active_bucket) if active_bucket == bucket_ms => {
                min_val = min_val.min(sample.physical);
                max_val = max_val.max(sample.physical);
                sum_val += sample.physical;
                count = count.saturating_add(1);
            }
            Some(active_bucket) => {
                if count > 0 {
                    rows.push(OverviewBucketRow {
                        bucket_utc_ms: active_bucket,
                        min_val,
                        max_val,
                        mean_val: (sum_val / f64::from(count)).clamp(min_val, max_val),
                        count,
                    });
                }
                current_bucket_ms = Some(bucket_ms);
                min_val = sample.physical;
                max_val = sample.physical;
                sum_val = sample.physical;
                count = 1;
            }
            None => {
                current_bucket_ms = Some(bucket_ms);
                min_val = sample.physical;
                max_val = sample.physical;
                sum_val = sample.physical;
                count = 1;
            }
        }
    }

    if let Some(active_bucket) = current_bucket_ms
        && count > 0
    {
        rows.push(OverviewBucketRow {
            bucket_utc_ms: active_bucket,
            min_val,
            max_val,
            mean_val: (sum_val / f64::from(count)).clamp(min_val, max_val),
            count,
        });
    }

    rows
}

fn write_overview_parquet(
    target_path: &path::Path,
    columns: &BTreeMap<storage::SignalId, SignalColumnAccumulator>,
    bucket_width_ms: i64,
) -> Result<(), ImportError> {
    let schema = Arc::new(Schema::new(vec![
        Field::new(
            "signal_id",
            DataType::Dictionary(Box::new(DataType::Int32), Box::new(DataType::Utf8)),
            false,
        ),
        Field::new("bucket_utc_ms", DataType::Int64, false),
        Field::new("min_val", DataType::Float64, false),
        Field::new("max_val", DataType::Float64, false),
        Field::new("mean_val", DataType::Float64, false),
        Field::new("count", DataType::UInt32, false),
    ]));

    let mut signal_id_builder =
        StringDictionaryBuilder::<Int32Type>::with_capacity(1024, columns.len().max(1), 1024);
    let mut bucket_utc_ms_builder = Int64Builder::new();
    let mut min_val_builder = Float64Builder::new();
    let mut max_val_builder = Float64Builder::new();
    let mut mean_val_builder = Float64Builder::new();
    let mut count_builder = UInt32Builder::new();

    for (signal_id, acc) in columns {
        let buckets = build_overview_buckets(&acc.samples, bucket_width_ms);
        for bucket in buckets {
            signal_id_builder.append_value(signal_id);
            bucket_utc_ms_builder.append_value(bucket.bucket_utc_ms);
            min_val_builder.append_value(bucket.min_val);
            max_val_builder.append_value(bucket.max_val);
            mean_val_builder.append_value(bucket.mean_val);
            count_builder.append_value(bucket.count);
        }
    }

    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(signal_id_builder.finish()),
            Arc::new(bucket_utc_ms_builder.finish()),
            Arc::new(min_val_builder.finish()),
            Arc::new(max_val_builder.finish()),
            Arc::new(mean_val_builder.finish()),
            Arc::new(count_builder.finish()),
        ],
    )?;

    write_record_batch_atomic(
        target_path,
        schema,
        &batch,
        default_writer_properties(vec![
            SortingColumn::new(0, false, false),
            SortingColumn::new(1, false, false),
        ]),
    )
}

fn build_rle_state_intervals(samples: &[RawSample]) -> Vec<StateIntervalRow> {
    let mut intervals = Vec::new();
    let Some(first) = samples.first() else {
        return intervals;
    };

    let mut current_start_ms = first.utc_ms;
    let mut last_seen_ms = first.utc_ms;
    let mut current_raw = first.raw_int;
    let mut current_label = first
        .enum_label
        .clone()
        .unwrap_or_else(|| format!("RAW_{}", first.raw_int));

    for sample in &samples[1..] {
        if sample.raw_int == current_raw {
            last_seen_ms = sample.utc_ms;
        } else {
            intervals.push(StateIntervalRow {
                start_utc_ms: current_start_ms,
                end_utc_ms: sample.utc_ms,
                raw_int: current_raw,
                label: current_label,
            });
            current_start_ms = sample.utc_ms;
            last_seen_ms = sample.utc_ms;
            current_raw = sample.raw_int;
            current_label = sample
                .enum_label
                .clone()
                .unwrap_or_else(|| format!("RAW_{}", sample.raw_int));
        }
    }

    intervals.push(StateIntervalRow {
        start_utc_ms: current_start_ms,
        end_utc_ms: last_seen_ms.max(current_start_ms),
        raw_int: current_raw,
        label: current_label,
    });

    intervals
}

fn write_states_parquet(
    target_path: &path::Path,
    columns: &BTreeMap<storage::SignalId, SignalColumnAccumulator>,
) -> Result<(), ImportError> {
    let schema = Arc::new(Schema::new(vec![
        Field::new(
            "signal_id",
            DataType::Dictionary(Box::new(DataType::Int32), Box::new(DataType::Utf8)),
            false,
        ),
        Field::new("start_utc_ms", DataType::Int64, false),
        Field::new("end_utc_ms", DataType::Int64, false),
        Field::new("raw_int", DataType::Int64, false),
        Field::new("label", DataType::Utf8, false),
    ]));

    let mut signal_id_builder =
        StringDictionaryBuilder::<Int32Type>::with_capacity(256, columns.len().max(1), 512);
    let mut start_utc_ms_builder = Int64Builder::new();
    let mut end_utc_ms_builder = Int64Builder::new();
    let mut raw_int_builder = Int64Builder::new();
    let mut label_builder = StringBuilder::new();

    for (signal_id, acc) in columns {
        if !acc.is_discrete {
            continue;
        }
        for interval in build_rle_state_intervals(&acc.samples) {
            signal_id_builder.append_value(signal_id);
            start_utc_ms_builder.append_value(interval.start_utc_ms);
            end_utc_ms_builder.append_value(interval.end_utc_ms);
            raw_int_builder.append_value(interval.raw_int);
            label_builder.append_value(&interval.label);
        }
    }

    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(signal_id_builder.finish()),
            Arc::new(start_utc_ms_builder.finish()),
            Arc::new(end_utc_ms_builder.finish()),
            Arc::new(raw_int_builder.finish()),
            Arc::new(label_builder.finish()),
        ],
    )?;

    write_record_batch_atomic(
        target_path,
        schema,
        &batch,
        default_writer_properties(vec![
            SortingColumn::new(0, false, false),
            SortingColumn::new(1, false, false),
        ]),
    )
}

fn write_record_batch_atomic(
    target_path: &path::Path,
    schema: Arc<Schema>,
    batch: &RecordBatch,
    props: WriterProperties,
) -> Result<(), ImportError> {
    let tmp_path = target_path.with_extension("parquet.tmp");
    {
        let file = fs::File::create(&tmp_path)?;
        let mut writer = ArrowWriter::try_new(file, schema, Some(props))?;
        writer.write(batch)?;
        writer.close()?;
    }
    fs::rename(&tmp_path, target_path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{
        Array as _, DictionaryArray, Float64Array, Int64Array, StringArray, UInt32Array,
    };
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

    fn sample_superdbc_json() -> &'static str {
        r#"{
          "content_hash": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
          "versions": {
            "schema_version": "1.1",
            "hash": "test_v1"
          },
          "buses": {
            "VCAN": {
              "bus_id": 0,
              "baud_rate": 500000,
              "nodes": [
                { "name": "MAIN_MODULE", "is_external": false },
                { "name": "TORQUE_VECTOR", "is_external": false },
                { "name": "A_BOX", "is_external": false }
              ],
              "messages": [
                {
                  "id": 16,
                  "is_extended_id": false,
                  "message_name": "gps_time",
                  "transmitter": "TORQUE_VECTOR",
                  "receivers": [],
                  "length_bytes": 8,
                  "nominal_period_ms": 1000,
                  "priority": 4,
                  "description": "gps time in UTC",
                  "signals": [
                    { "signal_name": "year", "description": "", "data_type": "uint8_t", "raw_type": "unsigned", "start_bit": 0, "bit_length": 8, "byte_order": "little_endian", "scale": 1.0, "offset": 0.0, "limits": null, "unit": "yr", "choices": null, "display_format": null },
                    { "signal_name": "month", "description": "", "data_type": "uint8_t", "raw_type": "unsigned", "start_bit": 8, "bit_length": 8, "byte_order": "little_endian", "scale": 1.0, "offset": 0.0, "limits": null, "unit": "mo", "choices": null, "display_format": null },
                    { "signal_name": "day", "description": "", "data_type": "uint8_t", "raw_type": "unsigned", "start_bit": 16, "bit_length": 8, "byte_order": "little_endian", "scale": 1.0, "offset": 0.0, "limits": null, "unit": "d", "choices": null, "display_format": null },
                    { "signal_name": "hour", "description": "", "data_type": "uint8_t", "raw_type": "unsigned", "start_bit": 24, "bit_length": 8, "byte_order": "little_endian", "scale": 1.0, "offset": 0.0, "limits": null, "unit": "h", "choices": null, "display_format": null },
                    { "signal_name": "minute", "description": "", "data_type": "uint8_t", "raw_type": "unsigned", "start_bit": 32, "bit_length": 8, "byte_order": "little_endian", "scale": 1.0, "offset": 0.0, "limits": null, "unit": "m", "choices": null, "display_format": null },
                    { "signal_name": "second", "description": "", "data_type": "uint8_t", "raw_type": "unsigned", "start_bit": 40, "bit_length": 8, "byte_order": "little_endian", "scale": 1.0, "offset": 0.0, "limits": null, "unit": "s", "choices": null, "display_format": null },
                    { "signal_name": "millisecond", "description": "", "data_type": "uint16_t", "raw_type": "unsigned", "start_bit": 48, "bit_length": 16, "byte_order": "little_endian", "scale": 1.0, "offset": 0.0, "limits": null, "unit": "ms", "choices": null, "display_format": null }
                  ]
                },
                {
                  "id": 32,
                  "is_extended_id": false,
                  "message_name": "gps_speed_heading",
                  "transmitter": "TORQUE_VECTOR",
                  "receivers": [],
                  "length_bytes": 8,
                  "nominal_period_ms": 40,
                  "priority": 3,
                  "description": "gps 2D ground speed and heading",
                  "signals": [
                    { "signal_name": "gps_speed", "description": "", "data_type": "int32_t", "raw_type": "signed", "start_bit": 0, "bit_length": 32, "byte_order": "little_endian", "scale": 0.001, "offset": 0.0, "limits": null, "unit": "m/s", "choices": null, "display_format": null },
                    { "signal_name": "gps_heading", "description": "", "data_type": "int32_t", "raw_type": "signed", "start_bit": 32, "bit_length": 32, "byte_order": "little_endian", "scale": 0.00001, "offset": 0.0, "limits": null, "unit": "deg", "choices": null, "display_format": null }
                  ]
                },
                {
                  "id": 48,
                  "is_extended_id": false,
                  "message_name": "main_hb",
                  "transmitter": "MAIN_MODULE",
                  "receivers": [],
                  "length_bytes": 1,
                  "nominal_period_ms": 500,
                  "priority": 2,
                  "description": "Main heartbeat",
                  "signals": [
                    {
                      "signal_name": "car_state",
                      "description": "",
                      "data_type": "car_state_t",
                      "raw_type": "unsigned",
                      "start_bit": 0,
                      "bit_length": 8,
                      "byte_order": "little_endian",
                      "scale": 1.0,
                      "offset": 0.0,
                      "limits": null,
                      "unit": "",
                      "choices": { "0": "fatal", "1": "idle", "5": "ready2drive" },
                      "display_format": null
                    }
                  ]
                },
                {
                  "id": 64,
                  "is_extended_id": false,
                  "message_name": "main_module_fault_sync",
                  "transmitter": "MAIN_MODULE",
                  "receivers": [],
                  "length_bytes": 1,
                  "nominal_period_ms": 100,
                  "priority": 1,
                  "description": "Fault sync",
                  "signals": [
                    {
                      "signal_name": "SDC1_IMD",
                      "description": "",
                      "data_type": "bool",
                      "raw_type": "unsigned",
                      "start_bit": 0,
                      "bit_length": 1,
                      "byte_order": "little_endian",
                      "scale": 1.0,
                      "offset": 0.0,
                      "limits": null,
                      "unit": "",
                      "choices": { "0": "OFF", "1": "ON" },
                      "display_format": null
                    }
                  ]
                }
              ]
            }
          }
        }"#
    }

    fn pad_to_8(payload: &[u8]) -> [u8; 8] {
        let mut out = [0u8; 8];
        out[..payload.len()].copy_from_slice(payload);
        out
    }

    #[test]
    fn test_end_to_end_parquet_pyramid_and_idempotency() {
        let db = superdbc::database::Database::from_json(sample_superdbc_json())
            .expect("valid test superdbc");

        let temp_dir = std::env::temp_dir().join(format!("daq_m1_test_{}", uuid::Uuid::new_v4()));
        let logs_dir = temp_dir.join("logs");
        let store_dir = temp_dir.join("store");
        fs::create_dir_all(&logs_dir).unwrap();

        // Build raw frames across 8 seconds with GPS lock, driving stint (> 5s at > 1 m/s),
        // discrete state transition on main_hb.car_state (idle -> ready2drive),
        // and fault transition on main_module_fault_sync.SDC1_IMD (0 -> 1 -> 0).
        let mut raw_frames = Vec::new();

        // Two GPS points at ticks 1_000 ms and 5_000 ms anchored to 2026-11-12T14:00:01Z and 14:00:05Z
        for (ticks_ms, sec) in [(1_000u32, 1f64), (5_000u32, 5f64)] {
            let payload = db
                .encode(
                    0,
                    16,
                    &[
                        ("year", 26.0),
                        ("month", 11.0),
                        ("day", 12.0),
                        ("hour", 14.0),
                        ("minute", 0.0),
                        ("second", sec),
                        ("millisecond", 0.0),
                    ],
                )
                .unwrap();
            raw_frames.push(parse::RawFrame::new(ticks_ms, 16, 0, pad_to_8(&payload)).unwrap());
        }

        // Speed samples every 100 ms from t=1000..=7000 ms (6.0 seconds at 10.0 m/s -> triggers driving_stint)
        for step in 0..=60 {
            let ticks_ms = 1_000 + step * 100;
            let payload = db
                .encode(
                    0,
                    32,
                    &[
                        ("gps_speed", 10.0 + (step % 2) as f64),
                        ("gps_heading", 90.0),
                    ],
                )
                .unwrap();
            raw_frames.push(parse::RawFrame::new(ticks_ms, 32, 0, pad_to_8(&payload)).unwrap());
        }

        // State transitions on main_hb.car_state: 1 ("idle") at 1000ms, 5 ("ready2drive") at 2000ms & 3000ms
        for (ticks_ms, state_val) in [(1_000u32, 1.0), (2_000u32, 5.0), (3_000u32, 5.0)] {
            let payload = db.encode(0, 48, &[("car_state", state_val)]).unwrap();
            raw_frames.push(parse::RawFrame::new(ticks_ms, 48, 0, pad_to_8(&payload)).unwrap());
        }

        // Fault transitions on main_module_fault_sync.SDC1_IMD: 0 at 1000ms, 1 at 4000ms, 0 at 6000ms
        for (ticks_ms, fault_val) in [(1_000u32, 0.0), (4_000u32, 1.0), (6_000u32, 0.0)] {
            let payload = db.encode(0, 64, &[("SDC1_IMD", fault_val)]).unwrap();
            raw_frames.push(parse::RawFrame::new(ticks_ms, 64, 0, pad_to_8(&payload)).unwrap());
        }

        raw_frames.sort_by_key(|f| f.ticks_ms);
        let log_path = logs_dir.join("log-2026-11-12--14-00-00.log");
        fs::write(&log_path, bytemuck::cast_slice(&raw_frames)).unwrap();

        // 1. Run first import
        let summary1 =
            crate::log_parse::import_logs_to_store(&logs_dir, &store_dir, &db).expect("import 1");
        assert_eq!(summary1.log_files_parsed, 1);
        assert_eq!(summary1.boot_epochs_detected, 1);
        assert_eq!(summary1.partitions_written, 1);
        assert_eq!(summary1.partitions_skipped_existing, 0);
        assert_eq!(summary1.manifests.len(), 1);

        let manifest = &summary1.manifests[0];
        assert!(matches!(
            manifest.clock_mapping.confidence,
            correlate::ClockConfidence::GpsSynced { point_count: 2, .. }
        ));
        assert_eq!(manifest.track_day_id, "2026-11-12");

        // Check auto-annotations (1 driving_stint + 1 fault)
        assert!(
            manifest
                .annotations
                .iter()
                .any(|a| a.category == "driving_stint")
        );
        assert!(
            manifest.annotations.iter().any(
                |a| a.category == "fault" && a.name.contains("main_module_fault_sync.SDC1_IMD")
            )
        );

        let part_dir = store_dir.join("partitions").join(&manifest.partition_id);
        assert!(part_dir.join("full_res.parquet").exists());
        assert!(part_dir.join("overview_10hz.parquet").exists());
        assert!(part_dir.join("overview_1hz.parquet").exists());
        assert!(part_dir.join("states.parquet").exists());
        assert!(part_dir.join("manifest.json").exists());

        // 2. Read back full_res.parquet and verify (signal_id, utc_ms) sort order & canonical msg.sig IDs
        let full_res_file = fs::File::open(part_dir.join("full_res.parquet")).unwrap();
        let reader = ParquetRecordBatchReaderBuilder::try_new(full_res_file)
            .unwrap()
            .build()
            .unwrap();
        let mut prev_pair: Option<(String, i64)> = None;
        let mut total_full_res_rows = 0usize;
        for batch_res in reader {
            let batch = batch_res.unwrap();
            let dict_col = batch
                .column(0)
                .as_any()
                .downcast_ref::<DictionaryArray<Int32Type>>()
                .unwrap();
            let values_col = dict_col
                .values()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let utc_col = batch
                .column(1)
                .as_any()
                .downcast_ref::<Int64Array>()
                .unwrap();
            let _ticks_col = batch
                .column(2)
                .as_any()
                .downcast_ref::<UInt32Array>()
                .unwrap();
            let _val_col = batch
                .column(3)
                .as_any()
                .downcast_ref::<Float64Array>()
                .unwrap();

            for i in 0..batch.num_rows() {
                let key_idx = dict_col.keys().value(i) as usize;
                let sig_id = values_col.value(key_idx).to_owned();
                let utc_ms = utc_col.value(i);
                assert!(
                    sig_id.contains('.'),
                    "SignalId should be msg_name.sig_name: {sig_id}"
                );
                assert!(
                    !sig_id.starts_with("VCAN."),
                    "SignalId must omit bus.node prefix: {sig_id}"
                );
                if let Some((ref prev_sig, prev_ts)) = prev_pair {
                    assert!(
                        (prev_sig.as_str(), prev_ts) <= (sig_id.as_str(), utc_ms),
                        "full_res.parquet must be sorted by (signal_id, utc_ms)"
                    );
                }
                prev_pair = Some((sig_id, utc_ms));
                total_full_res_rows += 1;
            }
        }
        assert!(total_full_res_rows > 100);

        // 3. Read back states.parquet and verify RLE intervals on main_hb.car_state
        let states_file = fs::File::open(part_dir.join("states.parquet")).unwrap();
        let states_reader = ParquetRecordBatchReaderBuilder::try_new(states_file)
            .unwrap()
            .build()
            .unwrap();
        let mut car_state_labels = Vec::new();
        for batch_res in states_reader {
            let batch = batch_res.unwrap();
            let dict_col = batch
                .column(0)
                .as_any()
                .downcast_ref::<DictionaryArray<Int32Type>>()
                .unwrap();
            let dict_vals = dict_col
                .values()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let label_col = batch
                .column(4)
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            for i in 0..batch.num_rows() {
                let sig_id = dict_vals.value(dict_col.keys().value(i) as usize);
                if sig_id == "main_hb.car_state" {
                    car_state_labels.push(label_col.value(i).to_owned());
                }
            }
        }
        // Two RLE intervals: "idle" (at 1000ms) followed by "ready2drive" (at 2000..3000ms)
        assert_eq!(car_state_labels, vec!["idle", "ready2drive"]);

        // 4. Re-run import and verify idempotency (skips existing partition)
        let summary2 =
            crate::log_parse::import_logs_to_store(&logs_dir, &store_dir, &db).expect("import 2");
        assert_eq!(summary2.partitions_written, 0);
        assert_eq!(summary2.partitions_skipped_existing, 1);

        // 5. Verify local_registry.sqlite was automatically populated and matches the manifest
        let registry_path = store_dir.join(storage::LOCAL_REGISTRY_FILENAME);
        assert!(registry_path.is_file());
        let registry = storage::Registry::open_store(&store_dir).expect("open store registry");
        let days = registry.list_track_days().unwrap();
        assert_eq!(days.len(), 1);
        assert_eq!(days[0].id, manifest.track_day_id);

        let registered_parts = registry
            .list_partitions_for_day(&manifest.track_day_id)
            .unwrap();
        assert_eq!(registered_parts.len(), 1);
        assert_eq!(registered_parts[0].partition_id, manifest.partition_id);
        assert_eq!(registered_parts[0].manifest, *manifest);

        let registered_annotations = registry
            .list_annotations_for_day(&manifest.track_day_id, false)
            .unwrap();
        assert_eq!(registered_annotations.len(), manifest.annotations.len());

        // 6. If local_registry.sqlite is deleted, sync_from_store_dir reconstructs it from manifest.json files
        drop(registry);
        fs::remove_file(&registry_path).unwrap();
        let rebuilt_registry = storage::Registry::open_store(&store_dir).unwrap();
        let synced_count = rebuilt_registry.sync_from_store_dir(&store_dir).unwrap();
        assert_eq!(synced_count, 1);
        assert_eq!(
            rebuilt_registry
                .list_partitions_for_day(&manifest.track_day_id)
                .unwrap()
                .len(),
            1
        );

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_rtc_and_mtime_fallback_tiers() {
        let db = superdbc::database::Database::from_json(sample_superdbc_json())
            .expect("valid test superdbc");

        let payload = db.encode(0, 48, &[("car_state", 1.0)]).unwrap();
        let decoded = db.decode(0, 48, &payload).unwrap();
        let msg = parse::ParsedMessage {
            timestamp: 500,
            decoded,
            bus_name: "VCAN".to_owned(),
            source_file_idx: 0,
        };

        // Tier 2: RtcEstimated when rtc_timestamp_ms is present
        let rtc_files = vec![parse::SourceLogFile {
            file_name: "log-2026-11-12--14-05-30.log".to_owned(),
            sha256_hex: "abc".to_owned(),
            rtc_timestamp_ms: Some(1_794_492_330_000),
            mtime_unix_ms: 1_700_000_000_000,
        }];
        let res_rtc = correlate::time_correlate_chunk(vec![msg.clone()], &rtc_files);
        assert_eq!(
            res_rtc.clock_mapping.confidence,
            correlate::ClockConfidence::RtcEstimated
        );
        assert_eq!(
            res_rtc.clock_mapping.ticks_to_utc_ms(500),
            1_794_492_330_000
        );

        // Tier 3: FileMtimeEstimated when neither GPS nor RTC filename is present
        let mtime_files = vec![parse::SourceLogFile {
            file_name: "log-0.log".to_owned(),
            sha256_hex: "def".to_owned(),
            rtc_timestamp_ms: None,
            mtime_unix_ms: 1_794_500_000_000,
        }];
        let res_mtime = correlate::time_correlate_chunk(vec![msg], &mtime_files);
        assert_eq!(
            res_mtime.clock_mapping.confidence,
            correlate::ClockConfidence::FileMtimeEstimated
        );
        assert_eq!(
            res_mtime.clock_mapping.ticks_to_utc_ms(500),
            1_794_500_000_000
        );
    }

    #[test]
    fn test_subpartition_splitting_and_epoch_stitching() {
        let db = superdbc::database::Database::from_json(sample_superdbc_json())
            .expect("valid test superdbc");

        let temp_dir =
            std::env::temp_dir().join(format!("daq_m1_split_test_{}", uuid::Uuid::new_v4()));
        let logs_dir = temp_dir.join("logs");
        let store_dir = temp_dir.join("store");
        fs::create_dir_all(&logs_dir).unwrap();

        let payload = db.encode(0, 48, &[("car_state", 5.0)]).unwrap();
        let padded = pad_to_8(&payload);

        // File 1 & File 2: contiguous boot epoch spanning 20 minutes (0..=1_200_000 ms in 60s steps)
        // Should stitch into 1 BootEpoch and then slice into 2 sub-partitions (0..900_000 and 900_000..=1_200_000)
        let mut file1_frames = Vec::new();
        for minute in 0..10 {
            let ticks_ms = minute * 60_000;
            file1_frames.push(parse::RawFrame::new(ticks_ms, 48, 0, padded).unwrap());
        }
        fs::write(
            logs_dir.join("log-2026-11-12--14-00-00.log"),
            bytemuck::cast_slice(&file1_frames),
        )
        .unwrap();

        let mut file2_frames = Vec::new();
        for minute in 10..=20 {
            let ticks_ms = minute * 60_000;
            file2_frames.push(parse::RawFrame::new(ticks_ms, 48, 0, padded).unwrap());
        }
        fs::write(
            logs_dir.join("log-2026-11-12--14-10-00.log"),
            bytemuck::cast_slice(&file2_frames),
        )
        .unwrap();

        // File 3: MCU power cycle (ticks_ms resets to 100 ms) -> 2nd BootEpoch
        let file3_frames = vec![parse::RawFrame::new(100, 48, 0, padded).unwrap()];
        fs::write(
            logs_dir.join("log-2026-11-12--15-00-00.log"),
            bytemuck::cast_slice(&file3_frames),
        )
        .unwrap();

        let summary = crate::log_parse::import_logs_to_store(&logs_dir, &store_dir, &db).unwrap();
        assert_eq!(summary.log_files_parsed, 3);
        assert_eq!(summary.boot_epochs_detected, 2);
        // Epoch 1 splits into 2 sub-partitions (<= 15m each) + Epoch 2 has 1 partition = 3 total
        assert_eq!(summary.partitions_written, 3);
        assert_eq!(summary.manifests.len(), 3);

        for manifest in &summary.manifests {
            assert!(
                manifest.end_utc_ms - manifest.start_utc_ms <= consts::MAX_PARTITION_SPAN_MS,
                "Partition span {} exceeds 15 minutes",
                manifest.end_utc_ms - manifest.start_utc_ms
            );
        }

        // Simulate pulling the same SD card later in the day after another boot appended to log-0.log
        // and wrote a new RTC-named file: the earlier partitions must be skipped (idempotent)!
        let mut log0_frames = vec![
            parse::RawFrame::new(10, 48, 0, padded).unwrap(),
            parse::RawFrame::new(20, 48, 0, padded).unwrap(),
        ];
        fs::write(
            logs_dir.join("log-0.log"),
            bytemuck::cast_slice(&log0_frames),
        )
        .unwrap();
        let summary_with_log0_a =
            crate::log_parse::import_logs_to_store(&logs_dir, &store_dir, &db).unwrap();

        // Append a second boot segment into log-0.log + a new RTC file
        log0_frames.push(parse::RawFrame::new(5, 48, 0, padded).unwrap());
        fs::write(
            logs_dir.join("log-0.log"),
            bytemuck::cast_slice(&log0_frames),
        )
        .unwrap();
        let file4_frames = vec![parse::RawFrame::new(500, 48, 0, padded).unwrap()];
        fs::write(
            logs_dir.join("log-2026-11-12--16-00-00.log"),
            bytemuck::cast_slice(&file4_frames),
        )
        .unwrap();

        let summary_with_log0_b =
            crate::log_parse::import_logs_to_store(&logs_dir, &store_dir, &db).unwrap();
        assert_eq!(
            summary_with_log0_b.partitions_skipped_existing,
            summary_with_log0_a.manifests.len(),
            "All earlier partitions must be skipped even after log-0.log grows with new boots"
        );
        assert_eq!(summary_with_log0_b.partitions_written, 1);

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
