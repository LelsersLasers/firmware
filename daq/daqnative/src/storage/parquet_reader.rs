use crate::{
    router::{
        EnvelopePoint, PartitionPreloadPayload, Resolution, RouterError, SignalSeries,
        StateInterval,
        wire::{encode_preload_payload, encode_tile_payload},
    },
    storage::{PartitionManifest, SignalId, SignalManifestEntry},
};
use arrow::{
    array::{Array as _, DictionaryArray, Float64Array, Int64Array, StringArray, UInt32Array},
    datatypes::Int32Type,
};
use parquet::{
    arrow::{ProjectionMask, arrow_reader::ParquetRecordBatchReaderBuilder},
    file::statistics::Statistics,
};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::Path,
    sync::Arc,
};

fn parquet_err(e: parquet::errors::ParquetError) -> RouterError {
    RouterError::Format(format!("Parquet error: {e}"))
}

fn arrow_err(e: arrow::error::ArrowError) -> RouterError {
    RouterError::Format(format!("Arrow error: {e}"))
}

/// Find the dictionary key for `target_signal_id` inside a `DictionaryArray<Int32Type>`.
fn find_dictionary_key(
    dict_col: &DictionaryArray<Int32Type>,
    target_signal_id: &str,
) -> Result<Option<i32>, RouterError> {
    let values = dict_col
        .values()
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or(RouterError::InvalidColumn(
            "signal_id dictionary values must be Utf8",
        ))?;

    for idx in 0..values.len() {
        if !values.is_null(idx) && values.value(idx) == target_signal_id {
            return Ok(Some(idx as i32));
        }
    }
    Ok(None)
}

/// Filter Parquet row groups using column 0 (`signal_id`) min/max statistics so that row groups
/// whose `[min, max]` string interval cannot contain `target_signal_id` are skipped without I/O.
fn matching_row_groups_for_signal(
    builder: &ParquetRecordBatchReaderBuilder<fs::File>,
    target_signal_id: &str,
) -> Vec<usize> {
    let target_bytes = target_signal_id.as_bytes();
    let rg_meta = builder.metadata().row_groups();
    let mut matching = Vec::with_capacity(rg_meta.len());
    for (idx, rg) in rg_meta.iter().enumerate() {
        if rg.columns().is_empty() {
            matching.push(idx);
            continue;
        }
        if let Some(Statistics::ByteArray(stats)) = rg.column(0).statistics()
            && let (Some(min_ba), Some(max_ba)) = (stats.min_opt(), stats.max_opt())
            && (target_bytes < min_ba.data() || target_bytes > max_ba.data())
        {
            continue;
        }
        matching.push(idx);
    }
    matching
}

/// Return `true` when the last non-null `signal_id` in `dict_col` is lexicographically strictly
/// greater than `target_signal_id` (indicating a sorted Parquet file has passed `target_signal_id`).
fn batch_passed_sorted_signal(
    dict_col: &DictionaryArray<Int32Type>,
    target_signal_id: &str,
) -> bool {
    let Some(values) = dict_col.values().as_any().downcast_ref::<StringArray>() else {
        return false;
    };
    let keys = dict_col.keys().values();
    let dict_len = values.len();
    for row in (0..dict_col.len()).rev() {
        if dict_col.is_null(row) {
            continue;
        }
        if let Ok(key_idx) = usize::try_from(keys[row])
            && key_idx < dict_len
        {
            return values.value(key_idx) > target_signal_id;
        }
        break;
    }
    false
}

pub type OverviewWithActivity = (HashMap<SignalId, Arc<SignalSeries>>, Vec<(i64, u32)>);

/// Read all signals from an overview Parquet file (`overview_1hz.parquet` or `overview_10hz.parquet`).
pub fn read_all_signals_overview(
    path: &Path,
) -> Result<HashMap<SignalId, Arc<SignalSeries>>, RouterError> {
    read_all_signals_overview_with_activity(path).map(|(signals, _)| signals)
}

/// Read all signals plus aggregated per-bucket sample counts from an overview Parquet file.
pub fn read_all_signals_overview_with_activity(
    path: &Path,
) -> Result<OverviewWithActivity, RouterError> {
    let file = fs::File::open(path)?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file).map_err(parquet_err)?;
    let mask = ProjectionMask::roots(builder.parquet_schema(), [0, 1, 2, 3, 4, 5]);
    let reader = builder.with_projection(mask).build().map_err(parquet_err)?;

    let mut map: HashMap<SignalId, Vec<EnvelopePoint>> = HashMap::new();
    let mut activity: BTreeMap<i64, u32> = BTreeMap::new();

    for batch_result in reader {
        let batch = batch_result.map_err(arrow_err)?;
        let sig_col = batch
            .column(0)
            .as_any()
            .downcast_ref::<DictionaryArray<Int32Type>>()
            .ok_or(RouterError::InvalidColumn(
                "overview col 0 must be Dictionary<Int32, Utf8>",
            ))?;
        let dict_values = sig_col
            .values()
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or(RouterError::InvalidColumn(
                "overview dictionary values must be Utf8",
            ))?;
        let bucket_col = batch
            .column(1)
            .as_any()
            .downcast_ref::<Int64Array>()
            .ok_or(RouterError::InvalidColumn("bucket_utc_ms must be Int64"))?;
        let min_col = batch
            .column(2)
            .as_any()
            .downcast_ref::<Float64Array>()
            .ok_or(RouterError::InvalidColumn("min_val must be Float64"))?;
        let max_col = batch
            .column(3)
            .as_any()
            .downcast_ref::<Float64Array>()
            .ok_or(RouterError::InvalidColumn("max_val must be Float64"))?;
        let mean_col = batch
            .column(4)
            .as_any()
            .downcast_ref::<Float64Array>()
            .ok_or(RouterError::InvalidColumn("mean_val must be Float64"))?;
        let count_col = batch.column(5).as_any().downcast_ref::<UInt32Array>();

        let keys = sig_col.keys().values();
        let buckets = bucket_col.values();
        let mins = min_col.values();
        let maxs = max_col.values();
        let means = mean_col.values();

        let mut current_key: Option<i32> = None;
        let mut current_vec: Option<&mut Vec<EnvelopePoint>> = None;

        let dict_len = dict_values.len();
        for row in 0..batch.num_rows() {
            if sig_col.is_null(row) {
                continue;
            }
            let key = keys[row];
            let Ok(key_idx) = usize::try_from(key) else {
                return Err(RouterError::InvalidColumn(
                    "negative dictionary key in overview Parquet",
                ));
            };
            if key_idx >= dict_len {
                return Err(RouterError::InvalidColumn(
                    "out-of-bounds dictionary key in overview Parquet",
                ));
            }
            let bucket_ms = buckets[row];
            let sample_count = count_col.map_or(1, |c| c.value(row));
            let act_entry = activity.entry(bucket_ms).or_insert(0);
            *act_entry = act_entry.saturating_add(sample_count);

            if current_key != Some(key) {
                let sig_name = dict_values.value(key_idx);
                current_key = Some(key);
                current_vec = Some(map.entry(sig_name.to_owned()).or_default());
            }
            if let Some(target_vec) = current_vec.as_mut() {
                target_vec.push(EnvelopePoint {
                    utc_ms: bucket_ms,
                    min: mins[row],
                    max: maxs[row],
                    mean: means[row],
                });
            }
        }
    }

    let series_map = map
        .into_iter()
        .map(|(k, pts)| (k, Arc::new(SignalSeries::Envelope(pts))))
        .collect();
    let activity_vec: Vec<(i64, u32)> = activity.into_iter().collect();
    Ok((series_map, activity_vec))
}

/// Read a single signal from an overview Parquet file (`overview_10hz.parquet` or `overview_1hz.parquet`).
pub fn read_single_signal_overview(
    path: &Path,
    target_signal_id: &str,
) -> Result<SignalSeries, RouterError> {
    let file = fs::File::open(path)?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file).map_err(parquet_err)?;
    let row_groups = matching_row_groups_for_signal(&builder, target_signal_id);
    if row_groups.is_empty() {
        return Ok(SignalSeries::Envelope(Vec::new()));
    }
    let mask = ProjectionMask::roots(builder.parquet_schema(), [0, 1, 2, 3, 4]);
    let reader = builder
        .with_row_groups(row_groups)
        .with_projection(mask)
        .build()
        .map_err(parquet_err)?;

    let mut points = Vec::new();
    let mut found_any = false;

    for batch_result in reader {
        let batch = batch_result.map_err(arrow_err)?;
        let sig_col = batch
            .column(0)
            .as_any()
            .downcast_ref::<DictionaryArray<Int32Type>>()
            .ok_or(RouterError::InvalidColumn(
                "overview col 0 must be Dictionary<Int32, Utf8>",
            ))?;
        let Some(target_key) = find_dictionary_key(sig_col, target_signal_id)? else {
            if found_any || batch_passed_sorted_signal(sig_col, target_signal_id) {
                break;
            }
            continue;
        };

        let bucket_col = batch
            .column(1)
            .as_any()
            .downcast_ref::<Int64Array>()
            .ok_or(RouterError::InvalidColumn("bucket_utc_ms must be Int64"))?;
        let min_col = batch
            .column(2)
            .as_any()
            .downcast_ref::<Float64Array>()
            .ok_or(RouterError::InvalidColumn("min_val must be Float64"))?;
        let max_col = batch
            .column(3)
            .as_any()
            .downcast_ref::<Float64Array>()
            .ok_or(RouterError::InvalidColumn("max_val must be Float64"))?;
        let mean_col = batch
            .column(4)
            .as_any()
            .downcast_ref::<Float64Array>()
            .ok_or(RouterError::InvalidColumn("mean_val must be Float64"))?;

        let keys = sig_col.keys().values();
        let buckets = bucket_col.values();
        let mins = min_col.values();
        let maxs = max_col.values();
        let means = mean_col.values();

        if let Some(first_idx) = keys.iter().position(|&k| k == target_key) {
            found_any = true;
            let last_idx = keys
                .iter()
                .rposition(|&k| k == target_key)
                .unwrap_or(first_idx);
            points.reserve(last_idx - first_idx + 1);
            for row in first_idx..=last_idx {
                if keys[row] == target_key {
                    points.push(EnvelopePoint {
                        utc_ms: buckets[row],
                        min: mins[row],
                        max: maxs[row],
                        mean: means[row],
                    });
                }
            }
            if last_idx + 1 < batch.num_rows() {
                break;
            }
        } else if found_any || batch_passed_sorted_signal(sig_col, target_signal_id) {
            break;
        }
    }

    Ok(SignalSeries::Envelope(points))
}

/// Read all discrete state intervals from `states.parquet`.
pub fn read_all_signals_states(
    path: &Path,
) -> Result<HashMap<SignalId, Arc<Vec<StateInterval>>>, RouterError> {
    let file = fs::File::open(path)?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file).map_err(parquet_err)?;
    let reader = builder.build().map_err(parquet_err)?;

    let mut map: HashMap<SignalId, Vec<StateInterval>> = HashMap::new();

    for batch_result in reader {
        let batch = batch_result.map_err(arrow_err)?;
        let sig_col = batch
            .column(0)
            .as_any()
            .downcast_ref::<DictionaryArray<Int32Type>>()
            .ok_or(RouterError::InvalidColumn(
                "states col 0 must be Dictionary<Int32, Utf8>",
            ))?;
        let dict_values = sig_col
            .values()
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or(RouterError::InvalidColumn(
                "states dictionary values must be Utf8",
            ))?;
        let start_col = batch
            .column(1)
            .as_any()
            .downcast_ref::<Int64Array>()
            .ok_or(RouterError::InvalidColumn("start_utc_ms must be Int64"))?;
        let end_col = batch
            .column(2)
            .as_any()
            .downcast_ref::<Int64Array>()
            .ok_or(RouterError::InvalidColumn("end_utc_ms must be Int64"))?;
        let raw_col = batch
            .column(3)
            .as_any()
            .downcast_ref::<Int64Array>()
            .ok_or(RouterError::InvalidColumn("raw_int must be Int64"))?;
        let label_col = batch
            .column(4)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or(RouterError::InvalidColumn("label must be Utf8"))?;

        let keys = sig_col.keys().values();
        let starts = start_col.values();
        let ends = end_col.values();
        let raws = raw_col.values();

        let dict_len = dict_values.len();
        for row in 0..batch.num_rows() {
            if sig_col.is_null(row) {
                continue;
            }
            let Ok(key_idx) = usize::try_from(keys[row]) else {
                return Err(RouterError::InvalidColumn(
                    "negative dictionary key in states Parquet",
                ));
            };
            if key_idx >= dict_len {
                return Err(RouterError::InvalidColumn(
                    "out-of-bounds dictionary key in states Parquet",
                ));
            }
            let sig_name = dict_values.value(key_idx);
            map.entry(sig_name.to_owned())
                .or_default()
                .push(StateInterval {
                    start_utc_ms: starts[row],
                    end_utc_ms: ends[row],
                    raw_int: raws[row],
                    label: label_col.value(row).to_owned(),
                });
        }
    }

    Ok(map.into_iter().map(|(k, v)| (k, Arc::new(v))).collect())
}

/// Read a single signal's exact `(utc_ms, value)` series from `full_res.parquet`, skipping `ticks_ms`.
pub fn read_single_signal_full_res(
    path: &Path,
    target_signal_id: &str,
) -> Result<SignalSeries, RouterError> {
    let file = fs::File::open(path)?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file).map_err(parquet_err)?;
    let row_groups = matching_row_groups_for_signal(&builder, target_signal_id);
    if row_groups.is_empty() {
        return Ok(SignalSeries::Exact {
            timestamps_ms: Vec::new(),
            values: Vec::new(),
        });
    }
    // Project only col 0 (signal_id), col 1 (utc_ms), and col 3 (value); skip col 2 (ticks_ms).
    let mask = ProjectionMask::roots(builder.parquet_schema(), [0, 1, 3]);
    let reader = builder
        .with_row_groups(row_groups)
        .with_projection(mask)
        .build()
        .map_err(parquet_err)?;

    let mut timestamps_ms = Vec::new();
    let mut values = Vec::new();
    let mut found_any = false;

    for batch_result in reader {
        let batch = batch_result.map_err(arrow_err)?;
        let sig_col = batch
            .column(0)
            .as_any()
            .downcast_ref::<DictionaryArray<Int32Type>>()
            .ok_or(RouterError::InvalidColumn(
                "full_res col 0 must be Dictionary<Int32, Utf8>",
            ))?;
        let Some(target_key) = find_dictionary_key(sig_col, target_signal_id)? else {
            if found_any || batch_passed_sorted_signal(sig_col, target_signal_id) {
                break;
            }
            continue;
        };

        let utc_col = batch
            .column(1)
            .as_any()
            .downcast_ref::<Int64Array>()
            .ok_or(RouterError::InvalidColumn("utc_ms must be Int64"))?;
        let val_col = batch
            .column(2)
            .as_any()
            .downcast_ref::<Float64Array>()
            .ok_or(RouterError::InvalidColumn("value must be Float64"))?;

        let keys = sig_col.keys().values();
        let ts_slice = utc_col.values();
        let val_slice = val_col.values();

        // Fast path: because full_res.parquet is written grouped by signal_id, locate the
        // first and last matching row in the batch and verify contiguity for direct slice copy.
        if let Some(first_idx) = keys.iter().position(|&k| k == target_key) {
            found_any = true;
            let last_idx = keys
                .iter()
                .rposition(|&k| k == target_key)
                .unwrap_or(first_idx);
            let span_len = last_idx - first_idx + 1;
            let is_contiguous = keys[first_idx..=last_idx].iter().all(|&k| k == target_key);
            if is_contiguous {
                timestamps_ms.reserve(span_len);
                values.reserve(span_len);
                timestamps_ms.extend_from_slice(&ts_slice[first_idx..=last_idx]);
                values.extend_from_slice(&val_slice[first_idx..=last_idx]);
            } else {
                for row in first_idx..=last_idx {
                    if keys[row] == target_key {
                        timestamps_ms.push(ts_slice[row]);
                        values.push(val_slice[row]);
                    }
                }
            }
            if last_idx + 1 < batch.num_rows() {
                break;
            }
        } else if found_any || batch_passed_sorted_signal(sig_col, target_signal_id) {
            break;
        }
    }

    Ok(SignalSeries::Exact {
        timestamps_ms,
        values,
    })
}

/// Read `overview_1hz.parquet`, `states.parquet`, and `manifest.json` from `partition_dir`
/// into a [`PartitionPreloadPayload`].
pub fn preload_partition_dir_from_parquet(
    partition_id: &str,
    partition_dir: &Path,
    known_signals: Option<&[SignalManifestEntry]>,
) -> Result<Option<PartitionPreloadPayload>, RouterError> {
    let overview_1hz_path = partition_dir.join("overview_1hz.parquet");
    let states_path = partition_dir.join("states.parquet");
    let manifest_path = partition_dir.join("manifest.json");

    if !overview_1hz_path.is_file() {
        return Ok(None);
    }

    let (overview_1hz, activity_1hz) = read_all_signals_overview_with_activity(&overview_1hz_path)?;
    let states = if states_path.is_file() {
        read_all_signals_states(&states_path)?
    } else {
        HashMap::new()
    };

    let signals = if let Some(sigs) = known_signals {
        sigs.to_vec()
    } else if manifest_path.is_file() {
        let bytes = fs::read(&manifest_path)?;
        let manifest: PartitionManifest =
            serde_json::from_slice(&bytes).map_err(|e| RouterError::Format(e.to_string()))?;
        manifest.signals
    } else {
        Vec::new()
    };

    Ok(Some(PartitionPreloadPayload {
        partition_id: partition_id.to_owned(),
        overview_1hz,
        activity_1hz,
        states,
        signals,
    }))
}

/// Build a compact binary `DAQP\x01` preload bundle (`manifest.json` + `overview_1hz` + `states` + `activity_1hz`)
/// from a partition directory on disk.
pub fn build_partition_preload_payload(
    partition_id: &str,
    part_dir: &Path,
) -> Result<Vec<u8>, RouterError> {
    let payload =
        preload_partition_dir_from_parquet(partition_id, part_dir, None)?.ok_or_else(|| {
            RouterError::Format(format!(
                "overview_1hz.parquet missing in {}",
                part_dir.display()
            ))
        })?;
    encode_preload_payload(&payload).map_err(RouterError::Format)
}

/// Build a compact binary `DAQT\x01` tile payload (`Overview1Hz`, `Overview10Hz`, or `FullRes`)
/// for a single `signal_id` from a partition directory on disk.
pub fn build_partition_tile_payload(
    part_dir: &Path,
    signal_id: &str,
    resolution: Resolution,
) -> Result<Vec<u8>, RouterError> {
    let series = match resolution {
        Resolution::Overview1Hz => {
            let path = part_dir.join("overview_1hz.parquet");
            read_single_signal_overview(&path, signal_id)?
        }
        Resolution::Overview10Hz => {
            let path = part_dir.join("overview_10hz.parquet");
            read_single_signal_overview(&path, signal_id)?
        }
        Resolution::FullRes => {
            let path = part_dir.join("full_res.parquet");
            read_single_signal_full_res(&path, signal_id)?
        }
    };
    Ok(encode_tile_payload(&series))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        router::{TileCache, TileLookup},
        storage::NativeTileLoader,
        superdbc::database::Database,
    };
    use std::{path::PathBuf, time::Duration};

    #[test]
    fn tile_cache_preload_and_query_envelope_and_states_from_per26_partition() {
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
        let part_dir = tmp_store.path().join("partitions").join(&m0.partition_id);

        let loader = Arc::new(NativeTileLoader::new());
        let mut cache = TileCache::with_loader(64 * 1024 * 1024, Some(loader));
        cache
            .preload_partition(&m0.partition_id, &part_dir, Some(&m0.signals))
            .expect("preload partition");
        assert!(cache.is_partition_preloaded(&m0.partition_id));

        // 1. 1Hz overview is immediately Ready in RAM
        let lookup_1hz = cache
            .query_signal(
                &m0.partition_id,
                "IMU_acceleration.X_axis",
                Resolution::Overview1Hz,
            )
            .expect("IMU_acceleration.X_axis exists");
        assert!(!lookup_1hz.is_fallback());
        assert!(!lookup_1hz.series().is_empty());

        // 2. Requesting FullRes returns 1Hz fallback immediately and queues background read
        let lookup_full_first = cache
            .query_signal(
                &m0.partition_id,
                "IMU_acceleration.X_axis",
                Resolution::FullRes,
            )
            .expect("IMU_acceleration.X_axis fallback");
        assert!(matches!(
            lookup_full_first,
            TileLookup::Fallback {
                actual_resolution: Resolution::Overview1Hz,
                ..
            }
        ));

        // 3. Wait for background worker to finish loading FullRes
        let loaded = cache.wait_for_in_flight(Duration::from_secs(5));
        assert!(loaded >= 1);

        let lookup_full_ready = cache
            .query_signal(
                &m0.partition_id,
                "IMU_acceleration.X_axis",
                Resolution::FullRes,
            )
            .expect("IMU_acceleration.X_axis ready");
        assert!(!lookup_full_ready.is_fallback());
        assert!(matches!(
            lookup_full_ready.series().as_ref(),
            SignalSeries::Exact { .. }
        ));
        assert!(lookup_full_ready.series().len() > lookup_1hz.series().len());

        // 4. Discrete states are resident in RAM
        let states = cache
            .query_states(&m0.partition_id, "main_hb.car_state")
            .expect("main_hb.car_state states");
        assert!(!states.is_empty());
    }
}
