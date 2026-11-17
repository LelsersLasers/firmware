use crate::{
    router::RepaintCallback,
    storage::{Annotation, DerivedSignalDef, PartitionManifest, Registry, TrackDay},
};
use futures_util::StreamExt as _;
use sha2::Digest as _;
use std::{
    collections::{HashMap, HashSet},
    fs, io,
    io::{Read as _, Seek as _, Write as _},
    path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread, time,
};
use tokio::sync::mpsc as tokio_mpsc;

pub use daqcore::storage::{
    AnnotationPatchRequest, AnnotationUpsertPayload, AssetUploadResponse, CHECKSUM_HEADER,
    CatalogResponse, CloudSyncCommand, CloudSyncEvent, CloudSyncStatus, CloudSyncSummary,
    CloudWsMessage, DerivedSignalUpsertPayload, RANGE_CHECKSUM_HEADER, compute_sha256_hex,
    http_base_to_ws_url, is_safe_path_segment, normalize_http_base_url,
};

const UPLOAD_CHUNK_BYTES: usize = 4 * 1024 * 1024;

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

#[derive(Default)]
struct WorkerMetrics {
    connected: bool,
    ws_connected: bool,
    synced_partitions: usize,
    pending_downloads: usize,
    pending_uploads: usize,
    last_error: Option<String>,
}

impl WorkerMetrics {
    fn to_status(&self, url: &str) -> CloudSyncStatus {
        if let Some(err) = &self.last_error
            && !self.connected
            && !self.ws_connected
        {
            return CloudSyncStatus::Error {
                url: url.to_owned(),
                error: err.clone(),
            };
        }
        if self.connected || self.ws_connected {
            CloudSyncStatus::Connected {
                url: url.to_owned(),
                synced_partitions: self.synced_partitions,
                pending_downloads: self.pending_downloads,
                pending_uploads: self.pending_uploads,
                ws_connected: self.ws_connected,
            }
        } else {
            CloudSyncStatus::Connecting {
                url: url.to_owned(),
            }
        }
    }
}

/// Handle to the non-blocking background `daqcloud` sync & WebSocket worker.
pub struct CloudSyncHandle {
    base_url: String,
    cmd_tx: tokio_mpsc::UnboundedSender<CloudSyncCommand>,
    event_rx: Mutex<mpsc::Receiver<CloudSyncEvent>>,
    status: Arc<Mutex<CloudSyncStatus>>,
    requested_downloads: Arc<Mutex<HashSet<String>>>,
    localized_partitions: Arc<Mutex<HashSet<String>>>,
    repaint_cb: Arc<Mutex<Option<RepaintCallback>>>,
    shutdown_flag: Arc<AtomicBool>,
    worker_thread: Option<thread::JoinHandle<()>>,
}

impl CloudSyncHandle {
    /// Spawn a background `daqcloud` sync worker attached to `store_dir` and `base_url`.
    pub fn spawn(
        base_url: impl Into<String>,
        store_dir: impl AsRef<path::Path>,
        active_track_day_id: Option<String>,
        repaint_cb: Option<RepaintCallback>,
    ) -> Self {
        let base_url = normalize_http_base_url(&base_url.into());
        let store_dir = store_dir.as_ref().to_path_buf();
        let (cmd_tx, cmd_rx) = tokio_mpsc::unbounded_channel::<CloudSyncCommand>();
        let (event_tx, event_rx) = mpsc::channel::<CloudSyncEvent>();
        let status = Arc::new(Mutex::new(CloudSyncStatus::Connecting {
            url: base_url.clone(),
        }));
        let requested_downloads = Arc::new(Mutex::new(HashSet::new()));
        let localized_partitions = Arc::new(Mutex::new(HashSet::new()));
        let repaint_cb = Arc::new(Mutex::new(repaint_cb));
        let shutdown_flag = Arc::new(AtomicBool::new(false));

        // Seed `localized_partitions` with partitions that are already local on disk.
        if let Ok(reg) = Registry::open_store(&store_dir)
            && let Ok(parts) = reg.list_all_partitions()
            && let Ok(mut guard) = localized_partitions.lock()
        {
            for p in parts {
                if p.is_local {
                    guard.insert(p.partition_id);
                }
            }
        }

        let thread_url = base_url.clone();
        let thread_store = store_dir.clone();
        let thread_status = Arc::clone(&status);
        let thread_req_dl = Arc::clone(&requested_downloads);
        let thread_loc_parts = Arc::clone(&localized_partitions);
        let thread_repaint = Arc::clone(&repaint_cb);
        let thread_shutdown = Arc::clone(&shutdown_flag);
        let thread_cmd_tx = cmd_tx.clone();

        let worker_thread = thread::Builder::new()
            .name("daq-cloud-sync".to_owned())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(err) => {
                        if let Ok(mut st) = thread_status.lock() {
                            *st = CloudSyncStatus::Error {
                                url: thread_url,
                                error: format!("Failed to build Tokio runtime: {err}"),
                            };
                        }
                        return;
                    }
                };

                rt.block_on(run_cloud_sync_runtime(
                    thread_url,
                    thread_store,
                    active_track_day_id,
                    cmd_rx,
                    thread_cmd_tx,
                    event_tx,
                    thread_status,
                    thread_req_dl,
                    thread_loc_parts,
                    thread_repaint,
                    thread_shutdown,
                ));
            })
            .ok();

        // Queue initial catalog sync and local partition upload pass.
        let _ = cmd_tx.send(CloudSyncCommand::SyncCatalog);
        let _ = cmd_tx.send(CloudSyncCommand::UploadLocalPartitions);

        Self {
            base_url,
            cmd_tx,
            event_rx: Mutex::new(event_rx),
            status,
            requested_downloads,
            localized_partitions,
            repaint_cb,
            shutdown_flag,
            worker_thread,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn status(&self) -> CloudSyncStatus {
        self.status
            .lock()
            .map(|g| g.clone())
            .unwrap_or(CloudSyncStatus::Disconnected)
    }

    pub fn set_repaint_callback(&self, callback: Option<RepaintCallback>) {
        if let Ok(mut guard) = self.repaint_cb.lock() {
            *guard = callback;
        }
    }

    pub fn trigger_sync(&self) {
        let _ = self.cmd_tx.send(CloudSyncCommand::SyncCatalog);
        let _ = self.cmd_tx.send(CloudSyncCommand::UploadLocalPartitions);
    }

    pub fn trigger_upload(&self) {
        let _ = self.cmd_tx.send(CloudSyncCommand::UploadLocalPartitions);
    }

    pub fn set_active_track_day(&self, track_day_id: Option<String>) {
        let _ = self
            .cmd_tx
            .send(CloudSyncCommand::SetActiveTrackDay { track_day_id });
    }

    /// Request an on-demand download of `overview_10hz.parquet` + `full_res.parquet` for
    /// `partition_id` if not already localized or in flight.
    pub fn request_partition_download(&self, partition_id: &str) -> bool {
        if let Ok(loc) = self.localized_partitions.lock()
            && loc.contains(partition_id)
        {
            return false;
        }
        if let Ok(mut req) = self.requested_downloads.lock()
            && !req.insert(partition_id.to_owned())
        {
            return false;
        }
        let _ = self.cmd_tx.send(CloudSyncCommand::DownloadPartition {
            partition_id: partition_id.to_owned(),
        });
        true
    }

    pub fn is_partition_localized(&self, partition_id: &str) -> bool {
        self.localized_partitions
            .lock()
            .map(|g| g.contains(partition_id))
            .unwrap_or(false)
    }

    pub fn push_annotation(&self, annotation: Annotation) {
        let _ = self
            .cmd_tx
            .send(CloudSyncCommand::PushAnnotation { annotation });
    }

    pub fn push_track_day(&self, day: TrackDay) {
        let _ = self.cmd_tx.send(CloudSyncCommand::PushTrackDay { day });
    }

    pub fn delete_annotation(&self, annotation_id: String, updated_at_ms: i64) {
        let _ = self.cmd_tx.send(CloudSyncCommand::DeleteAnnotation {
            annotation_id,
            updated_at_ms,
        });
    }

    pub fn push_derived_signal(&self, signal: DerivedSignalDef) {
        let _ = self
            .cmd_tx
            .send(CloudSyncCommand::PushDerivedSignal { signal });
    }

    pub fn delete_derived_signal(&self, id: String, updated_at_ms: i64) {
        let _ = self.cmd_tx.send(CloudSyncCommand::DeleteDerivedSignal {
            id,
            updated_at_ms,
        });
    }

    /// Drain all pending [`CloudSyncEvent`]s without blocking.
    pub fn drain_events(&self) -> Vec<CloudSyncEvent> {
        let mut out = Vec::new();
        if let Ok(rx) = self.event_rx.lock() {
            while let Ok(ev) = rx.try_recv() {
                out.push(ev);
            }
        }
        out
    }

    pub fn shutdown(&mut self) {
        self.shutdown_flag.store(true, Ordering::SeqCst);
        let _ = self.cmd_tx.send(CloudSyncCommand::Shutdown);
        if let Some(handle) = self.worker_thread.take() {
            let _ = handle.join();
        }
        if let Ok(mut st) = self.status.lock() {
            *st = CloudSyncStatus::Disconnected;
        }
    }
}

impl Drop for CloudSyncHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn notify_repaint(repaint_cb: &Arc<Mutex<Option<RepaintCallback>>>) {
    if let Ok(guard) = repaint_cb.lock()
        && let Some(cb) = guard.as_ref()
    {
        cb();
    }
}

fn update_status_snapshot(
    metrics: &Arc<Mutex<WorkerMetrics>>,
    status: &Arc<Mutex<CloudSyncStatus>>,
    base_url: &str,
    f: impl FnOnce(&mut WorkerMetrics),
) {
    if let Ok(mut m) = metrics.lock() {
        f(&mut m);
        let next = m.to_status(base_url);
        if let Ok(mut s) = status.lock() {
            *s = next;
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_cloud_sync_runtime(
    base_url: String,
    store_dir: path::PathBuf,
    initial_track_day: Option<String>,
    mut cmd_rx: tokio_mpsc::UnboundedReceiver<CloudSyncCommand>,
    internal_cmd_tx: tokio_mpsc::UnboundedSender<CloudSyncCommand>,
    event_tx: mpsc::Sender<CloudSyncEvent>,
    status: Arc<Mutex<CloudSyncStatus>>,
    requested_downloads: Arc<Mutex<HashSet<String>>>,
    localized_partitions: Arc<Mutex<HashSet<String>>>,
    repaint_cb: Arc<Mutex<Option<RepaintCallback>>>,
    shutdown_flag: Arc<AtomicBool>,
) {
    let client = reqwest::Client::builder()
        .timeout(time::Duration::from_secs(60))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let metrics = Arc::new(Mutex::new(WorkerMetrics::default()));
    let uploaded_partitions: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
    let mut active_track_day = initial_track_day;

    // Spawn WebSocket live listener task
    let ws_url = http_base_to_ws_url(&base_url);
    let ws_base_url = base_url.clone();
    let ws_store_dir = store_dir.clone();
    let ws_metrics = Arc::clone(&metrics);
    let ws_status = Arc::clone(&status);
    let ws_repaint = Arc::clone(&repaint_cb);
    let ws_shutdown = Arc::clone(&shutdown_flag);
    let ws_event_tx = event_tx.clone();
    let ws_cmd_tx = internal_cmd_tx.clone();

    let ws_task = tokio::spawn(async move {
        while !ws_shutdown.load(Ordering::SeqCst) {
            match tokio_tungstenite::connect_async(&ws_url).await {
                Ok((mut ws_stream, _)) => {
                    update_status_snapshot(&ws_metrics, &ws_status, &ws_base_url, |m| {
                        m.ws_connected = true;
                        m.connected = true;
                        m.last_error = None;
                    });
                    notify_repaint(&ws_repaint);

                    while !ws_shutdown.load(Ordering::SeqCst) {
                        let next_msg = tokio::select! {
                            msg = ws_stream.next() => msg,
                            _ = tokio::time::sleep(time::Duration::from_millis(200)) => {
                                if ws_shutdown.load(Ordering::SeqCst) {
                                    break;
                                }
                                continue;
                            }
                        };
                        let Some(Ok(msg)) = next_msg else {
                            break;
                        };
                        let text = match msg {
                            tokio_tungstenite::tungstenite::Message::Text(t) => t.to_string(),
                            tokio_tungstenite::tungstenite::Message::Binary(b) => {
                                String::from_utf8_lossy(&b).into_owned()
                            }
                            tokio_tungstenite::tungstenite::Message::Close(_) => break,
                            _ => continue,
                        };

                        let Ok(parsed) = serde_json::from_str::<CloudWsMessage>(&text) else {
                            continue;
                        };
                        match parsed {
                            CloudWsMessage::AnnotationUpdated { annotation } => {
                                if let Ok(reg) = Registry::open_store(&ws_store_dir)
                                    && reg.upsert_annotation(&annotation).unwrap_or(false)
                                {
                                    let _ = ws_event_tx.send(CloudSyncEvent::AnnotationsUpdated);
                                    notify_repaint(&ws_repaint);
                                }
                            }
                            CloudWsMessage::AnnotationDeleted {
                                annotation_id,
                                updated_at_ms,
                            } => {
                                if let Ok(reg) = Registry::open_store(&ws_store_dir)
                                    && reg
                                        .soft_delete_annotation(&annotation_id, updated_at_ms)
                                        .unwrap_or(false)
                                {
                                    let _ = ws_event_tx.send(CloudSyncEvent::AnnotationsUpdated);
                                    notify_repaint(&ws_repaint);
                                }
                            }
                            CloudWsMessage::DerivedSignalUpdated { signal } => {
                                if let Ok(reg) = Registry::open_store(&ws_store_dir)
                                    && reg.upsert_derived_signal(&signal).unwrap_or(false)
                                {
                                    let _ = ws_event_tx.send(CloudSyncEvent::DerivedSignalsUpdated);
                                    notify_repaint(&ws_repaint);
                                }
                            }
                            CloudWsMessage::DerivedSignalDeleted { id, updated_at_ms } => {
                                if let Ok(reg) = Registry::open_store(&ws_store_dir)
                                    && reg
                                        .soft_delete_derived_signal(&id, updated_at_ms)
                                        .unwrap_or(false)
                                {
                                    let _ = ws_event_tx.send(CloudSyncEvent::DerivedSignalsUpdated);
                                    notify_repaint(&ws_repaint);
                                }
                            }
                            CloudWsMessage::PartitionAvailable { manifest } => {
                                let _ =
                                    ws_cmd_tx.send(CloudSyncCommand::PrefetchOverview { manifest });
                            }
                            CloudWsMessage::Ping { .. } => {}
                        }
                    }

                    update_status_snapshot(&ws_metrics, &ws_status, &ws_base_url, |m| {
                        m.ws_connected = false;
                    });
                    notify_repaint(&ws_repaint);
                }
                Err(_) => {
                    update_status_snapshot(&ws_metrics, &ws_status, &ws_base_url, |m| {
                        m.ws_connected = false;
                    });
                }
            }

            if ws_shutdown.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(time::Duration::from_millis(250)).await;
        }
    });

    while let Some(cmd) = cmd_rx.recv().await {
        if shutdown_flag.load(Ordering::SeqCst) {
            break;
        }
        match cmd {
            CloudSyncCommand::Shutdown => break,
            CloudSyncCommand::SetActiveTrackDay { track_day_id } => {
                active_track_day = track_day_id;
            }
            CloudSyncCommand::SyncCatalog => {
                match sync_catalog_and_overviews(
                    &client,
                    &base_url,
                    &store_dir,
                    active_track_day.as_deref(),
                    &event_tx,
                    &repaint_cb,
                    &metrics,
                    &status,
                    &localized_partitions,
                )
                .await
                {
                    Ok(()) => {
                        update_status_snapshot(&metrics, &status, &base_url, |m| {
                            m.connected = true;
                            m.last_error = None;
                        });
                        notify_repaint(&repaint_cb);
                    }
                    Err(err) => {
                        update_status_snapshot(&metrics, &status, &base_url, |m| {
                            m.last_error = Some(err.clone());
                        });
                        let _ = event_tx.send(CloudSyncEvent::Error(err));
                        notify_repaint(&repaint_cb);
                    }
                }
            }
            CloudSyncCommand::PrefetchOverview { manifest } => {
                if let Err(err) = prefetch_single_manifest_overview(
                    &client,
                    &base_url,
                    &store_dir,
                    &manifest,
                    &event_tx,
                    &repaint_cb,
                    &metrics,
                    &status,
                )
                .await
                {
                    log::warn!(
                        "Failed to prefetch overview for {}: {err}",
                        manifest.partition_id
                    );
                }
            }
            CloudSyncCommand::DownloadPartition { partition_id } => {
                update_status_snapshot(&metrics, &status, &base_url, |m| {
                    m.pending_downloads = m.pending_downloads.saturating_add(1);
                });
                notify_repaint(&repaint_cb);

                let res =
                    download_full_partition(&client, &base_url, &store_dir, &partition_id).await;

                match res {
                    Ok(()) => {
                        if let Ok(mut loc) = localized_partitions.lock() {
                            loc.insert(partition_id.clone());
                        }
                        update_status_snapshot(&metrics, &status, &base_url, |m| {
                            m.pending_downloads = m.pending_downloads.saturating_sub(1);
                            m.synced_partitions = m.synced_partitions.saturating_add(1);
                            m.connected = true;
                            m.last_error = None;
                        });
                        let _ = event_tx.send(CloudSyncEvent::PartitionLocalized { partition_id });
                        notify_repaint(&repaint_cb);
                    }
                    Err(err) => {
                        if let Ok(mut req) = requested_downloads.lock() {
                            req.remove(&partition_id);
                        }
                        update_status_snapshot(&metrics, &status, &base_url, |m| {
                            m.pending_downloads = m.pending_downloads.saturating_sub(1);
                            m.last_error = Some(err.clone());
                        });
                        let _ = event_tx.send(CloudSyncEvent::Error(err));
                        notify_repaint(&repaint_cb);
                    }
                }
            }
            CloudSyncCommand::UploadLocalPartitions => {
                if let Err(err) = upload_local_partitions(
                    &client,
                    &base_url,
                    &store_dir,
                    &uploaded_partitions,
                    &event_tx,
                    &repaint_cb,
                    &metrics,
                    &status,
                )
                .await
                {
                    update_status_snapshot(&metrics, &status, &base_url, |m| {
                        m.last_error = Some(err.clone());
                    });
                    let _ = event_tx.send(CloudSyncEvent::Error(err));
                    notify_repaint(&repaint_cb);
                }
            }
            CloudSyncCommand::PushAnnotation { annotation } => {
                let url = format!("{base_url}/api/v1/annotations");
                let payload = AnnotationUpsertPayload::Single(Box::new(annotation));
                if let Err(err) = client.post(&url).json(&payload).send().await {
                    log::warn!("Failed to push annotation to {url}: {err}");
                }
            }
            CloudSyncCommand::PushTrackDay { day } => {
                let url = format!("{base_url}/api/v1/track-days");
                if let Err(err) = client.post(&url).json(&day).send().await {
                    log::warn!("Failed to push track day to {url}: {err}");
                }
            }
            CloudSyncCommand::DeleteAnnotation {
                annotation_id,
                updated_at_ms,
            } => {
                let url = format!(
                    "{base_url}/api/v1/annotations/{annotation_id}?updated_at_ms={updated_at_ms}"
                );
                if let Err(err) = client.delete(&url).send().await {
                    log::warn!("Failed to delete annotation at {url}: {err}");
                }
            }
            CloudSyncCommand::PushDerivedSignal { signal } => {
                let url = format!("{base_url}/api/v1/derived-signals");
                let payload = DerivedSignalUpsertPayload::Single(Box::new(signal));
                if let Err(err) = client.post(&url).json(&payload).send().await {
                    log::warn!("Failed to push derived signal to {url}: {err}");
                }
            }
            CloudSyncCommand::DeleteDerivedSignal { id, updated_at_ms } => {
                let url =
                    format!("{base_url}/api/v1/derived-signals/{id}?updated_at_ms={updated_at_ms}");
                if let Err(err) = client.delete(&url).send().await {
                    log::warn!("Failed to delete derived signal at {url}: {err}");
                }
            }
        }
    }

    shutdown_flag.store(true, Ordering::SeqCst);
    ws_task.abort();
}

#[allow(clippy::too_many_arguments)]
async fn sync_catalog_and_overviews(
    client: &reqwest::Client,
    base_url: &str,
    store_dir: &path::Path,
    active_track_day: Option<&str>,
    event_tx: &mpsc::Sender<CloudSyncEvent>,
    repaint_cb: &Arc<Mutex<Option<RepaintCallback>>>,
    metrics: &Arc<Mutex<WorkerMetrics>>,
    status: &Arc<Mutex<CloudSyncStatus>>,
    localized_partitions: &Arc<Mutex<HashSet<String>>>,
) -> Result<(), String> {
    let catalog_url = format!("{base_url}/api/v1/catalog");
    let resp = client
        .get(&catalog_url)
        .send()
        .await
        .map_err(|e| format!("GET {catalog_url} failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("GET {catalog_url} returned HTTP {}", resp.status()));
    }
    let catalog: CatalogResponse = resp
        .json()
        .await
        .map_err(|e| format!("Invalid catalog JSON from {catalog_url}: {e}"))?;

    let reg = Registry::open_store(store_dir)
        .map_err(|e| format!("Failed to open local registry: {e}"))?;

    for day in &catalog.track_days {
        reg.upsert_track_day(day)
            .map_err(|e| format!("Failed to upsert track day {}: {e}", day.id))?;
    }

    let mut remote_ann_map: HashMap<String, i64> = HashMap::new();
    let mut ann_changed = false;
    for ann in &catalog.annotations {
        remote_ann_map.insert(ann.id.clone(), ann.updated_at_ms);
        if reg
            .upsert_annotation(ann)
            .map_err(|e| format!("Failed to upsert remote annotation {}: {e}", ann.id))?
        {
            ann_changed = true;
        }
    }

    // Push any offline-created or newer local annotations (including tombstones) to `daqcloud`.
    if let Ok(local_anns) = reg.query_annotations_in_range(i64::MIN, i64::MAX, true) {
        let to_push: Vec<Annotation> = local_anns
            .into_iter()
            .filter(|local| {
                remote_ann_map
                    .get(&local.id)
                    .is_none_or(|&remote_ts| local.updated_at_ms > remote_ts)
            })
            .collect();
        if !to_push.is_empty() {
            let ann_url = format!("{base_url}/api/v1/annotations");
            let payload = AnnotationUpsertPayload::Batch(to_push);
            let _ = client.post(&ann_url).json(&payload).send().await;
        }
    }

    let mut remote_sig_map: HashMap<String, i64> = HashMap::new();
    let mut derived_changed = false;
    for sig in &catalog.derived_signals {
        remote_sig_map.insert(sig.id.clone(), sig.updated_at_ms);
        if reg
            .upsert_derived_signal(sig)
            .map_err(|e| format!("Failed to upsert remote derived signal {}: {e}", sig.id))?
        {
            derived_changed = true;
        }
    }

    // Push any offline-created or newer local derived signals (including tombstones) to `daqcloud`.
    if let Ok(local_sigs) = reg.list_derived_signals(true) {
        let to_push: Vec<DerivedSignalDef> = local_sigs
            .into_iter()
            .filter(|local| {
                remote_sig_map
                    .get(&local.id)
                    .is_none_or(|&remote_ts| local.updated_at_ms > remote_ts)
            })
            .collect();
        if !to_push.is_empty() {
            let sig_url = format!("{base_url}/api/v1/derived-signals");
            let payload = DerivedSignalUpsertPayload::Batch(to_push);
            let _ = client.post(&sig_url).json(&payload).send().await;
        }
    }

    let mut complete_count = 0usize;
    for manifest in &catalog.partitions {
        let part_dir = store_dir.join("partitions").join(&manifest.partition_id);
        fs::create_dir_all(&part_dir)
            .map_err(|e| format!("Failed to create {}: {e}", part_dir.display()))?;
        let manifest_path = part_dir.join("manifest.json");
        if !manifest_path.is_file()
            && let Ok(bytes) = serde_json::to_vec_pretty(manifest)
        {
            let _ = fs::write(&manifest_path, bytes);
        }
        let is_complete = Registry::is_partition_dir_complete(&part_dir);
        if is_complete {
            complete_count += 1;
            if let Ok(mut loc) = localized_partitions.lock() {
                loc.insert(manifest.partition_id.clone());
            }
        }
        reg.register_manifest(manifest, is_complete)
            .map_err(|e| format!("Failed to register manifest {}: {e}", manifest.partition_id))?;
    }

    update_status_snapshot(metrics, status, base_url, |m| {
        m.connected = true;
        m.synced_partitions = m.synced_partitions.max(complete_count);
        m.last_error = None;
    });

    let _ = event_tx.send(CloudSyncEvent::CatalogSynced {
        track_days: catalog.track_days.len(),
        partitions: catalog.partitions.len(),
        annotations: catalog.annotations.len(),
    });
    if ann_changed {
        let _ = event_tx.send(CloudSyncEvent::AnnotationsUpdated);
    }
    if derived_changed {
        let _ = event_tx.send(CloudSyncEvent::DerivedSignalsUpdated);
    }
    notify_repaint(repaint_cb);

    // Prioritize downloading 1Hz overviews + states for the active Track Day first, then others.
    let mut ordered_partitions = catalog.partitions;
    ordered_partitions.sort_by(|a, b| {
        let a_prio = active_track_day.is_some_and(|d| d == a.track_day_id);
        let b_prio = active_track_day.is_some_and(|d| d == b.track_day_id);
        b_prio
            .cmp(&a_prio)
            .then_with(|| a.start_utc_ms.cmp(&b.start_utc_ms))
    });

    // Ensure any content-addressed SuperDBC artifacts referenced by remote partitions
    // are cached locally under `<store_dir>/superdbcs/<superdbc_hash>.json`.
    let mut seen_superdbcs = HashSet::new();
    for manifest in &ordered_partitions {
        let hash = manifest.superdbc_hash.trim();
        if !hash.is_empty() && seen_superdbcs.insert(hash.to_owned()) {
            let _ = download_superdbc_if_missing(client, base_url, store_dir, hash).await;
        }
    }

    for manifest in &ordered_partitions {
        prefetch_single_manifest_overview(
            client, base_url, store_dir, manifest, event_tx, repaint_cb, metrics, status,
        )
        .await?;
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn prefetch_single_manifest_overview(
    client: &reqwest::Client,
    base_url: &str,
    store_dir: &path::Path,
    manifest: &PartitionManifest,
    event_tx: &mpsc::Sender<CloudSyncEvent>,
    repaint_cb: &Arc<Mutex<Option<RepaintCallback>>>,
    metrics: &Arc<Mutex<WorkerMetrics>>,
    status: &Arc<Mutex<CloudSyncStatus>>,
) -> Result<(), String> {
    let part_dir = store_dir.join("partitions").join(&manifest.partition_id);
    fs::create_dir_all(&part_dir)
        .map_err(|e| format!("Failed to create {}: {e}", part_dir.display()))?;
    let manifest_path = part_dir.join("manifest.json");
    if !manifest_path.is_file()
        && let Ok(bytes) = serde_json::to_vec_pretty(manifest)
    {
        let _ = fs::write(&manifest_path, bytes);
    }

    let reg = Registry::open_store(store_dir)
        .map_err(|e| format!("Failed to open local registry: {e}"))?;
    let is_complete = Registry::is_partition_dir_complete(&part_dir);
    reg.register_manifest(manifest, is_complete)
        .map_err(|e| format!("Failed to register manifest {}: {e}", manifest.partition_id))?;

    let ov1_missing = !part_dir.join("overview_1hz.parquet").is_file();
    let states_missing = !part_dir.join("states.parquet").is_file();

    if ov1_missing || states_missing {
        update_status_snapshot(metrics, status, base_url, |m| {
            m.pending_downloads = m.pending_downloads.saturating_add(1);
        });
        notify_repaint(repaint_cb);

        let res1 = download_file_resumable(
            client,
            base_url,
            &manifest.partition_id,
            "overview_1hz.parquet",
            &part_dir,
        )
        .await;
        let res2 = download_file_resumable(
            client,
            base_url,
            &manifest.partition_id,
            "states.parquet",
            &part_dir,
        )
        .await;

        update_status_snapshot(metrics, status, base_url, |m| {
            m.pending_downloads = m.pending_downloads.saturating_sub(1);
        });

        res1?;
        res2?;

        let _ = event_tx.send(CloudSyncEvent::OverviewDownloaded {
            partition_id: manifest.partition_id.clone(),
        });
        notify_repaint(repaint_cb);
    }

    Ok(())
}

async fn download_full_partition(
    client: &reqwest::Client,
    base_url: &str,
    store_dir: &path::Path,
    partition_id: &str,
) -> Result<(), String> {
    let part_dir = store_dir.join("partitions").join(partition_id);
    fs::create_dir_all(&part_dir)
        .map_err(|e| format!("Failed to create {}: {e}", part_dir.display()))?;

    for filename in [
        "manifest.json",
        "overview_1hz.parquet",
        "states.parquet",
        "overview_10hz.parquet",
        "full_res.parquet",
    ] {
        download_file_resumable(client, base_url, partition_id, filename, &part_dir).await?;
    }

    let reg = Registry::open_store(store_dir)
        .map_err(|e| format!("Failed to open local registry: {e}"))?;
    let manifest_path = part_dir.join("manifest.json");
    if let Ok(bytes) = fs::read(&manifest_path)
        && let Ok(manifest) = serde_json::from_slice::<PartitionManifest>(&bytes)
    {
        reg.register_manifest(&manifest, true)
            .map_err(|e| format!("Failed to register localized manifest {partition_id}: {e}"))?;
    } else {
        reg.set_partition_local(partition_id, true)
            .map_err(|e| format!("Failed to mark partition {partition_id} local: {e}"))?;
    }

    Ok(())
}

/// Download a single immutable partition file from `daqcloud` into `part_dir/<filename>`,
/// resuming from `part_dir/<filename>.part` with HTTP `Range: bytes=<offset>-` if a partial
/// file exists, and verifying the `x-checksum-sha256` digest before atomically renaming.
pub async fn download_file_resumable(
    client: &reqwest::Client,
    base_url: &str,
    partition_id: &str,
    filename: &str,
    part_dir: &path::Path,
) -> Result<bool, String> {
    let final_path = part_dir.join(filename);
    if final_path.is_file()
        && let Ok(meta) = fs::metadata(&final_path)
        && meta.len() > 0
    {
        return Ok(false);
    }

    fs::create_dir_all(part_dir)
        .map_err(|e| format!("Failed to create {}: {e}", part_dir.display()))?;
    let part_path = part_dir.join(format!("{filename}.part"));
    let url = format!("{base_url}/api/v1/partitions/{partition_id}/{filename}");

    let existing_len = fs::metadata(&part_path).map(|m| m.len()).unwrap_or(0);
    if existing_len > 0 {
        let resp = client
            .get(&url)
            .header(reqwest::header::RANGE, format!("bytes={existing_len}-"))
            .send()
            .await
            .map_err(|e| format!("GET (Range) {url} failed: {e}"))?;

        if resp.status() == reqwest::StatusCode::PARTIAL_CONTENT {
            let expected_full_sha = resp
                .headers()
                .get(CHECKSUM_HEADER)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.trim().to_ascii_lowercase());
            let expected_range_sha = resp
                .headers()
                .get(RANGE_CHECKSUM_HEADER)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.trim().to_ascii_lowercase());

            let chunk = resp
                .bytes()
                .await
                .map_err(|e| format!("Failed reading range body from {url}: {e}"))?;
            if let Some(exp_range) = &expected_range_sha {
                let actual_range = compute_sha256_hex(&chunk);
                if &actual_range != exp_range {
                    let _ = fs::remove_file(&part_path);
                    return Err(format!(
                        "Range SHA-256 mismatch for {url}: expected {exp_range}, got {actual_range}"
                    ));
                }
            }

            {
                let mut f = fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&part_path)
                    .map_err(|e| format!("Failed to open {}: {e}", part_path.display()))?;
                f.write_all(&chunk)
                    .map_err(|e| format!("Failed appending to {}: {e}", part_path.display()))?;
                f.flush()
                    .map_err(|e| format!("Failed flushing {}: {e}", part_path.display()))?;
            }

            let (_, full_sha) = compute_file_sha256_hex(&part_path)
                .map_err(|e| format!("Failed hashing {}: {e}", part_path.display()))?;
            if expected_full_sha
                .as_ref()
                .is_none_or(|exp| exp == &full_sha)
            {
                fs::rename(&part_path, &final_path).map_err(|e| {
                    format!(
                        "Failed renaming {} -> {}: {e}",
                        part_path.display(),
                        final_path.display()
                    )
                })?;
                return Ok(true);
            } else {
                // Corrupted partial file; remove and fall through to clean full download
                let _ = fs::remove_file(&part_path);
            }
        } else {
            let _ = fs::remove_file(&part_path);
        }
    }

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("GET {url} failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("GET {url} returned HTTP {}", resp.status()));
    }
    let expected_sha = resp
        .headers()
        .get(CHECKSUM_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_ascii_lowercase());
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("Failed reading body from {url}: {e}"))?;

    let actual_sha = compute_sha256_hex(&bytes);
    if let Some(exp) = &expected_sha
        && &actual_sha != exp
    {
        return Err(format!(
            "SHA-256 mismatch for {url}: expected {exp}, got {actual_sha}"
        ));
    }

    fs::write(&part_path, &bytes)
        .map_err(|e| format!("Failed writing {}: {e}", part_path.display()))?;
    fs::rename(&part_path, &final_path).map_err(|e| {
        format!(
            "Failed renaming {} -> {}: {e}",
            part_path.display(),
            final_path.display()
        )
    })?;
    Ok(true)
}

/// Upload a single partition file to `daqcloud` with `HEAD` deduplication, SHA-256 checksum
/// verification, and automatic `Content-Range` chunking for large files.
pub async fn upload_file_verified(
    client: &reqwest::Client,
    base_url: &str,
    partition_id: &str,
    filename: &str,
    local_path: &path::Path,
) -> Result<bool, String> {
    if !local_path.is_file() {
        return Ok(false);
    }
    let (total_size, sha256_hex) = compute_file_sha256_hex(local_path)
        .map_err(|e| format!("Failed hashing {}: {e}", local_path.display()))?;
    let url = format!("{base_url}/api/v1/partitions/{partition_id}/{filename}");

    if let Ok(head_resp) = client.head(&url).send().await
        && head_resp.status().is_success()
    {
        let remote_sha = head_resp
            .headers()
            .get(CHECKSUM_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.trim().to_ascii_lowercase());
        if remote_sha.as_deref() == Some(sha256_hex.as_str()) {
            return Ok(false);
        }
    }

    if total_size as usize <= UPLOAD_CHUNK_BYTES {
        let bytes = fs::read(local_path)
            .map_err(|e| format!("Failed reading {}: {e}", local_path.display()))?;
        let resp = client
            .put(&url)
            .header(CHECKSUM_HEADER, &sha256_hex)
            .body(bytes)
            .send()
            .await
            .map_err(|e| format!("PUT {url} failed: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("PUT {url} returned HTTP {}", resp.status()));
        }
        return Ok(true);
    }

    // Resumable chunked upload via Content-Range
    let mut file = fs::File::open(local_path)
        .map_err(|e| format!("Failed opening {}: {e}", local_path.display()))?;
    let mut offset = 0u64;
    let mut buf = vec![0u8; UPLOAD_CHUNK_BYTES];
    while offset < total_size {
        file.seek(io::SeekFrom::Start(offset))
            .map_err(|e| format!("Seek failed in {}: {e}", local_path.display()))?;
        let to_read = ((total_size - offset) as usize).min(UPLOAD_CHUNK_BYTES);
        let mut read_sofar = 0usize;
        while read_sofar < to_read {
            let n = file
                .read(&mut buf[read_sofar..to_read])
                .map_err(|e| format!("Read failed in {}: {e}", local_path.display()))?;
            if n == 0 {
                break;
            }
            read_sofar += n;
        }
        if read_sofar == 0 {
            break;
        }
        let chunk = &buf[..read_sofar];
        let chunk_sha = compute_sha256_hex(chunk);
        let end_inclusive = offset + (read_sofar as u64) - 1;
        let content_range = format!("bytes {offset}-{end_inclusive}/{total_size}");

        let mut req = client
            .put(&url)
            .header(reqwest::header::CONTENT_RANGE, content_range)
            .header(RANGE_CHECKSUM_HEADER, chunk_sha);
        if end_inclusive + 1 == total_size {
            req = req.header(CHECKSUM_HEADER, &sha256_hex);
        }
        let resp = req
            .body(chunk.to_vec())
            .send()
            .await
            .map_err(|e| format!("PUT (chunk) {url} failed: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("PUT (chunk) {url} returned HTTP {}", resp.status()));
        }
        offset = end_inclusive + 1;
    }

    Ok(true)
}

/// Upload a content-addressed `<store_dir>/superdbcs/<superdbc_hash>.json` artifact to
/// `daqcloud` (`PUT /api/v1/superdbcs/{superdbc_hash}`) if present locally and not already on server.
pub async fn upload_superdbc_if_present(
    client: &reqwest::Client,
    base_url: &str,
    store_dir: &path::Path,
    superdbc_hash: &str,
) -> Result<bool, String> {
    let hash = superdbc_hash.trim();
    if hash.is_empty() || !is_safe_path_segment(hash) {
        return Ok(false);
    }
    let local_path = store_dir.join("superdbcs").join(format!("{hash}.json"));
    if !local_path.is_file() {
        return Ok(false);
    }
    let (_, sha256_hex) = compute_file_sha256_hex(&local_path)
        .map_err(|e| format!("Failed hashing {}: {e}", local_path.display()))?;
    let url = format!("{base_url}/api/v1/superdbcs/{hash}");

    if let Ok(head_resp) = client.head(&url).send().await
        && head_resp.status().is_success()
    {
        return Ok(false);
    }

    let bytes = fs::read(&local_path)
        .map_err(|e| format!("Failed reading {}: {e}", local_path.display()))?;
    let resp = client
        .put(&url)
        .header(CHECKSUM_HEADER, &sha256_hex)
        .body(bytes)
        .send()
        .await
        .map_err(|e| format!("PUT {url} failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("PUT {url} returned HTTP {}", resp.status()));
    }
    Ok(true)
}

/// Download a content-addressed `SuperDBC` artifact (`GET /api/v1/superdbcs/{superdbc_hash}`)
/// into `<store_dir>/superdbcs/<superdbc_hash>.json` if not already present locally.
pub async fn download_superdbc_if_missing(
    client: &reqwest::Client,
    base_url: &str,
    store_dir: &path::Path,
    superdbc_hash: &str,
) -> Result<bool, String> {
    let hash = superdbc_hash.trim();
    if hash.is_empty() || !is_safe_path_segment(hash) {
        return Ok(false);
    }
    let superdbcs_dir = store_dir.join("superdbcs");
    let final_path = superdbcs_dir.join(format!("{hash}.json"));
    if final_path.is_file() {
        return Ok(false);
    }
    let url = format!("{base_url}/api/v1/superdbcs/{hash}");
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("GET {url} failed: {e}"))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(false);
    }
    if !resp.status().is_success() {
        return Err(format!("GET {url} returned HTTP {}", resp.status()));
    }
    let expected_sha = resp
        .headers()
        .get(CHECKSUM_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_ascii_lowercase());
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("Failed reading body from {url}: {e}"))?;
    if let Some(exp) = &expected_sha {
        let actual_sha = compute_sha256_hex(&bytes);
        if &actual_sha != exp {
            return Err(format!(
                "SHA-256 mismatch for {url}: expected {exp}, got {actual_sha}"
            ));
        }
    }
    fs::create_dir_all(&superdbcs_dir)
        .map_err(|e| format!("Failed creating {}: {e}", superdbcs_dir.display()))?;
    let tmp_path = superdbcs_dir.join(format!(".{hash}.json.tmp"));
    fs::write(&tmp_path, &bytes)
        .map_err(|e| format!("Failed writing {}: {e}", tmp_path.display()))?;
    fs::rename(&tmp_path, &final_path)
        .map_err(|e| format!("Failed renaming {}: {e}", final_path.display()))?;
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
async fn upload_local_partitions(
    client: &reqwest::Client,
    base_url: &str,
    store_dir: &path::Path,
    uploaded_partitions: &Arc<Mutex<HashSet<String>>>,
    event_tx: &mpsc::Sender<CloudSyncEvent>,
    repaint_cb: &Arc<Mutex<Option<RepaintCallback>>>,
    metrics: &Arc<Mutex<WorkerMetrics>>,
    status: &Arc<Mutex<CloudSyncStatus>>,
) -> Result<usize, String> {
    let reg = Registry::open_store(store_dir)
        .map_err(|e| format!("Failed to open local registry: {e}"))?;
    let all_parts = reg
        .list_all_partitions()
        .map_err(|e| format!("Failed to list partitions: {e}"))?;

    let mut to_upload = Vec::new();
    for part in all_parts {
        if !part.is_local {
            continue;
        }
        let already_done = uploaded_partitions
            .lock()
            .map(|g| g.contains(&part.partition_id))
            .unwrap_or(false);
        if !already_done {
            to_upload.push(part);
        }
    }

    if to_upload.is_empty() {
        return Ok(0);
    }

    update_status_snapshot(metrics, status, base_url, |m| {
        m.pending_uploads = m.pending_uploads.saturating_add(to_upload.len());
    });
    notify_repaint(repaint_cb);

    let mut uploaded_count = 0usize;
    for part in to_upload {
        let part_dir = store_dir.join(&part.relative_path);
        let mut any_uploaded = false;

        // Upload Parquet files and raw .log files BEFORE manifest.json so that when `daqcloud`
        // receives manifest.json and broadcasts `PartitionAvailable` over WebSocket, all data
        // files are already stored on the server.
        for filename in [
            "overview_1hz.parquet",
            "states.parquet",
            "overview_10hz.parquet",
            "full_res.parquet",
        ] {
            let file_path = part_dir.join(filename);
            if upload_file_verified(client, base_url, &part.partition_id, filename, &file_path)
                .await?
            {
                any_uploaded = true;
            }
        }

        for raw_entry in &part.manifest.raw_log_hashes {
            let raw_file = &raw_entry.file_name;
            if is_safe_path_segment(raw_file) {
                let raw_path = store_dir.join("raw").join(raw_file);
                if raw_path.is_file() {
                    let _ = upload_file_verified(
                        client,
                        base_url,
                        &part.partition_id,
                        raw_file,
                        &raw_path,
                    )
                    .await?;
                }
            }
        }

        let _ =
            upload_superdbc_if_present(client, base_url, store_dir, &part.manifest.superdbc_hash)
                .await?;

        let manifest_path = part_dir.join("manifest.json");
        if upload_file_verified(
            client,
            base_url,
            &part.partition_id,
            "manifest.json",
            &manifest_path,
        )
        .await?
        {
            any_uploaded = true;
        }

        if let Ok(mut guard) = uploaded_partitions.lock() {
            guard.insert(part.partition_id.clone());
        }
        update_status_snapshot(metrics, status, base_url, |m| {
            m.pending_uploads = m.pending_uploads.saturating_sub(1);
            m.synced_partitions = m.synced_partitions.saturating_add(1);
            m.connected = true;
            m.last_error = None;
        });

        if any_uploaded {
            uploaded_count += 1;
            let _ = event_tx.send(CloudSyncEvent::PartitionUploaded {
                partition_id: part.partition_id,
            });
        }
        notify_repaint(repaint_cb);
    }

    Ok(uploaded_count)
}

/// Perform a synchronous one-shot bidirectional sync between `store_dir` and `base_url`:
/// 1. Uploads all local partitions (`is_local == true`) to `daqcloud`.
/// 2. Fetches `/api/v1/catalog` and merges track days, partition manifests, and annotations.
/// 3. Pushes any local annotations that are newer or missing on `daqcloud`.
/// 4. Downloads `overview_1hz.parquet` + `states.parquet` for all remote partitions (and
///    `overview_10hz.parquet` + `full_res.parquet` as well when `download_full` is `true`).
pub fn sync_store_with_cloud_blocking(
    store_dir: impl AsRef<path::Path>,
    base_url: &str,
    download_full: bool,
) -> Result<CloudSyncSummary, String> {
    let store_dir = store_dir.as_ref().to_path_buf();
    let base_url = normalize_http_base_url(base_url);

    let run = async move {
        let client = reqwest::Client::builder()
            .timeout(time::Duration::from_secs(60))
            .build()
            .map_err(|e| e.to_string())?;

        let (event_tx, _event_rx) = mpsc::channel();
        let repaint_cb = Arc::new(Mutex::new(None));
        let metrics = Arc::new(Mutex::new(WorkerMetrics::default()));
        let status = Arc::new(Mutex::new(CloudSyncStatus::Connecting {
            url: base_url.clone(),
        }));
        let uploaded_partitions = Arc::new(Mutex::new(HashSet::new()));
        let localized_partitions = Arc::new(Mutex::new(HashSet::new()));

        let partitions_uploaded = upload_local_partitions(
            &client,
            &base_url,
            &store_dir,
            &uploaded_partitions,
            &event_tx,
            &repaint_cb,
            &metrics,
            &status,
        )
        .await?;

        let catalog_url = format!("{base_url}/api/v1/catalog");
        let catalog: CatalogResponse = client
            .get(&catalog_url)
            .send()
            .await
            .map_err(|e| format!("GET {catalog_url} failed: {e}"))?
            .json()
            .await
            .map_err(|e| format!("Invalid catalog JSON: {e}"))?;

        let remote_track_days = catalog.track_days.len();
        let remote_partitions = catalog.partitions.len();
        let annotations_pulled = catalog.annotations.len();

        let reg = Registry::open_store(&store_dir).map_err(|e| e.to_string())?;
        let local_anns_before = reg
            .query_annotations_in_range(i64::MIN, i64::MAX, true)
            .map(|v| v.len())
            .unwrap_or(0);

        sync_catalog_and_overviews(
            &client,
            &base_url,
            &store_dir,
            None,
            &event_tx,
            &repaint_cb,
            &metrics,
            &status,
            &localized_partitions,
        )
        .await?;

        let mut full_partitions_downloaded = 0usize;
        if download_full {
            for manifest in &catalog.partitions {
                let part_dir = store_dir.join("partitions").join(&manifest.partition_id);
                if !Registry::is_partition_dir_complete(&part_dir) {
                    download_full_partition(&client, &base_url, &store_dir, &manifest.partition_id)
                        .await?;
                    full_partitions_downloaded += 1;
                }
            }
        }

        Ok(CloudSyncSummary {
            remote_track_days,
            remote_partitions,
            overviews_downloaded: remote_partitions,
            full_partitions_downloaded,
            partitions_uploaded,
            annotations_pushed: local_anns_before,
            annotations_pulled,
        })
    };

    if tokio::runtime::Handle::try_current().is_ok() {
        thread::scope(|s| {
            s.spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| e.to_string())?;
                rt.block_on(run)
            })
            .join()
            .map_err(|_| "Cloud sync thread panicked".to_owned())?
        })
    } else {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        rt.block_on(run)
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
