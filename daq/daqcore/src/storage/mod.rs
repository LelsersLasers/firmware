//! Portable storage, manifest, annotation, and cloud sync protocol types.

pub use crate::analysis::DerivedSignalDef;
pub use crate::log_parse::auto_annotate::{Annotation, FaultSeverity};
use crate::log_parse::{correlate, parse};
use sha2::Digest as _;
use std::{collections::BTreeMap, fmt};

pub type SignalId = String;

pub const LOCAL_REGISTRY_FILENAME: &str = "local_registry.sqlite";
pub const CHECKSUM_HEADER: &str = "x-checksum-sha256";
pub const RANGE_CHECKSUM_HEADER: &str = "x-range-checksum-sha256";

/// Canonical signal identifier format: `"{msg_name}.{sig_name}"`.
pub fn format_signal_id(message: &str, signal: &str) -> SignalId {
    format!("{message}.{signal}")
}

pub fn default_track_day_title(track_day_id: &str) -> String {
    format!("Track Day {track_day_id}")
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TrackDay {
    pub id: String,
    pub title: String,
    pub location: String,
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignalManifestEntry {
    pub signal_id: SignalId,
    pub unit: String,
    pub is_discrete: bool,
    pub bus_name: String,
    pub bus_id: u8,
    pub can_id: u32,
    pub transmitter: String,
    pub nominal_period_ms: Option<u32>,
    pub description: String,
    pub sample_count: u64,
    pub min_utc_ms: i64,
    pub max_utc_ms: i64,
    pub min_val: f64,
    pub max_val: f64,
}

impl SignalManifestEntry {
    pub fn fault_severity(&self) -> Option<FaultSeverity> {
        let (msg_name, sig_name) = self
            .signal_id
            .split_once('.')
            .unwrap_or(("", self.signal_id.as_str()));
        if !crate::log_parse::auto_annotate::is_fault_flag_signal(msg_name, sig_name) {
            return None;
        }
        FaultSeverity::resolve_for_signal(&self.signal_id, None)
    }
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
    pub annotations: Vec<Annotation>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImportSummary {
    pub log_files_parsed: usize,
    pub boot_epochs_detected: usize,
    pub partitions_written: usize,
    pub partitions_skipped_existing: usize,
    pub manifests: Vec<PartitionManifest>,
}

#[derive(Debug, Clone)]
pub struct RawSample {
    pub utc_ms: i64,
    pub ticks_ms: u32,
    pub physical: f64,
    pub raw_int: i64,
    pub enum_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RegisteredPartition {
    pub partition_id: String,
    pub track_day_id: String,
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    pub clock_mapping: correlate::ClockMapping,
    pub superdbc_hash: String,
    pub relative_path: String,
    pub is_local: bool,
    pub manifest: PartitionManifest,
}

impl RegisteredPartition {
    pub fn from_manifest(manifest: PartitionManifest, is_local: bool) -> Self {
        let relative_path = format!("partitions/{}/", manifest.partition_id);
        Self {
            partition_id: manifest.partition_id.clone(),
            track_day_id: manifest.track_day_id.clone(),
            start_utc_ms: manifest.start_utc_ms,
            end_utc_ms: manifest.end_utc_ms.max(manifest.start_utc_ms),
            clock_mapping: manifest.clock_mapping.clone(),
            superdbc_hash: manifest.superdbc_hash.clone(),
            relative_path,
            is_local,
            manifest,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RawAssetRecord {
    pub sha256_hex: String,
    pub track_day_id: String,
    pub file_name: String,
    pub rtc_timestamp_ms: Option<i64>,
    pub mtime_unix_ms: i64,
    pub relative_path: String,
    pub is_local: bool,
}

#[derive(Debug, Clone)]
pub struct RegistryErrorMessage(pub String);

impl fmt::Display for RegistryErrorMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RegistryErrorMessage {}

/// Catalog payload returned by `GET /api/v1/catalog` on `daqcloud`.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CatalogResponse {
    #[serde(default)]
    pub track_days: Vec<TrackDay>,
    #[serde(default)]
    pub partitions: Vec<PartitionManifest>,
    #[serde(default)]
    pub annotations: Vec<Annotation>,
    #[serde(default)]
    pub derived_signals: Vec<DerivedSignalDef>,
}

/// Metadata payload returned by `PUT /api/v1/partitions/{partition_id}/{filename}`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AssetUploadResponse {
    pub partition_id: String,
    pub filename: String,
    pub size_bytes: u64,
    pub sha256_hex: String,
}

/// Flexible request payload for `POST /api/v1/derived-signals`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum DerivedSignalUpsertPayload {
    Single(Box<DerivedSignalDef>),
    Batch(Vec<DerivedSignalDef>),
}

impl DerivedSignalUpsertPayload {
    pub fn into_vec(self) -> Vec<DerivedSignalDef> {
        match self {
            Self::Single(def) => vec![*def],
            Self::Batch(list) => list,
        }
    }
}

/// Flexible request payload for `POST /api/v1/annotations`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum AnnotationUpsertPayload {
    Single(Box<Annotation>),
    Batch(Vec<Annotation>),
}

impl AnnotationUpsertPayload {
    pub fn into_vec(self) -> Vec<Annotation> {
        match self {
            Self::Single(ann) => vec![*ann],
            Self::Batch(list) => list,
        }
    }
}

/// Partial update payload for `PATCH /api/v1/annotations` or `PATCH /api/v1/annotations/{id}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AnnotationPatchRequest {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub start_utc_ms: Option<i64>,
    #[serde(default)]
    pub end_utc_ms: Option<Option<i64>>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub updated_at_ms: Option<i64>,
    #[serde(default)]
    pub deleted: Option<bool>,
}

impl AnnotationPatchRequest {
    pub fn apply_to(&self, existing: &Annotation, fallback_now_ms: i64) -> Annotation {
        let mut updated = existing.clone();
        if let Some(name) = &self.name {
            updated.name = name.clone();
        }
        if let Some(category) = &self.category {
            updated.category = category.clone();
        }
        if let Some(color) = &self.color {
            updated.color = color.clone();
        }
        if let Some(start_utc_ms) = self.start_utc_ms {
            updated.start_utc_ms = start_utc_ms;
        }
        if let Some(end_utc_ms) = self.end_utc_ms {
            updated.end_utc_ms = end_utc_ms.map(|e| e.max(updated.start_utc_ms));
        } else if let Some(end_ms) = updated.end_utc_ms
            && end_ms < updated.start_utc_ms
        {
            updated.end_utc_ms = Some(updated.start_utc_ms);
        }
        if let Some(author) = &self.author {
            updated.author = author.clone();
        }
        if let Some(source) = &self.source {
            updated.source = source.clone();
        } else {
            updated.source = "manual".to_owned();
        }
        if let Some(notes) = &self.notes {
            updated.notes = notes.clone();
        }
        if let Some(deleted) = self.deleted {
            updated.deleted = deleted;
        }
        updated.updated_at_ms = self
            .updated_at_ms
            .unwrap_or_else(|| fallback_now_ms.max(existing.updated_at_ms.saturating_add(1)));
        updated
    }
}

/// Real-time WebSocket message broadcast over `/api/v1/ws` between `daqcloud` and clients.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CloudWsMessage {
    PartitionAvailable {
        manifest: PartitionManifest,
    },
    AnnotationUpdated {
        annotation: Annotation,
    },
    AnnotationDeleted {
        annotation_id: String,
        updated_at_ms: i64,
    },
    DerivedSignalUpdated {
        signal: DerivedSignalDef,
    },
    DerivedSignalDeleted {
        id: String,
        updated_at_ms: i64,
    },
    Ping {
        utc_ms: i64,
    },
}

/// Compute lowercase hex SHA-256 digest of an in-memory byte slice.
pub fn compute_sha256_hex(bytes: &[u8]) -> String {
    let digest = sha2::Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}

/// Validate a URL path segment (`partition_id` or `filename`) so it cannot traverse directories.
pub fn is_safe_path_segment(segment: &str) -> bool {
    let trimmed = segment.trim();
    !trimmed.is_empty()
        && trimmed != "."
        && trimmed != ".."
        && !trimmed.contains("..")
        && !trimmed.contains('/')
        && !trimmed.contains('\\')
        && !trimmed.contains('\0')
        && trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// Convert an HTTP(S) base URL (`http://host:3000`) into its corresponding WebSocket endpoint
/// (`ws://host:3000/api/v1/ws`).
pub fn http_base_to_ws_url(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    let ws_base = if let Some(rest) = trimmed.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        format!("ws://{rest}")
    } else if trimmed.starts_with("ws://") || trimmed.starts_with("wss://") {
        trimmed.to_owned()
    } else {
        format!("ws://{trimmed}")
    };
    format!("{ws_base}/api/v1/ws")
}

/// Normalize a user-entered `daqcloud` base URL (`"localhost:3000"` -> `"http://localhost:3000"`).
pub fn normalize_http_base_url(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_owned()
    } else if let Some(rest) = trimmed.strip_prefix("ws://") {
        format!("http://{rest}")
    } else if let Some(rest) = trimmed.strip_prefix("wss://") {
        format!("https://{rest}")
    } else {
        format!("http://{trimmed}")
    }
}

/// Live connection and transfer status for a `daqcloud` sync backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudSyncStatus {
    Disconnected,
    Connecting {
        url: String,
    },
    Connected {
        url: String,
        synced_partitions: usize,
        pending_downloads: usize,
        pending_uploads: usize,
        ws_connected: bool,
    },
    Error {
        url: String,
        error: String,
    },
}

impl CloudSyncStatus {
    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Connected { .. })
    }

    pub fn url(&self) -> Option<&str> {
        match self {
            Self::Disconnected => None,
            Self::Connecting { url } | Self::Connected { url, .. } | Self::Error { url, .. } => {
                Some(url.as_str())
            }
        }
    }
}

/// Commands sent from [`crate::router::VirtualTimelineRouter`] or UI controls to a `daqcloud` sync worker.
#[derive(Debug, Clone)]
pub enum CloudSyncCommand {
    SyncCatalog,
    DownloadPartition {
        partition_id: String,
    },
    PrefetchOverview {
        manifest: PartitionManifest,
    },
    UploadLocalPartitions,
    PushAnnotation {
        annotation: Annotation,
    },
    PushTrackDay {
        day: TrackDay,
    },
    DeleteAnnotation {
        annotation_id: String,
        updated_at_ms: i64,
    },
    PushDerivedSignal {
        signal: DerivedSignalDef,
    },
    DeleteDerivedSignal {
        id: String,
        updated_at_ms: i64,
    },
    SetActiveTrackDay {
        track_day_id: Option<String>,
    },
    FetchStats,
    Shutdown,
}

/// Latency statistics for an individual HTTP endpoint category or overall server traffic.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EndpointLatencyStats {
    pub endpoint: String,
    pub request_count: u64,
    pub error_count: u64,
    pub avg_ms: f64,
    pub p50_ms: f64,
    pub p90_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
    pub last_ms: f64,
}

/// Individual recent HTTP request latency sample recorded by `daqcloud`.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CloudLatencySample {
    pub utc_ms: i64,
    pub elapsed_ms: f64,
    pub endpoint: String,
    pub status: u16,
    pub cache_hit: bool,
}

/// In-memory cache utilization and hit/miss statistics on `daqcloud`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CloudCacheStats {
    pub total_used_bytes: u64,
    pub total_max_bytes: u64,
    pub tile_used_bytes: u64,
    pub tile_max_bytes: u64,
    pub tile_entries: usize,
    pub tile_hits: u64,
    pub tile_misses: u64,
    pub preload_used_bytes: u64,
    pub preload_max_bytes: u64,
    pub preload_entries: usize,
    pub preload_hits: u64,
    pub preload_misses: u64,
    pub catalog_used_bytes: u64,
    pub catalog_hits: u64,
    pub catalog_misses: u64,
    pub sha256_entries: usize,
    pub sha256_hits: u64,
    pub sha256_misses: u64,
}

impl CloudCacheStats {
    pub fn total_hits(&self) -> u64 {
        self.tile_hits
            .saturating_add(self.preload_hits)
            .saturating_add(self.catalog_hits)
    }

    pub fn total_misses(&self) -> u64 {
        self.tile_misses
            .saturating_add(self.preload_misses)
            .saturating_add(self.catalog_misses)
    }

    pub fn hit_rate_pct(&self) -> Option<f64> {
        let hits = self.total_hits();
        let total = hits.saturating_add(self.total_misses());
        if total == 0 {
            None
        } else {
            Some((hits as f64 / total as f64) * 100.0)
        }
    }

    pub fn tile_hit_rate_pct(&self) -> Option<f64> {
        let total = self.tile_hits.saturating_add(self.tile_misses);
        if total == 0 {
            None
        } else {
            Some((self.tile_hits as f64 / total as f64) * 100.0)
        }
    }

    pub fn preload_hit_rate_pct(&self) -> Option<f64> {
        let total = self.preload_hits.saturating_add(self.preload_misses);
        if total == 0 {
            None
        } else {
            Some((self.preload_hits as f64 / total as f64) * 100.0)
        }
    }
}

/// Process RSS/virtual memory and host VPS memory statistics reported by `daqcloud`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CloudMemoryStats {
    pub process_rss_bytes: Option<u64>,
    pub process_virtual_bytes: Option<u64>,
    pub vps_total_bytes: Option<u64>,
    pub vps_free_bytes: Option<u64>,
    pub vps_available_bytes: Option<u64>,
    pub vps_buffers_cached_bytes: Option<u64>,
    pub vps_swap_total_bytes: Option<u64>,
    pub vps_swap_free_bytes: Option<u64>,
}

/// On-disk store inventory and disk usage statistics reported by `daqcloud`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CloudStoreStats {
    pub track_days: usize,
    pub partitions: usize,
    pub annotations: usize,
    pub derived_signals: usize,
    pub store_disk_bytes: u64,
}

/// Telemetry & system health snapshot returned by `GET /api/v1/stats` on `daqcloud`.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CloudStatsResponse {
    pub collected_at_utc_ms: i64,
    pub uptime_secs: f64,
    pub active_ws_clients: usize,
    pub in_flight_http_requests: usize,
    pub overall_latency: EndpointLatencyStats,
    #[serde(default)]
    pub endpoints: Vec<EndpointLatencyStats>,
    #[serde(default)]
    pub recent_latency_samples: Vec<CloudLatencySample>,
    pub cache: CloudCacheStats,
    pub memory: CloudMemoryStats,
    pub store: CloudStoreStats,
}

/// Client-side state and rolling snapshot history for `daqcloud` server statistics.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CloudStatsSnapshot {
    pub latest: Option<CloudStatsResponse>,
    pub history: Vec<CloudStatsResponse>,
    pub is_loading: bool,
    pub last_error: Option<String>,
}

impl CloudStatsSnapshot {
    pub const MAX_HISTORY_LEN: usize = 120;

    pub fn record_response(&mut self, resp: CloudStatsResponse) {
        self.is_loading = false;
        self.last_error = None;
        self.history.push(resp.clone());
        if self.history.len() > Self::MAX_HISTORY_LEN {
            let excess = self.history.len() - Self::MAX_HISTORY_LEN;
            self.history.drain(..excess);
        }
        self.latest = Some(resp);
    }

    pub fn record_error(&mut self, err: String) {
        self.is_loading = false;
        self.last_error = Some(err);
    }
}

/// Summary of active background network/storage requests inside a [`crate::router::RouterBackend`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackendLoadingActivity {
    pub catalog_syncing: bool,
    pub metadata_syncing: bool,
    pub stats_fetching: bool,
    pub in_flight_preloads: usize,
    pub pending_downloads: usize,
    pub pending_uploads: usize,
}

impl BackendLoadingActivity {
    pub fn total_active(&self) -> usize {
        usize::from(self.catalog_syncing)
            + usize::from(self.metadata_syncing)
            + usize::from(self.stats_fetching)
            + self.in_flight_preloads
            + self.pending_downloads
            + self.pending_uploads
    }

    pub fn is_active(&self) -> bool {
        self.total_active() > 0
    }
}

/// Events emitted by a `daqcloud` sync worker for [`crate::router::VirtualTimelineRouter`] to drain.
#[derive(Debug, Clone)]
pub enum CloudSyncEvent {
    CatalogSynced {
        track_days: usize,
        partitions: usize,
        annotations: usize,
    },
    OverviewDownloaded {
        partition_id: String,
    },
    PartitionLocalized {
        partition_id: String,
    },
    PartitionUploaded {
        partition_id: String,
    },
    AnnotationsUpdated,
    DerivedSignalsUpdated,
    Error(String),
}

/// Summary returned by a synchronous one-shot cloud sync pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CloudSyncSummary {
    pub remote_track_days: usize,
    pub remote_partitions: usize,
    pub overviews_downloaded: usize,
    pub full_partitions_downloaded: usize,
    pub partitions_uploaded: usize,
    pub annotations_pushed: usize,
    pub annotations_pulled: usize,
}

/// Portable in-memory metadata catalog (used by `InMemoryRouterBackend` and `WebCloudRouterBackend`),
/// implementing the exact same LWW annotation conflict resolution and auto-stint numbering as `Registry`.
#[derive(Debug, Clone)]
pub struct InMemoryCatalog {
    track_days: BTreeMap<String, TrackDay>,
    partitions: BTreeMap<String, RegisteredPartition>,
    annotations: BTreeMap<String, Annotation>,
    derived_signals: BTreeMap<String, DerivedSignalDef>,
}

impl Default for InMemoryCatalog {
    fn default() -> Self {
        let mut derived_signals = BTreeMap::new();
        for def in crate::analysis::FormulaEngine::builtin_defs() {
            derived_signals.insert(def.id.clone(), def);
        }
        Self {
            track_days: BTreeMap::new(),
            partitions: BTreeMap::new(),
            annotations: BTreeMap::new(),
            derived_signals,
        }
    }
}

impl InMemoryCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn upsert_track_day(&mut self, day: &TrackDay) -> TrackDay {
        let default_title = default_track_day_title(&day.id);
        let updated = match self.track_days.get(&day.id) {
            Some(prev) => {
                let title = if day.title.is_empty()
                    || (day.title == default_title && prev.title != default_title)
                {
                    prev.title.clone()
                } else {
                    day.title.clone()
                };
                let location = if day.location.is_empty() {
                    prev.location.clone()
                } else {
                    day.location.clone()
                };
                let (start_utc_ms, end_utc_ms) = if prev.start_utc_ms == 0 && prev.end_utc_ms == 0 {
                    (day.start_utc_ms, day.end_utc_ms.max(day.start_utc_ms))
                } else if day.start_utc_ms == 0 && day.end_utc_ms == 0 {
                    (prev.start_utc_ms, prev.end_utc_ms.max(prev.start_utc_ms))
                } else {
                    let lo = prev.start_utc_ms.min(day.start_utc_ms);
                    let hi = prev.end_utc_ms.max(day.end_utc_ms).max(lo);
                    (lo, hi)
                };
                TrackDay {
                    id: day.id.clone(),
                    title,
                    location,
                    start_utc_ms,
                    end_utc_ms,
                }
            }
            None => {
                let title = if day.title.is_empty() {
                    default_title
                } else {
                    day.title.clone()
                };
                let start_utc_ms = day.start_utc_ms;
                let end_utc_ms = day.end_utc_ms.max(start_utc_ms);
                TrackDay {
                    id: day.id.clone(),
                    title,
                    location: day.location.clone(),
                    start_utc_ms,
                    end_utc_ms,
                }
            }
        };
        self.track_days.insert(day.id.clone(), updated.clone());
        updated
    }

    pub fn set_track_day_label(&mut self, track_day_id: &str, label: &str) -> TrackDay {
        let trimmed = label.trim();
        let title = if trimmed.is_empty() {
            default_track_day_title(track_day_id)
        } else {
            format!("{track_day_id} — {trimmed}")
        };
        let location = trimmed.to_owned();
        let updated = match self.track_days.get(track_day_id) {
            Some(prev) => TrackDay {
                id: prev.id.clone(),
                title,
                location,
                start_utc_ms: prev.start_utc_ms,
                end_utc_ms: prev.end_utc_ms,
            },
            None => TrackDay {
                id: track_day_id.to_owned(),
                title,
                location,
                start_utc_ms: 0,
                end_utc_ms: 0,
            },
        };
        self.track_days
            .insert(track_day_id.to_owned(), updated.clone());
        updated
    }

    pub fn get_track_day(&self, id: &str) -> Option<TrackDay> {
        self.track_days.get(id).cloned()
    }

    pub fn list_track_days(&self) -> Vec<TrackDay> {
        let mut days: Vec<TrackDay> = self.track_days.values().cloned().collect();
        days.sort_by(|a, b| a.start_utc_ms.cmp(&b.start_utc_ms).then(a.id.cmp(&b.id)));
        days
    }

    pub fn register_manifest(&mut self, manifest: &PartitionManifest, is_local: bool) -> bool {
        self.upsert_track_day(&TrackDay {
            id: manifest.track_day_id.clone(),
            title: default_track_day_title(&manifest.track_day_id),
            location: String::new(),
            start_utc_ms: manifest.start_utc_ms,
            end_utc_ms: manifest.end_utc_ms.max(manifest.start_utc_ms),
        });
        let existed = self.partitions.contains_key(&manifest.partition_id);
        let reg = RegisteredPartition::from_manifest(manifest.clone(), is_local);
        self.partitions.insert(manifest.partition_id.clone(), reg);
        for ann in &manifest.annotations {
            self.upsert_annotation_inner(ann, true);
        }
        self.renumber_auto_stints(&manifest.track_day_id);
        !existed
    }

    pub fn get_partition(&self, partition_id: &str) -> Option<RegisteredPartition> {
        self.partitions.get(partition_id).cloned()
    }

    pub fn list_partitions(&self) -> Vec<RegisteredPartition> {
        let mut list: Vec<RegisteredPartition> = self.partitions.values().cloned().collect();
        list.sort_by(|a, b| {
            a.start_utc_ms
                .cmp(&b.start_utc_ms)
                .then_with(|| a.partition_id.cmp(&b.partition_id))
        });
        list
    }

    fn upsert_annotation_inner(&mut self, ann: &Annotation, from_manifest: bool) -> bool {
        if !self.track_days.contains_key(&ann.track_day_id) {
            self.upsert_track_day(&TrackDay {
                id: ann.track_day_id.clone(),
                title: default_track_day_title(&ann.track_day_id),
                location: String::new(),
                start_utc_ms: ann.start_utc_ms,
                end_utc_ms: ann.end_utc_ms.unwrap_or(ann.start_utc_ms),
            });
        }
        let mut candidate = ann.clone();
        if let Some(prev) = self.annotations.get(&ann.id) {
            // Auto-generated manifest annotations never overwrite an existing row.
            if from_manifest && ann.source == "auto" {
                return false;
            }
            if ann.updated_at_ms < prev.updated_at_ms {
                return false;
            }
            if !from_manifest && prev.source == "auto" {
                let user_modified = ann.name != prev.name
                    || ann.notes != prev.notes
                    || ann.category != prev.category
                    || ann.color != prev.color
                    || ann.start_utc_ms != prev.start_utc_ms
                    || ann.end_utc_ms != prev.end_utc_ms;
                if user_modified {
                    candidate.source = "manual".to_owned();
                    if candidate.author == "daqcore" || candidate.author.is_empty() {
                        candidate.author = "user".to_owned();
                    }
                }
            }
        }
        self.annotations.insert(candidate.id.clone(), candidate);
        true
    }

    pub fn upsert_annotation(&mut self, ann: &Annotation) -> bool {
        let changed = self.upsert_annotation_inner(ann, false);
        if changed && ann.category == "driving_stint" {
            self.renumber_auto_stints(&ann.track_day_id);
        }
        changed
    }

    pub fn soft_delete_annotation(&mut self, annotation_id: &str, updated_at_ms: i64) -> bool {
        let Some(existing) = self.annotations.get(annotation_id).cloned() else {
            return false;
        };
        if updated_at_ms < existing.updated_at_ms {
            return false;
        }
        let mut tombstone = existing.clone();
        tombstone.deleted = true;
        tombstone.updated_at_ms = updated_at_ms;
        self.annotations.insert(annotation_id.to_owned(), tombstone);
        if existing.category == "driving_stint" {
            self.renumber_auto_stints(&existing.track_day_id);
        }
        true
    }

    pub fn query_annotations(&self, include_deleted: bool) -> Vec<Annotation> {
        let mut list: Vec<Annotation> = self
            .annotations
            .values()
            .filter(|a| include_deleted || !a.deleted)
            .cloned()
            .collect();
        list.sort_by(|a, b| a.start_utc_ms.cmp(&b.start_utc_ms).then(a.id.cmp(&b.id)));
        list
    }

    pub fn upsert_derived_signal(&mut self, def: &DerivedSignalDef) -> bool {
        let id = def.id.trim();
        if id.is_empty() {
            return false;
        }
        if let Some(prev) = self.derived_signals.get(id)
            && def.updated_at_ms < prev.updated_at_ms
        {
            return false;
        }
        let mut clean = def.clone();
        clean.id = id.to_owned();
        if clean.label.trim().is_empty() {
            clean.label = clean.id.clone();
        }
        self.derived_signals.insert(clean.id.clone(), clean);
        true
    }

    pub fn soft_delete_derived_signal(&mut self, id: &str, updated_at_ms: i64) -> bool {
        let trimmed = id.trim();
        if trimmed.is_empty() {
            return false;
        }
        if let Some(existing) = self.derived_signals.get(trimmed)
            && updated_at_ms < existing.updated_at_ms
        {
            return false;
        }
        let tombstone = match self.derived_signals.get(trimmed) {
            Some(prev) => {
                let mut t = prev.clone();
                t.deleted = true;
                t.updated_at_ms = updated_at_ms;
                t
            }
            None => DerivedSignalDef {
                id: trimmed.to_owned(),
                label: trimmed.to_owned(),
                expression: "0".to_owned(),
                unit: String::new(),
                updated_at_ms,
                deleted: true,
            },
        };
        self.derived_signals.insert(trimmed.to_owned(), tombstone);
        true
    }

    pub fn query_derived_signals(&self, include_deleted: bool) -> Vec<DerivedSignalDef> {
        self.derived_signals
            .values()
            .filter(|d| include_deleted || !d.deleted)
            .cloned()
            .collect()
    }

    pub fn replace_from_catalog(&mut self, catalog: CatalogResponse) {
        for day in &catalog.track_days {
            self.upsert_track_day(day);
        }
        for manifest in &catalog.partitions {
            let is_local = self
                .partitions
                .get(&manifest.partition_id)
                .is_some_and(|p| p.is_local);
            self.register_manifest(manifest, is_local);
        }
        for ann in &catalog.annotations {
            self.upsert_annotation_inner(ann, false);
        }
        for sig in &catalog.derived_signals {
            self.upsert_derived_signal(sig);
        }
        let day_ids: Vec<String> = self.track_days.keys().cloned().collect();
        for day_id in day_ids {
            self.renumber_auto_stints(&day_id);
        }
    }

    fn renumber_auto_stints(&mut self, track_day_id: &str) {
        let mut auto_ids: Vec<(i64, String)> = self
            .annotations
            .values()
            .filter(|a| {
                a.track_day_id == track_day_id
                    && !a.deleted
                    && a.category == "driving_stint"
                    && (a.source == "auto" || a.author == "daqcore")
            })
            .map(|a| (a.start_utc_ms, a.id.clone()))
            .collect();
        auto_ids.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        for (idx, (_, id)) in auto_ids.into_iter().enumerate() {
            let expected = format!("Driving Stint #{}", idx + 1);
            if let Some(ann) = self.annotations.get_mut(&id)
                && is_default_auto_stint_name(&ann.name)
                && ann.name != expected
            {
                ann.name = expected;
            }
        }
    }
}

/// Returns `true` if `name` matches the default auto-numbered stint pattern `"Driving Stint #<N>"`.
pub fn is_default_auto_stint_name(name: &str) -> bool {
    name.strip_prefix("Driving Stint #")
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}
