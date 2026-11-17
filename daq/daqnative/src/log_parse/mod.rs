use std::path;

pub mod parse;
pub mod per26;

pub use daqcore::log_parse::{ImportProgress, auto_annotate, consts, correlate};

use crate::{storage, superdbc};

/// Parse a directory of raw `.log` files, stitch them into contiguous MCU boot epochs,
/// apply 3-tier wall-clock correlation, auto-detect driving stints and fault transitions,
/// and write multi-resolution Parquet partition pyramids into `<store_dir>/partitions/`.
pub fn import_logs_to_store(
    logs_dir: &path::Path,
    store_dir: &path::Path,
    database: &superdbc::database::Database,
) -> Result<storage::ImportSummary, storage::ImportError> {
    import_logs_to_store_with_progress(logs_dir, store_dir, database, |_| {})
}

/// Same as [`import_logs_to_store`], invoking `on_progress` as each file and partition is processed.
pub fn import_logs_to_store_with_progress<F>(
    logs_dir: &path::Path,
    store_dir: &path::Path,
    database: &superdbc::database::Database,
    mut on_progress: F,
) -> Result<storage::ImportSummary, storage::ImportError>
where
    F: FnMut(ImportProgress),
{
    on_progress(ImportProgress::ScanningFiles);
    let parsed_set =
        parse::parse_log_files_with_progress(logs_dir, database, |idx, total, file_name| {
            on_progress(ImportProgress::ParsingFile {
                file_index: idx,
                total_files: total,
                file_name: file_name.to_owned(),
            });
        })?;
    let chunked = parse::chunk_parsed_with_files(&parsed_set.files, parsed_set.messages);
    on_progress(ImportProgress::CorrelatingEpochs {
        epoch_count: chunked.len(),
    });
    let correlated = correlate::time_correlate_chunks(chunked, &parsed_set.files);
    let summary = storage::write_correlated_epochs_to_store_with_progress(
        store_dir,
        &parsed_set.files,
        correlated,
        database,
        |idx, total, partition_id| {
            on_progress(ImportProgress::WritingPartition {
                partition_index: idx,
                total_partitions: total,
                partition_id: partition_id.to_owned(),
            });
        },
    )?;
    on_progress(ImportProgress::Complete(summary.clone()));
    Ok(summary)
}
