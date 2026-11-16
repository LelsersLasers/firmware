pub mod cloud_sync;
pub mod partition_writer;
pub mod registry;
pub mod schema;

pub use cloud_sync::{
    AnnotationPatchRequest, AnnotationUpsertPayload, AssetUploadResponse, CHECKSUM_HEADER,
    CatalogResponse, CloudSyncCommand, CloudSyncEvent, CloudSyncHandle, CloudSyncStatus,
    CloudSyncSummary, CloudWsMessage, RANGE_CHECKSUM_HEADER, compute_file_sha256_hex,
    compute_sha256_hex, download_file_resumable, http_base_to_ws_url, is_safe_path_segment,
    normalize_http_base_url, sync_store_with_cloud_blocking, upload_file_verified,
};

pub use partition_writer::{
    ImportError, ImportSummary, PartitionManifest, RawSample, SignalColumnAccumulator,
    SignalManifestEntry, archive_superdbc_to_store, load_archived_superdbc,
    write_column_accumulators_to_store, write_correlated_epochs_to_store,
    write_correlated_epochs_to_store_for_day, write_correlated_epochs_to_store_with_progress,
};
pub use registry::{
    Annotation, LOCAL_REGISTRY_FILENAME, RawAssetRecord, RegisteredPartition, Registry,
    RegistryError, TrackDay,
};

pub type SignalId = String;

/// Canonical signal identifier format: `"{msg_name}.{sig_name}"`.
///
/// CANpiler enforces globally unique message names across all buses and nodes,
/// keeping signal identifiers concise and subteam-friendly.
pub fn format_signal_id(message: &str, signal: &str) -> SignalId {
    format!("{message}.{signal}")
}
