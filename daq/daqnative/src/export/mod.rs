use crate::router::VirtualTimelineRouter;
use arrow::{
    array::{ArrayRef, Float64Array, Int64Array, RecordBatch},
    datatypes::{DataType, Field, Schema},
};
use parquet::{
    arrow::{ArrowWriter, arrow_reader::ParquetRecordBatchReaderBuilder},
    basic::{Compression, ZstdLevel},
    file::properties::WriterProperties,
};
use std::{
    collections::HashMap,
    fs,
    ops::RangeInclusive,
    path::{Path, PathBuf},
    sync::Arc,
};

pub use daqcore::export::{
    BUILTIN_EXPORT_PRESETS_JSON, DEFAULT_UNIFORM_EXPORT_RATE_HZ, DatasetExportRequest,
    ExportColumn, ExportError, ExportFormat, ExportPresetDef, ExportPresetRegistry,
    ExportPresetsConfigFile, ExportSignalMeta, ExportSummary, ExportTable, SamplingMode,
    discover_available_signals, expand_signal_patterns, materialize_export_table,
    read_matlab_mat_vars, read_matlab_mat_vars_bytes, resolve_annotation_time_range,
    sanitize_matlab_var_name, write_csv, write_csv_bytes, write_matlab_mat, write_matlab_mat_bytes,
};

/// Materialize and write a dataset to `output_path` in `format`.
pub fn export_dataset(
    router: &VirtualTimelineRouter,
    range: RangeInclusive<i64>,
    signal_patterns: &[String],
    sampling_mode: SamplingMode,
    format: ExportFormat,
    output_path: impl AsRef<Path>,
) -> Result<ExportSummary, ExportError> {
    let table = materialize_export_table(router, range, signal_patterns, sampling_mode)?;
    let out_path = output_path.as_ref().to_path_buf();
    if let Some(parent) = out_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }

    match format {
        ExportFormat::Parquet => write_parquet(&table, &out_path)?,
        ExportFormat::Csv => write_csv(&table, &out_path)?,
        ExportFormat::MatlabMat => write_matlab_mat(&table, &out_path)?,
    }

    let start_utc_ms = table.timestamps_utc_ms.first().copied().unwrap_or(0);
    let end_utc_ms = table.timestamps_utc_ms.last().copied().unwrap_or(0);
    let signals = table
        .columns
        .iter()
        .map(|c| (c.signal_id.clone(), c.unit.clone()))
        .collect();

    Ok(ExportSummary {
        output_path: out_path,
        format,
        start_utc_ms,
        end_utc_ms,
        row_count: table.row_count(),
        signal_count: table.column_count(),
        signals,
    })
}

/// Materialize a dataset in memory and return its serialized bytes in `format` (`Parquet`, `Csv`, or `MatlabMat`).
pub fn export_dataset_bytes(
    router: &VirtualTimelineRouter,
    range: RangeInclusive<i64>,
    signal_patterns: &[String],
    sampling_mode: SamplingMode,
    format: ExportFormat,
) -> Result<(Vec<u8>, ExportSummary), ExportError> {
    let table = materialize_export_table(router, range, signal_patterns, sampling_mode)?;
    let bytes = match format {
        ExportFormat::Parquet => write_parquet_bytes(&table)?,
        ExportFormat::Csv => write_csv_bytes(&table)?,
        ExportFormat::MatlabMat => write_matlab_mat_bytes(&table)?,
    };

    let start_utc_ms = table.timestamps_utc_ms.first().copied().unwrap_or(0);
    let end_utc_ms = table.timestamps_utc_ms.last().copied().unwrap_or(0);
    let signals = table
        .columns
        .iter()
        .map(|c| (c.signal_id.clone(), c.unit.clone()))
        .collect();

    let summary = ExportSummary {
        output_path: PathBuf::new(),
        format,
        start_utc_ms,
        end_utc_ms,
        row_count: table.row_count(),
        signal_count: table.column_count(),
        signals,
    };
    Ok((bytes, summary))
}

fn build_parquet_record_batch(table: &ExportTable) -> Result<(Arc<Schema>, RecordBatch), ExportError> {
    let mut fields = Vec::with_capacity(table.columns.len() + 2);

    let mut utc_meta = HashMap::new();
    utc_meta.insert("unit".to_owned(), "ms".to_owned());
    utc_meta.insert("label".to_owned(), "UTC Timestamp (ms)".to_owned());
    fields.push(Field::new("utc_ms", DataType::Int64, false).with_metadata(utc_meta));

    let mut elapsed_meta = HashMap::new();
    elapsed_meta.insert("unit".to_owned(), "s".to_owned());
    elapsed_meta.insert("label".to_owned(), "Elapsed Time (s)".to_owned());
    fields.push(Field::new("elapsed_s", DataType::Float64, false).with_metadata(elapsed_meta));

    for col in &table.columns {
        let mut meta = HashMap::new();
        meta.insert("unit".to_owned(), col.unit.clone());
        meta.insert("label".to_owned(), col.label.clone());
        fields.push(Field::new(&col.signal_id, DataType::Float64, true).with_metadata(meta));
    }

    let schema = Arc::new(Schema::new(fields));
    let mut arrays: Vec<ArrayRef> = Vec::with_capacity(table.columns.len() + 2);
    arrays.push(Arc::new(Int64Array::from(table.timestamps_utc_ms.clone())));
    arrays.push(Arc::new(Float64Array::from(table.elapsed_s.clone())));

    for col in &table.columns {
        let opt_vals: Vec<Option<f64>> = col
            .values
            .iter()
            .map(|&v| if v.is_finite() { Some(v) } else { None })
            .collect();
        arrays.push(Arc::new(Float64Array::from(opt_vals)));
    }

    let batch = RecordBatch::try_new(schema.clone(), arrays)
        .map_err(|e| ExportError::Format(format!("Arrow error: {e}")))?;
    Ok((schema, batch))
}

/// Write an [`ExportTable`] to a ZSTD-compressed Apache Parquet file, embedding `"unit"` and
/// `"label"` metadata on each Arrow [`Field`].
pub fn write_parquet(table: &ExportTable, path: &Path) -> Result<(), ExportError> {
    let (schema, batch) = build_parquet_record_batch(table)?;
    let file = fs::File::create(path)?;
    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::default()))
        .build();
    let mut writer = ArrowWriter::try_new(file, schema, Some(props))
        .map_err(|e| ExportError::Format(format!("Parquet error: {e}")))?;
    writer
        .write(&batch)
        .map_err(|e| ExportError::Format(format!("Parquet error: {e}")))?;
    writer
        .close()
        .map_err(|e| ExportError::Format(format!("Parquet error: {e}")))?;
    Ok(())
}

/// Write an [`ExportTable`] to an in-memory ZSTD-compressed Apache Parquet byte buffer.
pub fn write_parquet_bytes(table: &ExportTable) -> Result<Vec<u8>, ExportError> {
    let (schema, batch) = build_parquet_record_batch(table)?;
    let mut buf = Vec::new();
    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::default()))
        .build();
    let mut writer = ArrowWriter::try_new(&mut buf, schema, Some(props))
        .map_err(|e| ExportError::Format(format!("Parquet error: {e}")))?;
    writer
        .write(&batch)
        .map_err(|e| ExportError::Format(format!("Parquet error: {e}")))?;
    writer
        .close()
        .map_err(|e| ExportError::Format(format!("Parquet error: {e}")))?;
    Ok(buf)
}

/// Read back an exported Parquet file's column headers, units (from Arrow field metadata),
/// and row count for verification.
pub fn read_exported_parquet_info(
    path: &Path,
) -> Result<(usize, Vec<(String, String)>), ExportError> {
    let file = fs::File::open(path)?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| ExportError::Format(format!("Parquet error: {e}")))?;
    let schema = builder.schema().clone();
    let mut columns = Vec::with_capacity(schema.fields().len());
    for field in schema.fields() {
        let unit = field.metadata().get("unit").cloned().unwrap_or_default();
        columns.push((field.name().clone(), unit));
    }
    let reader = builder
        .build()
        .map_err(|e| ExportError::Format(format!("Parquet error: {e}")))?;
    let mut total_rows = 0usize;
    for batch in reader {
        let b = batch.map_err(|e| ExportError::Format(format!("Arrow error: {e}")))?;
        total_rows += b.num_rows();
    }
    Ok((total_rows, columns))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        frame::{FrameKind, ParsedFrame},
        storage::{live_router, open_store_router},
        superdbc::database::Database,
        time::Time,
    };

    #[test]
    fn export_parquet_csv_and_mat_round_trip_with_per26_data() {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let dbc_path = manifest_dir.join("../../superdbc/superdbc_per26.json");
        let log_path =
            manifest_dir.join("../../PER26/2026-09-06/2_regen/raw/log-2026-09-06--20-21-44.log");
        if !dbc_path.is_file() || !log_path.is_file() {
            return;
        }

        let db = Database::load(&dbc_path).expect("load PER26 database");
        let tmp_raw = tempfile::tempdir().expect("temp raw dir");
        fs::copy(
            &log_path,
            tmp_raw.path().join("log-2026-09-06--20-21-44.log"),
        )
        .expect("copy PER26 log");

        let tmp_store = tempfile::tempdir().expect("temp store dir");
        let summary = crate::log_parse::import_logs_to_store(tmp_raw.path(), tmp_store.path(), &db)
            .expect("import PER26 log");
        assert!(!summary.manifests.is_empty());

        let m0 = &summary.manifests[0];
        let mut router = open_store_router(
            tmp_store.path(),
            Time::from_unix_millis(m0.end_utc_ms),
            30.0,
        )
        .expect("open store");

        // Export a 10-second window using the "vd" preset to Parquet, CSV, and MATLAB .mat
        let window_start = m0.start_utc_ms;
        let window_end = (m0.start_utc_ms + 10_000).min(m0.end_utc_ms);
        let range = window_start..=window_end;

        let presets = ExportPresetRegistry::with_builtin();
        let vd_preset = presets.get("vd").expect("vd preset");

        let tmp_out = tempfile::tempdir().expect("temp out dir");
        let pq_path = tmp_out.path().join("vd_10s.parquet");
        let csv_path = tmp_out.path().join("vd_10s.csv");
        let mat_path = tmp_out.path().join("vd_10s.mat");

        let pq_summary = export_dataset(
            &router,
            range.clone(),
            &vd_preset.signals,
            vd_preset.default_sampling,
            ExportFormat::Parquet,
            &pq_path,
        )
        .expect("export vd parquet");
        assert_eq!(pq_summary.row_count, 1001); // 10.0s at 100 Hz inclusive
        assert!(pq_summary.signal_count >= 10);

        let (pq_rows, pq_cols) =
            read_exported_parquet_info(&pq_path).expect("read exported parquet info");
        assert_eq!(pq_rows, 1001);
        assert_eq!(pq_cols[0], ("utc_ms".to_owned(), "ms".to_owned()));
        assert_eq!(pq_cols[1], ("elapsed_s".to_owned(), "s".to_owned()));
        assert!(
            pq_cols
                .iter()
                .any(|(name, unit)| name == "IMU_acceleration.X_axis" && unit == "g")
        );
        assert!(
            pq_cols
                .iter()
                .any(|(name, unit)| name == "battery.power_w" && unit == "W")
        );

        let csv_summary = export_dataset(
            &router,
            range.clone(),
            &vd_preset.signals,
            vd_preset.default_sampling,
            ExportFormat::Csv,
            &csv_path,
        )
        .expect("export vd csv");
        assert_eq!(csv_summary.row_count, 1001);
        let csv_text = fs::read_to_string(&csv_path).expect("read csv");
        let mut lines = csv_text.lines();
        let header = lines.next().expect("csv header");
        assert!(header.starts_with("utc_ms,elapsed_s,"));
        assert!(header.contains("IMU_acceleration.X_axis"));
        assert!(header.contains("battery.power_w"));
        assert_eq!(lines.count(), 1001);

        let mat_summary = export_dataset(
            &router,
            range,
            &vd_preset.signals,
            vd_preset.default_sampling,
            ExportFormat::MatlabMat,
            &mat_path,
        )
        .expect("export vd mat");
        assert_eq!(mat_summary.row_count, 1001);
        let mat_vars = read_matlab_mat_vars(&mat_path).expect("read mat vars");
        assert_eq!(mat_vars.get("utc_ms").map(Vec::len), Some(1001));
        assert_eq!(mat_vars.get("elapsed_s").map(Vec::len), Some(1001));
        assert_eq!(
            mat_vars.get("IMU_acceleration_X_axis").map(Vec::len),
            Some(1001)
        );
        assert_eq!(mat_vars.get("battery_power_w").map(Vec::len), Some(1001));

        // Verify annotation-based lookup on both auto-generated annotations and "Driving Stint 1"
        if let Some(first_auto) = m0.annotations.first() {
            let (auto_name, auto_range) =
                resolve_annotation_time_range(&router, &first_auto.name).expect("resolve auto ann");
            assert_eq!(auto_name, first_auto.name);
            assert!(!auto_range.is_empty());
        }

        let track_day_id = router
            .timeline()
            .active_track_day_id()
            .unwrap_or("2026-09-06")
            .to_owned();
        router
            .upsert_annotation(&crate::storage::Annotation {
                id: "stint_1".to_owned(),
                track_day_id,
                name: "Driving Stint 1".to_owned(),
                category: "driving_stint".to_owned(),
                color: "#2ECC71".to_owned(),
                start_utc_ms: window_start,
                end_utc_ms: Some(window_end),
                author: "engineer".to_owned(),
                source: "manual".to_owned(),
                notes: String::new(),
                updated_at_ms: window_end,
                deleted: false,
            })
            .expect("upsert Driving Stint 1");
        let (ann_name, ann_range) =
            resolve_annotation_time_range(&router, "Driving Stint 1").expect("resolve stint 1");
        assert_eq!(ann_name, "Driving Stint 1");
        assert_eq!(ann_range, window_start..=window_end);
    }

    #[test]
    fn sparse_raw_export_preserves_exact_timestamps_on_live_data() {
        let t0 = Time::from_unix_millis(1_794_500_000_000);
        let mut router = live_router(t0, 30.0).expect("live router");
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let dbc_path = manifest_dir.join("../../superdbc/superdbc_per26.json");
        if !dbc_path.is_file() {
            return;
        }
        let db = Database::load(&dbc_path).expect("load PER26 db");
        let msg = db
            .messages()
            .find(|m| m.name() == "pack_stats")
            .expect("pack_stats message");
        let key = msg.key();

        for i in 0..5 {
            let raw = vec![0x10, 0x27, 0x30, 0x00, 0x50, 0x00, 0x00, 0x00];
            let decoded = db.decode(key.bus_id, key.identity, &raw).ok();
            router.ingest_frame(ParsedFrame {
                timestamp: t0.offset(i * 20),
                bus_id: key.bus_id,
                identity: key.identity,
                kind: FrameKind::Data,
                dlc: 8,
                raw_bytes: raw,
                decoded,
            });
        }

        let table = materialize_export_table(
            &router,
            t0.unix_millis()..=(t0.unix_millis() + 100),
            &["pack_stats.*".to_owned(), "battery.power_w".to_owned()],
            SamplingMode::SparseRaw,
        )
        .expect("materialize sparse table");
        assert_eq!(table.row_count(), 5);
        assert!(table.column("pack_stats.vbatt_adc").is_some());
        assert!(table.column("battery.power_w").is_some());
    }
}
