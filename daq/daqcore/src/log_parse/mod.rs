use std::path;

pub mod auto_annotate;
pub mod consts;
pub mod correlate;
pub mod parse;
use crate::{storage, superdbc};

/// Live progress update emitted during an SD card `.log` import and Parquet partition build.
#[derive(Debug, Clone, PartialEq)]
pub enum ImportProgress {
    ScanningFiles,
    ParsingFile {
        file_index: usize,
        total_files: usize,
        file_name: String,
    },
    CorrelatingEpochs {
        epoch_count: usize,
    },
    WritingPartition {
        partition_index: usize,
        total_partitions: usize,
        partition_id: String,
    },
    Complete(storage::ImportSummary),
}

impl ImportProgress {
    /// Progress fraction in `[0.0, 1.0]` suitable for driving an `egui::ProgressBar`.
    pub fn fraction(&self) -> f32 {
        match self {
            Self::ScanningFiles => 0.02,
            Self::ParsingFile {
                file_index,
                total_files,
                ..
            } => {
                let denom = (*total_files).max(1) as f32;
                0.05 + 0.40 * ((*file_index as f32) / denom).clamp(0.0, 1.0)
            }
            Self::CorrelatingEpochs { .. } => 0.50,
            Self::WritingPartition {
                partition_index,
                total_partitions,
                ..
            } => {
                let denom = (*total_partitions).max(1) as f32;
                0.55 + 0.40 * ((*partition_index as f32) / denom).clamp(0.0, 1.0)
            }
            Self::Complete(_) => 1.0,
        }
    }

    /// Human-readable status line describing the current import phase.
    pub fn status_text(&self) -> String {
        match self {
            Self::ScanningFiles => "Scanning directory for raw .log files…".to_owned(),
            Self::ParsingFile {
                file_index,
                total_files,
                file_name,
            } => format!(
                "Decoding {} ({}/{})",
                file_name,
                file_index + 1,
                (*total_files).max(1)
            ),
            Self::CorrelatingEpochs { epoch_count } => {
                format!("Correlating wall-clock epochs ({epoch_count} boot epochs detected)…")
            }
            Self::WritingPartition {
                partition_index,
                total_partitions,
                partition_id,
            } => format!(
                "Writing Parquet pyramid & auto-annotations {} ({}/{})",
                partition_id,
                partition_index + 1,
                (*total_partitions).max(1)
            ),
            Self::Complete(summary) => {
                let annotations: usize =
                    summary.manifests.iter().map(|m| m.annotations.len()).sum();
                format!(
                    "Import complete: {} files, {} epochs, {} partitions written ({} skipped), {} annotations",
                    summary.log_files_parsed,
                    summary.boot_epochs_detected,
                    summary.partitions_written,
                    summary.partitions_skipped_existing,
                    annotations,
                )
            }
        }
    }
}

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
