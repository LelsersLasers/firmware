pub mod cloud_sync;
pub mod native_backend;
pub mod parquet_reader;
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

pub use native_backend::{
    NativeRouterBackend, NativeRouterExt, NativeTileLoader, live_router, open_store_router,
};
pub use parquet_reader::{
    build_partition_preload_payload, build_partition_tile_payload,
    preload_partition_dir_from_parquet, read_all_signals_overview,
    read_all_signals_overview_with_activity, read_all_signals_states, read_single_signal_full_res,
    read_single_signal_overview,
};
pub use partition_writer::{
    ImportError, ImportSummary, PartitionManifest, RawSample, SignalColumnAccumulator,
    SignalManifestEntry, archive_superdbc_to_store, load_archived_superdbc,
    read_partition_column_accumulators, rewrite_partition_in_place,
    write_column_accumulators_to_store, write_correlated_epochs_to_store,
    write_correlated_epochs_to_store_for_day, write_correlated_epochs_to_store_with_progress,
};
pub use registry::{
    Annotation, LOCAL_REGISTRY_FILENAME, RawAssetRecord, RegisteredPartition, Registry,
    RegistryError, TrackDay,
};

pub use daqcore::storage::{InMemoryCatalog, SignalId, format_signal_id};
