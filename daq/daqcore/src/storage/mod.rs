pub mod partition_writer;
pub mod registry;
pub mod schema;

pub use partition_writer::{
    ImportError, ImportSummary, PartitionManifest, SignalManifestEntry,
    write_correlated_epochs_to_store,
};
pub use registry::{
    LOCAL_REGISTRY_FILENAME, RawAssetRecord, RegisteredPartition, Registry, RegistryError, TrackDay,
};

pub type SignalId = String;

/// Canonical signal identifier format: `"{msg_name}.{sig_name}"`.
///
/// CANpiler enforces globally unique message names across all buses and nodes,
/// keeping signal identifiers concise and subteam-friendly.
pub fn format_signal_id(message: &str, signal: &str) -> SignalId {
    format!("{message}.{signal}")
}
