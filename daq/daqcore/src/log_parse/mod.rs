use std::path;

pub mod auto_annotate;
pub mod consts;
pub mod correlate;
pub mod parse;
use crate::{storage, superdbc};

/// Parse a directory of raw `.log` files, stitch them into contiguous MCU boot epochs,
/// apply 3-tier wall-clock correlation, auto-detect driving stints and fault transitions,
/// and write multi-resolution Parquet partition pyramids into `<store_dir>/partitions/`.
pub fn import_logs_to_store(
    logs_dir: &path::Path,
    store_dir: &path::Path,
    database: &superdbc::database::Database,
) -> Result<storage::ImportSummary, storage::ImportError> {
    let parsed_set = parse::parse_log_files(logs_dir, database)?;
    let chunked = parse::chunk_parsed_with_files(&parsed_set.files, parsed_set.messages);
    let correlated = correlate::time_correlate_chunks(chunked, &parsed_set.files);
    storage::write_correlated_epochs_to_store(store_dir, &parsed_set.files, correlated, database)
}
