use crate::state::AppState;
use axum::{
    Json, Router,
    body::Bytes,
    extract::{
        DefaultBodyLimit, Path, Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, put},
};
use daqcore::{
    storage::{
        Annotation, AnnotationPatchRequest, AnnotationUpsertPayload, AssetUploadResponse,
        CHECKSUM_HEADER, CatalogResponse, CloudWsMessage, PartitionManifest, RANGE_CHECKSUM_HEADER,
        Registry, compute_file_sha256_hex, compute_sha256_hex, is_safe_path_segment,
    },
    time::Time,
};
use std::{
    fs,
    io::{Read as _, Seek as _, SeekFrom, Write as _},
    path,
};
use tokio::sync::broadcast::error::RecvError;

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(health_handler))
        .route("/api/v1/catalog", get(get_catalog_handler))
        .route(
            "/api/v1/partitions/{partition_id}/{filename}",
            put(put_partition_file_handler)
                .get(get_partition_file_handler)
                .head(head_partition_file_handler)
                .layer(DefaultBodyLimit::disable()),
        )
        .route(
            "/api/v1/superdbcs/{superdbc_hash}",
            put(put_superdbc_handler)
                .get(get_superdbc_handler)
                .head(head_superdbc_handler)
                .layer(DefaultBodyLimit::disable()),
        )
        .route(
            "/api/v1/annotations",
            get(get_annotations_handler)
                .post(post_annotations_handler)
                .patch(patch_annotations_body_handler)
                .delete(delete_annotation_query_handler),
        )
        .route(
            "/api/v1/annotations/{id}",
            axum::routing::patch(patch_annotation_id_handler).delete(delete_annotation_id_handler),
        )
        .route("/api/v1/ws", get(ws_upgrade_handler))
        .with_state(state)
}

async fn health_handler() -> &'static str {
    "daqcloud\n"
}

async fn get_catalog_handler(
    State(state): State<AppState>,
) -> Result<Json<CatalogResponse>, (StatusCode, String)> {
    let track_days = state
        .registry
        .list_track_days()
        .map_err(internal_registry_err)?;
    let registered_partitions = state
        .registry
        .list_all_partitions()
        .map_err(internal_registry_err)?;
    let partitions = registered_partitions
        .into_iter()
        .map(|p| p.manifest)
        .collect();
    let annotations = state
        .registry
        .list_annotations_updated_since(0)
        .map_err(internal_registry_err)?;

    Ok(Json(CatalogResponse {
        track_days,
        partitions,
        annotations,
    }))
}

fn resolve_existing_asset_path(
    store_dir: &path::Path,
    partition_id: &str,
    filename: &str,
) -> Option<path::PathBuf> {
    let primary = store_dir
        .join("partitions")
        .join(partition_id)
        .join(filename);
    if primary.is_file() {
        return Some(primary);
    }
    if filename.ends_with(".log") {
        let raw_fallback = store_dir.join("raw").join(filename);
        if raw_fallback.is_file() {
            return Some(raw_fallback);
        }
    }
    None
}

async fn head_partition_file_handler(
    State(state): State<AppState>,
    Path((partition_id, filename)): Path<(String, String)>,
) -> Response {
    if !is_safe_path_segment(&partition_id) || !is_safe_path_segment(&filename) {
        return (StatusCode::BAD_REQUEST, "Invalid partition_id or filename").into_response();
    }

    let Some(file_path) = resolve_existing_asset_path(&state.store_dir, &partition_id, &filename)
    else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let (size_bytes, sha256_hex) = match state.cached_file_sha256(&file_path) {
        Ok(res) => res,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to hash asset: {err}"),
            )
                .into_response();
        }
    };

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&size_bytes.to_string()).unwrap(),
    );
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if let Ok(etag) = HeaderValue::from_str(&format!("\"{sha256_hex}\"")) {
        headers.insert(header::ETAG, etag);
    }
    if let Ok(val) = HeaderValue::from_str(&sha256_hex) {
        headers.insert(CHECKSUM_HEADER, val);
    }

    (StatusCode::OK, headers).into_response()
}

async fn get_partition_file_handler(
    State(state): State<AppState>,
    Path((partition_id, filename)): Path<(String, String)>,
    req_headers: HeaderMap,
) -> Response {
    if !is_safe_path_segment(&partition_id) || !is_safe_path_segment(&filename) {
        return (StatusCode::BAD_REQUEST, "Invalid partition_id or filename").into_response();
    }

    let Some(file_path) = resolve_existing_asset_path(&state.store_dir, &partition_id, &filename)
    else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let (total_size, full_sha256) = match state.cached_file_sha256(&file_path) {
        Ok(res) => res,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to read asset metadata: {err}"),
            )
                .into_response();
        }
    };

    let content_type = if filename.ends_with(".json") {
        "application/json"
    } else {
        "application/octet-stream"
    };

    if let Some(range_val) = req_headers.get(header::RANGE).and_then(|v| v.to_str().ok()) {
        let Some((start, end)) = parse_byte_range(range_val, total_size) else {
            let mut headers = HeaderMap::new();
            if let Ok(cr) = HeaderValue::from_str(&format!("bytes */{total_size}")) {
                headers.insert(header::CONTENT_RANGE, cr);
            }
            return (StatusCode::RANGE_NOT_SATISFIABLE, headers).into_response();
        };

        let slice_len = (end - start + 1) as usize;
        let mut buf = Vec::with_capacity(slice_len);
        let read_res = (|| -> std::io::Result<()> {
            let mut file = fs::File::open(&file_path)?;
            file.seek(SeekFrom::Start(start))?;
            file.take(slice_len as u64).read_to_end(&mut buf)?;
            Ok(())
        })();

        if let Err(err) = read_res {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to read byte range: {err}"),
            )
                .into_response();
        }

        let range_sha256 = compute_sha256_hex(&buf);
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
        headers.insert(
            header::CONTENT_LENGTH,
            HeaderValue::from_str(&buf.len().to_string()).unwrap(),
        );
        headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        if let Ok(cr) = HeaderValue::from_str(&format!("bytes {start}-{end}/{total_size}")) {
            headers.insert(header::CONTENT_RANGE, cr);
        }
        if let Ok(etag) = HeaderValue::from_str(&format!("\"{full_sha256}\"")) {
            headers.insert(header::ETAG, etag);
        }
        if let Ok(val) = HeaderValue::from_str(&full_sha256) {
            headers.insert(CHECKSUM_HEADER, val);
        }
        if let Ok(val) = HeaderValue::from_str(&range_sha256) {
            headers.insert(RANGE_CHECKSUM_HEADER, val);
        }

        return (StatusCode::PARTIAL_CONTENT, headers, buf).into_response();
    }

    let bytes = match fs::read(&file_path) {
        Ok(b) => b,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to read file: {err}"),
            )
                .into_response();
        }
    };

    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&bytes.len().to_string()).unwrap(),
    );
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if let Ok(etag) = HeaderValue::from_str(&format!("\"{full_sha256}\"")) {
        headers.insert(header::ETAG, etag);
    }
    if let Ok(val) = HeaderValue::from_str(&full_sha256) {
        headers.insert(CHECKSUM_HEADER, val);
    }

    (StatusCode::OK, headers, bytes).into_response()
}

async fn put_partition_file_handler(
    State(state): State<AppState>,
    Path((partition_id, filename)): Path<(String, String)>,
    req_headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !is_safe_path_segment(&partition_id) || !is_safe_path_segment(&filename) {
        return (StatusCode::BAD_REQUEST, "Invalid partition_id or filename").into_response();
    }

    let part_dir = state.store_dir.join("partitions").join(&partition_id);
    if let Err(err) = fs::create_dir_all(&part_dir) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to create partition directory: {err}"),
        )
            .into_response();
    }

    let target_path = part_dir.join(&filename);
    let expected_sha256 = req_headers
        .get(CHECKSUM_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_ascii_lowercase());

    // Handle resumable chunked upload if `Content-Range: bytes start-end/total` is provided
    let (final_size, final_sha256) = if let Some(content_range_str) = req_headers
        .get(header::CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
    {
        let Some((start, end, total)) = parse_content_range(content_range_str) else {
            return (StatusCode::BAD_REQUEST, "Invalid Content-Range header").into_response();
        };
        if body.len() as u64 != end - start + 1 {
            return (
                StatusCode::BAD_REQUEST,
                "Chunk body length does not match Content-Range span",
            )
                .into_response();
        }

        let part_file_path = part_dir.join(format!(".{filename}.upload.part"));
        let write_res = (|| -> std::io::Result<()> {
            let mut f = fs::OpenOptions::new()
                .create(true)
                .truncate(start == 0)
                .write(true)
                .open(&part_file_path)?;
            f.seek(SeekFrom::Start(start))?;
            f.write_all(&body)?;
            f.flush()?;
            Ok(())
        })();
        if let Err(err) = write_res {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to write upload chunk: {err}"),
            )
                .into_response();
        }

        if end + 1 < total {
            let chunk_sha = compute_sha256_hex(&body);
            return (
                StatusCode::OK,
                Json(AssetUploadResponse {
                    partition_id,
                    filename,
                    size_bytes: end + 1,
                    sha256_hex: chunk_sha,
                }),
            )
                .into_response();
        }

        let (computed_size, computed_sha256) = match compute_file_sha256_hex(&part_file_path) {
            Ok(res) => res,
            Err(err) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to hash assembled file: {err}"),
                )
                    .into_response();
            }
        };
        if let Some(expected) = &expected_sha256
            && expected != &computed_sha256
        {
            let _ = fs::remove_file(&part_file_path);
            return (
                StatusCode::BAD_REQUEST,
                format!("SHA-256 checksum mismatch: expected {expected}, got {computed_sha256}"),
            )
                .into_response();
        }
        if let Err(err) = fs::rename(&part_file_path, &target_path) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to finalize uploaded file: {err}"),
            )
                .into_response();
        }
        (computed_size, computed_sha256)
    } else {
        let computed_sha256 = compute_sha256_hex(&body);
        if let Some(expected) = &expected_sha256
            && expected != &computed_sha256
        {
            return (
                StatusCode::BAD_REQUEST,
                format!("SHA-256 checksum mismatch: expected {expected}, got {computed_sha256}"),
            )
                .into_response();
        }

        if filename == "manifest.json" {
            match serde_json::from_slice::<PartitionManifest>(&body) {
                Ok(manifest) if manifest.partition_id == partition_id => {}
                Ok(manifest) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        format!(
                            "Manifest partition_id '{}' does not match path '{partition_id}'",
                            manifest.partition_id
                        ),
                    )
                        .into_response();
                }
                Err(err) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        format!("Invalid PartitionManifest JSON: {err}"),
                    )
                        .into_response();
                }
            }
        }

        let tmp_path = part_dir.join(format!(".{filename}.tmp"));
        if let Err(err) =
            fs::write(&tmp_path, &body).and_then(|()| fs::rename(&tmp_path, &target_path))
        {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to write asset file: {err}"),
            )
                .into_response();
        }
        (body.len() as u64, computed_sha256)
    };

    if filename.ends_with(".log") {
        let raw_copy = state.store_dir.join("raw").join(&filename);
        if !raw_copy.exists() {
            let _ = fs::copy(&target_path, &raw_copy);
        }
    }

    state.insert_cached_sha256(target_path.clone(), final_size, final_sha256.clone());

    if filename == "manifest.json" {
        let manifest_bytes = match fs::read(&target_path) {
            Ok(b) => b,
            Err(err) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to read manifest.json: {err}"),
                )
                    .into_response();
            }
        };
        let manifest: PartitionManifest = match serde_json::from_slice(&manifest_bytes) {
            Ok(m) => m,
            Err(err) => {
                return (
                    StatusCode::BAD_REQUEST,
                    format!("Invalid PartitionManifest JSON: {err}"),
                )
                    .into_response();
            }
        };
        let is_complete = Registry::is_partition_dir_complete(&part_dir);
        if let Err(err) = state.registry.register_manifest(&manifest, is_complete) {
            return internal_registry_err(err).into_response();
        }
        let _ = state
            .ws_tx
            .send(CloudWsMessage::PartitionAvailable { manifest });
    } else if Registry::is_partition_dir_complete(&part_dir) {
        let _ = state.registry.set_partition_local(&partition_id, true);
    }

    let mut headers = HeaderMap::new();
    if let Ok(etag) = HeaderValue::from_str(&format!("\"{final_sha256}\"")) {
        headers.insert(header::ETAG, etag);
    }
    if let Ok(val) = HeaderValue::from_str(&final_sha256) {
        headers.insert(CHECKSUM_HEADER, val);
    }

    (
        StatusCode::OK,
        headers,
        Json(AssetUploadResponse {
            partition_id,
            filename,
            size_bytes: final_size,
            sha256_hex: final_sha256,
        }),
    )
        .into_response()
}

fn normalize_superdbc_hash(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    let hash = trimmed.strip_suffix(".json").unwrap_or(trimmed);
    if hash.is_empty() || !is_safe_path_segment(hash) {
        return None;
    }
    Some(hash)
}

async fn head_superdbc_handler(
    State(state): State<AppState>,
    Path(raw_hash): Path<String>,
) -> Response {
    let Some(hash) = normalize_superdbc_hash(&raw_hash) else {
        return (StatusCode::BAD_REQUEST, "Invalid superdbc_hash").into_response();
    };
    let file_path = state
        .store_dir
        .join("superdbcs")
        .join(format!("{hash}.json"));
    if !file_path.is_file() {
        return StatusCode::NOT_FOUND.into_response();
    }

    let (size_bytes, sha256_hex) = match state.cached_file_sha256(&file_path) {
        Ok(res) => res,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to hash SuperDBC: {err}"),
            )
                .into_response();
        }
    };

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&size_bytes.to_string()).unwrap(),
    );
    if let Ok(etag) = HeaderValue::from_str(&format!("\"{sha256_hex}\"")) {
        headers.insert(header::ETAG, etag);
    }
    if let Ok(val) = HeaderValue::from_str(&sha256_hex) {
        headers.insert(CHECKSUM_HEADER, val);
    }

    (StatusCode::OK, headers).into_response()
}

async fn get_superdbc_handler(
    State(state): State<AppState>,
    Path(raw_hash): Path<String>,
) -> Response {
    let Some(hash) = normalize_superdbc_hash(&raw_hash) else {
        return (StatusCode::BAD_REQUEST, "Invalid superdbc_hash").into_response();
    };
    let file_path = state
        .store_dir
        .join("superdbcs")
        .join(format!("{hash}.json"));
    if !file_path.is_file() {
        return StatusCode::NOT_FOUND.into_response();
    }

    let (_, full_sha256) = match state.cached_file_sha256(&file_path) {
        Ok(res) => res,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to hash SuperDBC: {err}"),
            )
                .into_response();
        }
    };

    let bytes = match fs::read(&file_path) {
        Ok(b) => b,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to read SuperDBC: {err}"),
            )
                .into_response();
        }
    };

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&bytes.len().to_string()).unwrap(),
    );
    if let Ok(etag) = HeaderValue::from_str(&format!("\"{full_sha256}\"")) {
        headers.insert(header::ETAG, etag);
    }
    if let Ok(val) = HeaderValue::from_str(&full_sha256) {
        headers.insert(CHECKSUM_HEADER, val);
    }

    (StatusCode::OK, headers, bytes).into_response()
}

async fn put_superdbc_handler(
    State(state): State<AppState>,
    Path(raw_hash): Path<String>,
    req_headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(hash) = normalize_superdbc_hash(&raw_hash) else {
        return (StatusCode::BAD_REQUEST, "Invalid superdbc_hash").into_response();
    };

    let expected_sha256 = req_headers
        .get(CHECKSUM_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_ascii_lowercase());
    let computed_sha256 = compute_sha256_hex(&body);
    if let Some(expected) = &expected_sha256
        && expected != &computed_sha256
    {
        return (
            StatusCode::BAD_REQUEST,
            format!("SHA-256 checksum mismatch: expected {expected}, got {computed_sha256}"),
        )
            .into_response();
    }

    let Ok(json_str) = std::str::from_utf8(&body) else {
        return (StatusCode::BAD_REQUEST, "SuperDBC body must be valid UTF-8").into_response();
    };
    let db = match daqcore::superdbc::database::Database::from_json(json_str) {
        Ok(db) => db,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("Invalid SuperDBC JSON: {err}"),
            )
                .into_response();
        }
    };
    if db.content_hash() != hash {
        return (
            StatusCode::BAD_REQUEST,
            format!(
                "SuperDBC content_hash '{}' does not match path '{hash}'",
                db.content_hash()
            ),
        )
            .into_response();
    }

    let superdbcs_dir = state.store_dir.join("superdbcs");
    if let Err(err) = fs::create_dir_all(&superdbcs_dir) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to create superdbcs directory: {err}"),
        )
            .into_response();
    }
    let filename = format!("{hash}.json");
    let target_path = superdbcs_dir.join(&filename);
    let tmp_path = superdbcs_dir.join(format!(".{filename}.tmp"));
    if let Err(err) = fs::write(&tmp_path, &body).and_then(|()| fs::rename(&tmp_path, &target_path))
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to write SuperDBC file: {err}"),
        )
            .into_response();
    }

    let final_size = body.len() as u64;
    state.insert_cached_sha256(target_path, final_size, computed_sha256.clone());

    let mut headers = HeaderMap::new();
    if let Ok(etag) = HeaderValue::from_str(&format!("\"{computed_sha256}\"")) {
        headers.insert(header::ETAG, etag);
    }
    if let Ok(val) = HeaderValue::from_str(&computed_sha256) {
        headers.insert(CHECKSUM_HEADER, val);
    }

    (
        StatusCode::OK,
        headers,
        Json(AssetUploadResponse {
            partition_id: "superdbcs".to_owned(),
            filename,
            size_bytes: final_size,
            sha256_hex: computed_sha256,
        }),
    )
        .into_response()
}

#[derive(Debug, Default, serde::Deserialize)]
struct AnnotationsQuery {
    #[serde(default)]
    track_day_id: Option<String>,
    #[serde(default)]
    since_utc_ms: Option<i64>,
    #[serde(default)]
    include_deleted: Option<bool>,
}

async fn get_annotations_handler(
    State(state): State<AppState>,
    Query(query): Query<AnnotationsQuery>,
) -> Result<Json<Vec<Annotation>>, (StatusCode, String)> {
    let include_deleted = query.include_deleted.unwrap_or(false);
    let list = if let Some(since_ms) = query.since_utc_ms {
        let mut anns = state
            .registry
            .list_annotations_updated_since(since_ms)
            .map_err(internal_registry_err)?;
        if let Some(day_id) = &query.track_day_id {
            anns.retain(|a| &a.track_day_id == day_id);
        }
        if !include_deleted && query.include_deleted.is_some() {
            anns.retain(|a| !a.deleted);
        }
        anns
    } else if let Some(day_id) = &query.track_day_id {
        state
            .registry
            .list_annotations_for_day(day_id, include_deleted)
            .map_err(internal_registry_err)?
    } else {
        state
            .registry
            .query_annotations_in_range(i64::MIN, i64::MAX, include_deleted)
            .map_err(internal_registry_err)?
    };
    Ok(Json(list))
}

async fn post_annotations_handler(
    State(state): State<AppState>,
    Json(payload): Json<AnnotationUpsertPayload>,
) -> Result<Json<Vec<Annotation>>, (StatusCode, String)> {
    let incoming = payload.into_vec();
    let mut stored_list = Vec::with_capacity(incoming.len());

    for ann in incoming {
        let changed = state
            .registry
            .upsert_annotation(&ann)
            .map_err(internal_registry_err)?;
        let stored = state
            .registry
            .get_annotation(&ann.id)
            .map_err(internal_registry_err)?
            .unwrap_or(ann);
        if changed {
            if stored.deleted {
                let _ = state.ws_tx.send(CloudWsMessage::AnnotationDeleted {
                    annotation_id: stored.id.clone(),
                    updated_at_ms: stored.updated_at_ms,
                });
            }
            let _ = state.ws_tx.send(CloudWsMessage::AnnotationUpdated {
                annotation: stored.clone(),
            });
        }
        stored_list.push(stored);
    }

    Ok(Json(stored_list))
}

async fn patch_annotations_body_handler(
    State(state): State<AppState>,
    Json(patch): Json<AnnotationPatchRequest>,
) -> Result<Json<Annotation>, (StatusCode, String)> {
    let Some(id) = patch.id.clone() else {
        return Err((
            StatusCode::BAD_REQUEST,
            "PATCH /api/v1/annotations requires 'id' field in body".to_owned(),
        ));
    };
    apply_annotation_patch(&state, &id, patch)
}

async fn patch_annotation_id_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(patch): Json<AnnotationPatchRequest>,
) -> Result<Json<Annotation>, (StatusCode, String)> {
    apply_annotation_patch(&state, &id, patch)
}

fn apply_annotation_patch(
    state: &AppState,
    id: &str,
    patch: AnnotationPatchRequest,
) -> Result<Json<Annotation>, (StatusCode, String)> {
    let Some(existing) = state
        .registry
        .get_annotation(id)
        .map_err(internal_registry_err)?
    else {
        return Err((
            StatusCode::NOT_FOUND,
            format!("Annotation '{id}' not found"),
        ));
    };

    let now_ms = Time::now().unix_millis();
    let updated = patch.apply_to(&existing, now_ms);
    let changed = state
        .registry
        .upsert_annotation(&updated)
        .map_err(internal_registry_err)?;
    let stored = state
        .registry
        .get_annotation(id)
        .map_err(internal_registry_err)?
        .unwrap_or(updated);

    if changed {
        if stored.deleted {
            let _ = state.ws_tx.send(CloudWsMessage::AnnotationDeleted {
                annotation_id: stored.id.clone(),
                updated_at_ms: stored.updated_at_ms,
            });
        }
        let _ = state.ws_tx.send(CloudWsMessage::AnnotationUpdated {
            annotation: stored.clone(),
        });
    }

    Ok(Json(stored))
}

#[derive(Debug, Default, serde::Deserialize)]
struct DeleteAnnotationQuery {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    updated_at_ms: Option<i64>,
}

async fn delete_annotation_query_handler(
    State(state): State<AppState>,
    Query(query): Query<DeleteAnnotationQuery>,
) -> Result<Json<Annotation>, (StatusCode, String)> {
    let Some(id) = query.id else {
        return Err((
            StatusCode::BAD_REQUEST,
            "DELETE /api/v1/annotations requires '?id=<annotation_id>'".to_owned(),
        ));
    };
    soft_delete_and_broadcast(&state, &id, query.updated_at_ms)
}

async fn delete_annotation_id_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<DeleteAnnotationQuery>,
) -> Result<Json<Annotation>, (StatusCode, String)> {
    soft_delete_and_broadcast(&state, &id, query.updated_at_ms)
}

fn soft_delete_and_broadcast(
    state: &AppState,
    id: &str,
    requested_updated_at_ms: Option<i64>,
) -> Result<Json<Annotation>, (StatusCode, String)> {
    let Some(existing) = state
        .registry
        .get_annotation(id)
        .map_err(internal_registry_err)?
    else {
        return Err((
            StatusCode::NOT_FOUND,
            format!("Annotation '{id}' not found"),
        ));
    };

    let now_ms = Time::now().unix_millis();
    let effective_updated_at_ms = requested_updated_at_ms
        .unwrap_or(now_ms)
        .max(existing.updated_at_ms.saturating_add(1));

    state
        .registry
        .soft_delete_annotation(id, effective_updated_at_ms)
        .map_err(internal_registry_err)?;

    let stored = state
        .registry
        .get_annotation(id)
        .map_err(internal_registry_err)?
        .ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Deleted annotation missing from registry".to_owned(),
            )
        })?;

    let _ = state.ws_tx.send(CloudWsMessage::AnnotationDeleted {
        annotation_id: stored.id.clone(),
        updated_at_ms: stored.updated_at_ms,
    });
    let _ = state.ws_tx.send(CloudWsMessage::AnnotationUpdated {
        annotation: stored.clone(),
    });

    Ok(Json(stored))
}

async fn ws_upgrade_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws_client(socket, state))
}

async fn handle_ws_client(mut socket: WebSocket, state: AppState) {
    let mut rx = state.ws_tx.subscribe();

    loop {
        tokio::select! {
            broadcast_msg = rx.recv() => {
                match broadcast_msg {
                    Ok(event) => {
                        let Ok(json) = serde_json::to_string(&event) else {
                            continue;
                        };
                        if socket.send(Message::Text(json.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(RecvError::Lagged(skipped)) => {
                        log::warn!("WebSocket client lagged by {skipped} messages");
                    }
                    Err(RecvError::Closed) => break,
                }
            }
            incoming = socket.recv() => {
                let Some(Ok(msg)) = incoming else {
                    break;
                };
                match msg {
                    Message::Text(text) => {
                        if let Ok(ws_msg) = serde_json::from_str::<CloudWsMessage>(&text) {
                            match ws_msg {
                                CloudWsMessage::AnnotationUpdated { annotation } => {
                                    if state.registry.upsert_annotation(&annotation).unwrap_or(false)
                                        && let Ok(Some(stored)) = state.registry.get_annotation(&annotation.id)
                                    {
                                        let _ = state.ws_tx.send(CloudWsMessage::AnnotationUpdated {
                                            annotation: stored,
                                        });
                                    }
                                }
                                CloudWsMessage::AnnotationDeleted { annotation_id, updated_at_ms } => {
                                    if state.registry.soft_delete_annotation(&annotation_id, updated_at_ms).unwrap_or(false) {
                                        let _ = state.ws_tx.send(CloudWsMessage::AnnotationDeleted {
                                            annotation_id: annotation_id.clone(),
                                            updated_at_ms,
                                        });
                                        if let Ok(Some(stored)) = state.registry.get_annotation(&annotation_id) {
                                            let _ = state.ws_tx.send(CloudWsMessage::AnnotationUpdated {
                                                annotation: stored,
                                            });
                                        }
                                    }
                                }
                                CloudWsMessage::Ping { utc_ms } => {
                                    if let Ok(pong) = serde_json::to_string(&CloudWsMessage::Ping { utc_ms })
                                        && socket.send(Message::Text(pong.into())).await.is_err()
                                    {
                                        break;
                                    }
                                }
                                CloudWsMessage::PartitionAvailable { .. } => {}
                            }
                        }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        }
    }
}

fn internal_registry_err(err: daqcore::storage::RegistryError) -> (StatusCode, String) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("Registry error: {err}"),
    )
}

/// Parse an HTTP `Range: bytes=start-end` header against `total_len`, returning inclusive
/// `(start, end)` byte offsets.
pub fn parse_byte_range(range_header: &str, total_len: u64) -> Option<(u64, u64)> {
    if total_len == 0 {
        return None;
    }
    let spec = range_header.trim().strip_prefix("bytes=")?;
    let first_range = spec.split(',').next()?.trim();
    let (start_str, end_str) = first_range.split_once('-')?;
    let start_str = start_str.trim();
    let end_str = end_str.trim();

    if start_str.is_empty() {
        let suffix_len: u64 = end_str.parse().ok()?;
        if suffix_len == 0 {
            return None;
        }
        let start = total_len.saturating_sub(suffix_len);
        let end = total_len - 1;
        Some((start, end))
    } else {
        let start: u64 = start_str.parse().ok()?;
        if start >= total_len {
            return None;
        }
        let end = if end_str.is_empty() {
            total_len - 1
        } else {
            end_str.parse::<u64>().ok()?.min(total_len - 1)
        };
        if start > end {
            return None;
        }
        Some((start, end))
    }
}

/// Parse an HTTP `Content-Range: bytes start-end/total` header into `(start, end, total)`.
pub fn parse_content_range(header_val: &str) -> Option<(u64, u64, u64)> {
    let rest = header_val.trim().strip_prefix("bytes ")?;
    let (span_str, total_str) = rest.split_once('/')?;
    let (start_str, end_str) = span_str.split_once('-')?;
    let start: u64 = start_str.trim().parse().ok()?;
    let end: u64 = end_str.trim().parse().ok()?;
    let total: u64 = total_str.trim().parse().ok()?;
    if start > end || end >= total || total == 0 {
        return None;
    }
    Some((start, end, total))
}

#[cfg(test)]
mod tests {
    use super::*;
    use daqcore::{
        log_parse::{
            correlate::{ClockConfidence, ClockMapping},
            parse::SourceLogFile,
        },
        storage::{SignalManifestEntry, http_base_to_ws_url},
    };
    use futures_util::StreamExt as _;
    use std::time::Duration;

    async fn spawn_test_server() -> (String, tempfile::TempDir, tokio::task::JoinHandle<()>) {
        let tmp = tempfile::tempdir().expect("temp dir");
        let db_path = tmp.path().join("cloud_registry.sqlite");
        let store_dir = tmp.path().join("cloud_store");
        let state = AppState::open(&db_path, &store_dir).expect("init AppState");
        let app = build_router(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://{addr}");
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (base_url, tmp, handle)
    }

    fn sample_manifest(partition_id: &str, track_day_id: &str) -> PartitionManifest {
        PartitionManifest {
            partition_id: partition_id.to_owned(),
            track_day_id: track_day_id.to_owned(),
            start_utc_ms: 1_794_500_000_000,
            end_utc_ms: 1_794_500_060_000,
            clock_mapping: ClockMapping {
                slope: 1.0,
                intercept_ms: 1_794_500_000_000.0,
                confidence: ClockConfidence::RtcEstimated,
            },
            superdbc_hash: "deadbeef".to_owned(),
            raw_log_hashes: vec![SourceLogFile {
                file_name: "log-2026-09-06--18-36-02.log".to_owned(),
                sha256_hex: "0123456789abcdef".to_owned(),
                rtc_timestamp_ms: Some(1_794_500_000_000),
                mtime_unix_ms: 1_794_500_000_000,
            }],
            signals: vec![SignalManifestEntry {
                signal_id: "imu.accel_y_g".to_owned(),
                unit: "g".to_owned(),
                is_discrete: false,
                bus_name: "VCAN".to_owned(),
                bus_id: 0,
                can_id: 100,
                transmitter: "MAIN".to_owned(),
                nominal_period_ms: Some(10),
                description: "Lat accel".to_owned(),
                sample_count: 6000,
                min_utc_ms: 1_794_500_000_000,
                max_utc_ms: 1_794_500_060_000,
                min_val: -1.8,
                max_val: 1.9,
            }],
            annotations: vec![Annotation {
                id: "auto_stint_1".to_owned(),
                track_day_id: track_day_id.to_owned(),
                name: "Driving Stint #1".to_owned(),
                category: "driving_stint".to_owned(),
                color: "#22c55e".to_owned(),
                start_utc_ms: 1_794_500_005_000,
                end_utc_ms: Some(1_794_500_055_000),
                author: "daqcore".to_owned(),
                source: "auto".to_owned(),
                notes: String::new(),
                updated_at_ms: 100,
                deleted: false,
            }],
        }
    }

    #[test]
    fn test_byte_range_and_content_range_parsers() {
        assert_eq!(parse_byte_range("bytes=0-99", 1000), Some((0, 99)));
        assert_eq!(parse_byte_range("bytes=500-", 1000), Some((500, 999)));
        assert_eq!(parse_byte_range("bytes=-200", 1000), Some((800, 999)));
        assert_eq!(parse_byte_range("bytes=0-5000", 1000), Some((0, 999)));
        assert_eq!(parse_byte_range("bytes=1000-1005", 1000), None);
        assert_eq!(parse_byte_range("bytes=500-400", 1000), None);
        assert_eq!(parse_byte_range("bytes=0-0", 0), None);

        assert_eq!(
            parse_content_range("bytes 0-511/1024"),
            Some((0, 511, 1024))
        );
        assert_eq!(
            parse_content_range("bytes 512-1023/1024"),
            Some((512, 1023, 1024))
        );
        assert_eq!(parse_content_range("bytes 512-1024/1024"), None);
    }

    #[tokio::test]
    async fn test_daqcloud_catalog_partitions_range_annotations_and_ws() {
        let (base_url, _tmp, server_handle) = spawn_test_server().await;
        let client = reqwest::Client::new();

        // Connect a WebSocket subscriber upfront to verify real-time broadcasts
        let ws_url = http_base_to_ws_url(&base_url);
        let (mut ws_stream, _) = tokio_tungstenite::connect_async(&ws_url)
            .await
            .expect("connect to /api/v1/ws");

        // 1. Upload partition files (including a multi-chunk resumable PUT for full_res.parquet)
        let part_id = "epoch_1794500000000_aabbccddeeff";
        let overview_1hz_bytes = b"PAR1_overview_1hz_payload_PAR1";
        let overview_1hz_sha = compute_sha256_hex(overview_1hz_bytes);

        // Verify corrupted checksum is rejected with 400 Bad Request
        let bad_put = client
            .put(format!(
                "{base_url}/api/v1/partitions/{part_id}/overview_1hz.parquet"
            ))
            .header(
                CHECKSUM_HEADER,
                "0000000000000000000000000000000000000000000000000000000000000000",
            )
            .body(overview_1hz_bytes.to_vec())
            .send()
            .await
            .unwrap();
        assert_eq!(bad_put.status(), StatusCode::BAD_REQUEST);

        // Upload valid overview_1hz.parquet
        let ok_put = client
            .put(format!(
                "{base_url}/api/v1/partitions/{part_id}/overview_1hz.parquet"
            ))
            .header(CHECKSUM_HEADER, &overview_1hz_sha)
            .body(overview_1hz_bytes.to_vec())
            .send()
            .await
            .unwrap();
        assert_eq!(ok_put.status(), StatusCode::OK);
        let upload_meta: AssetUploadResponse = ok_put.json().await.unwrap();
        assert_eq!(upload_meta.sha256_hex, overview_1hz_sha);
        assert_eq!(upload_meta.size_bytes, overview_1hz_bytes.len() as u64);

        // Upload states.parquet and overview_10hz.parquet
        for (fname, content) in [
            ("states.parquet", b"PAR1_states_PAR1".as_slice()),
            ("overview_10hz.parquet", b"PAR1_10hz_PAR1".as_slice()),
        ] {
            let res = client
                .put(format!("{base_url}/api/v1/partitions/{part_id}/{fname}"))
                .header(CHECKSUM_HEADER, compute_sha256_hex(content))
                .body(content.to_vec())
                .send()
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::OK);
        }

        // Upload full_res.parquet in two resumable Content-Range chunks
        let full_res_bytes: Vec<u8> = (0u32..4096).map(|i| (i % 251) as u8).collect();
        let full_res_sha = compute_sha256_hex(&full_res_bytes);
        let chunk1 = &full_res_bytes[..2000];
        let chunk2 = &full_res_bytes[2000..];

        let c1_res = client
            .put(format!(
                "{base_url}/api/v1/partitions/{part_id}/full_res.parquet"
            ))
            .header("Content-Range", "bytes 0-1999/4096")
            .body(chunk1.to_vec())
            .send()
            .await
            .unwrap();
        assert_eq!(c1_res.status(), StatusCode::OK);

        let c2_res = client
            .put(format!(
                "{base_url}/api/v1/partitions/{part_id}/full_res.parquet"
            ))
            .header("Content-Range", "bytes 2000-4095/4096")
            .header(CHECKSUM_HEADER, &full_res_sha)
            .body(chunk2.to_vec())
            .send()
            .await
            .unwrap();
        assert_eq!(c2_res.status(), StatusCode::OK);

        // Upload manifest.json and verify WebSocket broadcasts PartitionAvailable
        let manifest = sample_manifest(part_id, "2026-09-06");
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        let manifest_res = client
            .put(format!(
                "{base_url}/api/v1/partitions/{part_id}/manifest.json"
            ))
            .header(CHECKSUM_HEADER, compute_sha256_hex(&manifest_bytes))
            .body(manifest_bytes)
            .send()
            .await
            .unwrap();
        assert_eq!(manifest_res.status(), StatusCode::OK);

        let ws_frame = tokio::time::timeout(Duration::from_secs(2), ws_stream.next())
            .await
            .expect("ws timeout")
            .expect("ws frame")
            .expect("ws ok");
        let ws_event: CloudWsMessage =
            serde_json::from_str(ws_frame.to_text().unwrap()).expect("parse ws event");
        assert_eq!(
            ws_event,
            CloudWsMessage::PartitionAvailable {
                manifest: manifest.clone()
            }
        );

        // 2. Verify HEAD and HTTP Range GET on full_res.parquet
        let head_res = client
            .head(format!(
                "{base_url}/api/v1/partitions/{part_id}/full_res.parquet"
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(head_res.status(), StatusCode::OK);
        assert_eq!(
            head_res
                .headers()
                .get(CHECKSUM_HEADER)
                .unwrap()
                .to_str()
                .unwrap(),
            full_res_sha
        );
        assert_eq!(
            head_res
                .headers()
                .get(header::CONTENT_LENGTH)
                .unwrap()
                .to_str()
                .unwrap(),
            "4096"
        );

        let range_res = client
            .get(format!(
                "{base_url}/api/v1/partitions/{part_id}/full_res.parquet"
            ))
            .header(header::RANGE, "bytes=1000-2499")
            .send()
            .await
            .unwrap();
        assert_eq!(range_res.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            range_res
                .headers()
                .get(header::CONTENT_RANGE)
                .unwrap()
                .to_str()
                .unwrap(),
            "bytes 1000-2499/4096"
        );
        let range_body = range_res.bytes().await.unwrap();
        assert_eq!(range_body.as_ref(), &full_res_bytes[1000..=2499]);

        // 3. Verify GET /api/v1/catalog returns track_days, partitions, and annotations
        let catalog: CatalogResponse = client
            .get(format!("{base_url}/api/v1/catalog"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(catalog.track_days.len(), 1);
        assert_eq!(catalog.track_days[0].id, "2026-09-06");
        assert_eq!(catalog.partitions.len(), 1);
        assert_eq!(catalog.partitions[0].partition_id, part_id);
        assert_eq!(catalog.annotations.len(), 1);

        // 4. Verify POST / PATCH / DELETE /api/v1/annotations and real-time WebSocket fanout
        let new_ann = Annotation {
            id: "manual_pit_1".to_owned(),
            track_day_id: "2026-09-06".to_owned(),
            name: "Autocross Run 1".to_owned(),
            category: "event".to_owned(),
            color: "#38bdf8".to_owned(),
            start_utc_ms: 1_794_500_010_000,
            end_utc_ms: Some(1_794_500_045_000),
            author: "vd_lead".to_owned(),
            source: "manual".to_owned(),
            notes: "Fresh Hoosiers".to_owned(),
            updated_at_ms: 500,
            deleted: false,
        };
        let post_res = client
            .post(format!("{base_url}/api/v1/annotations"))
            .json(&new_ann)
            .send()
            .await
            .unwrap();
        assert_eq!(post_res.status(), StatusCode::OK);

        let ws_ann_frame = tokio::time::timeout(Duration::from_secs(2), ws_stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let ws_ann_event: CloudWsMessage =
            serde_json::from_str(ws_ann_frame.to_text().unwrap()).unwrap();
        assert_eq!(
            ws_ann_event,
            CloudWsMessage::AnnotationUpdated {
                annotation: new_ann.clone()
            }
        );

        // PATCH annotation name & notes
        let patch_res: Annotation = client
            .patch(format!("{base_url}/api/v1/annotations/manual_pit_1"))
            .json(&AnnotationPatchRequest {
                name: Some("Autocross Run 1 (Fastest)".to_owned()),
                updated_at_ms: Some(600),
                ..Default::default()
            })
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(patch_res.name, "Autocross Run 1 (Fastest)");
        assert_eq!(patch_res.updated_at_ms, 600);

        let _ = tokio::time::timeout(Duration::from_secs(2), ws_stream.next())
            .await
            .unwrap();

        // DELETE annotation and verify soft-delete tombstone
        let del_res: Annotation = client
            .delete(format!(
                "{base_url}/api/v1/annotations/manual_pit_1?updated_at_ms=700"
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(del_res.deleted);
        assert_eq!(del_res.updated_at_ms, 700);

        let ws_del_frame = tokio::time::timeout(Duration::from_secs(2), ws_stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let ws_del_event: CloudWsMessage =
            serde_json::from_str(ws_del_frame.to_text().unwrap()).unwrap();
        assert_eq!(
            ws_del_event,
            CloudWsMessage::AnnotationDeleted {
                annotation_id: "manual_pit_1".to_owned(),
                updated_at_ms: 700,
            }
        );

        let _ = ws_stream.close(None).await;
        server_handle.abort();
    }
}
