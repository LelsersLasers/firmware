//! Portable storage, manifest, annotation, and cloud sync protocol types.

pub use crate::log_parse::auto_annotate::Annotation;
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
        let relative_path = format!("partitions/{}", manifest.partition_id);
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
}

/// Metadata payload returned by `PUT /api/v1/partitions/{partition_id}/{filename}`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AssetUploadResponse {
    pub partition_id: String,
    pub filename: String,
    pub size_bytes: u64,
    pub sha256_hex: String,
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
    SetActiveTrackDay {
        track_day_id: Option<String>,
    },
    Shutdown,
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
#[derive(Debug, Clone, Default)]
pub struct InMemoryCatalog {
    track_days: BTreeMap<String, TrackDay>,
    partitions: BTreeMap<String, RegisteredPartition>,
    annotations: BTreeMap<String, Annotation>,
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
                let start_utc_ms = prev.start_utc_ms.min(day.start_utc_ms);
                let end_utc_ms = prev.end_utc_ms.max(day.end_utc_ms).max(start_utc_ms);
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
            self.upsert_annotation_no_renumber(ann);
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

    fn upsert_annotation_no_renumber(&mut self, ann: &Annotation) -> bool {
        if !self.track_days.contains_key(&ann.track_day_id) {
            self.upsert_track_day(&TrackDay {
                id: ann.track_day_id.clone(),
                title: default_track_day_title(&ann.track_day_id),
                location: String::new(),
                start_utc_ms: ann.start_utc_ms,
                end_utc_ms: ann.end_utc_ms.unwrap_or(ann.start_utc_ms),
            });
        }
        if let Some(prev) = self.annotations.get(&ann.id)
            && ann.updated_at_ms < prev.updated_at_ms
        {
            return false;
        }
        self.annotations.insert(ann.id.clone(), ann.clone());
        true
    }

    pub fn upsert_annotation(&mut self, ann: &Annotation) -> bool {
        let changed = self.upsert_annotation_no_renumber(ann);
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
            self.upsert_annotation_no_renumber(ann);
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
                && ann.name != expected
            {
                ann.name = expected;
            }
        }
    }
}
