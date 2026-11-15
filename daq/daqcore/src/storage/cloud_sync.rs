use crate::storage::{Annotation, PartitionManifest, TrackDay};
use sha2::Digest as _;
use std::{fs, io, io::Read as _, path};

pub const CHECKSUM_HEADER: &str = "x-checksum-sha256";
pub const RANGE_CHECKSUM_HEADER: &str = "x-range-checksum-sha256";

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

/// Flexible request payload for `POST /api/v1/annotations` accepting either a single
/// [`Annotation`] or a batch of [`Annotation`]s.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum AnnotationUpsertPayload {
    Single(Annotation),
    Batch(Vec<Annotation>),
}

impl AnnotationUpsertPayload {
    pub fn into_vec(self) -> Vec<Annotation> {
        match self {
            Self::Single(ann) => vec![ann],
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
    /// Apply this patch to an existing [`Annotation`], updating `updated_at_ms` to at least
    /// `fallback_now_ms` (and strictly greater than `existing.updated_at_ms` if not specified).
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

/// Real-time WebSocket message broadcast over `/api/v1/ws` between `daqcloud` and `daqapp` clients.
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

/// Stream a file from disk to compute its `(size_bytes, sha256_hex)` without loading the entire
/// file into a single contiguous allocation.
pub fn compute_file_sha256_hex(path: &path::Path) -> io::Result<(u64, String)> {
    let mut file = fs::File::open(path)?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut total_bytes = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total_bytes = total_bytes.saturating_add(n as u64);
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{b:02x}");
    }
    Ok((total_bytes, out))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sha256_and_url_helpers() {
        let digest = compute_sha256_hex(b"per-daq-2026");
        assert_eq!(digest.len(), 64);
        assert_eq!(digest, compute_sha256_hex(b"per-daq-2026"));
        assert_ne!(digest, compute_sha256_hex(b"per-daq-2027"));

        assert!(is_safe_path_segment("epoch_1794500000000_a1b2c3d4e5f6"));
        assert!(is_safe_path_segment("full_res.parquet"));
        assert!(is_safe_path_segment("log-2026-09-06--18-36-02.log"));
        assert!(!is_safe_path_segment(""));
        assert!(!is_safe_path_segment(".."));
        assert!(!is_safe_path_segment("../secret"));
        assert!(!is_safe_path_segment("sub/file.parquet"));
        assert!(!is_safe_path_segment("sub\\file.parquet"));

        assert_eq!(
            normalize_http_base_url("127.0.0.1:3000/"),
            "http://127.0.0.1:3000"
        );
        assert_eq!(
            http_base_to_ws_url("http://127.0.0.1:3000/"),
            "ws://127.0.0.1:3000/api/v1/ws"
        );
        assert_eq!(
            http_base_to_ws_url("https://daq.per.racing"),
            "wss://daq.per.racing/api/v1/ws"
        );
    }
}
