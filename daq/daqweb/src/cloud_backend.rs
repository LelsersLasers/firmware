use daqcore::{
    analysis::DerivedSignalDef,
    router::{
        CloudPollOutcome, PartitionPreloadPayload, RepaintCallback, Resolution, RouterBackend,
        RouterError, SignalSeries, TileCache, TileKey, TileLoadResult, TileLoader,
        VirtualTimelineRouter,
        wire::{decode_preload_payload, decode_tile_payload},
    },
    storage::{
        Annotation, AnnotationUpsertPayload, CatalogResponse, CloudSyncStatus,
        DerivedSignalUpsertPayload, InMemoryCatalog, PartitionManifest, RegisteredPartition,
        TrackDay,
    },
    time::Time,
};
use std::{
    collections::HashSet,
    io, path,
    sync::{Arc, Mutex, mpsc},
};

/// Remote HTTP binary tile loader (`GET /api/v1/partitions/{partition_id}/tile`) for [`TileCache`].
///
/// Uses `ehttp::fetch` so it executes via browser `fetch()` on `wasm32-unknown-unknown` and via
/// background threads in native integration tests with zero platform `#[cfg]` conditionals.
pub struct WebTileLoader {
    base_url: Mutex<Option<String>>,
    repaint_cb: Arc<Mutex<Option<RepaintCallback>>>,
}

impl Default for WebTileLoader {
    fn default() -> Self {
        Self::new(None)
    }
}

impl WebTileLoader {
    pub fn new(base_url: Option<String>) -> Self {
        Self {
            base_url: Mutex::new(base_url),
            repaint_cb: Arc::new(Mutex::new(None)),
        }
    }

    pub fn set_base_url(&self, base_url: Option<String>) {
        if let Ok(mut guard) = self.base_url.lock() {
            *guard = base_url;
        }
    }

    pub fn base_url(&self) -> Option<String> {
        self.base_url.lock().ok().and_then(|g| g.clone())
    }

    fn resolution_query_param(resolution: Resolution) -> &'static str {
        match resolution {
            Resolution::Overview1Hz => "1hz",
            Resolution::Overview10Hz => "10hz",
            Resolution::FullRes => "full",
        }
    }
}

impl TileLoader for WebTileLoader {
    fn set_repaint_callback(&self, callback: Option<RepaintCallback>) {
        if let Ok(mut guard) = self.repaint_cb.lock() {
            *guard = callback;
        }
    }

    fn can_load_async(&self, _key: &TileKey) -> bool {
        self.base_url().is_some()
    }

    fn request_load(&self, key: TileKey, res_tx: mpsc::Sender<TileLoadResult>) {
        let Some(base_url) = self.base_url() else {
            return;
        };
        let trimmed = base_url.trim_end_matches('/');
        let res_param = Self::resolution_query_param(key.resolution);
        let mut parsed_url = match url::Url::parse(&format!(
            "{trimmed}/api/v1/partitions/{}/tile",
            key.partition_id
        )) {
            Ok(u) => u,
            Err(_) => {
                // Relative base URL in browser (e.g. "")
                let rel = format!(
                    "{trimmed}/api/v1/partitions/{}/tile?signal_id={}&resolution={res_param}",
                    key.partition_id, key.signal_id
                );
                let repaint_cb = Arc::clone(&self.repaint_cb);
                ehttp::fetch(ehttp::Request::get(rel), move |resp_res| {
                    let result = match resp_res {
                        Ok(resp) if resp.ok => decode_tile_payload(&resp.bytes)
                            .map(Arc::new)
                            .map_err(RouterError::Format),
                        Ok(resp) => Err(RouterError::Io(io::Error::other(format!(
                            "HTTP {}",
                            resp.status
                        )))),
                        Err(err) => Err(RouterError::Io(io::Error::other(err))),
                    };
                    let _ = res_tx.send(TileLoadResult { key, result });
                    if let Ok(guard) = repaint_cb.lock()
                        && let Some(cb) = guard.as_ref()
                    {
                        cb();
                    }
                });
                return;
            }
        };
        parsed_url
            .query_pairs_mut()
            .append_pair("signal_id", &key.signal_id)
            .append_pair("resolution", res_param);

        let repaint_cb = Arc::clone(&self.repaint_cb);
        let req = ehttp::Request::get(parsed_url.as_str());
        ehttp::fetch(req, move |resp_res| {
            let result = match resp_res {
                Ok(resp) if resp.ok => decode_tile_payload(&resp.bytes)
                    .map(Arc::new)
                    .map_err(RouterError::Format),
                Ok(resp) => Err(RouterError::Io(io::Error::other(format!(
                    "HTTP {}",
                    resp.status
                )))),
                Err(err) => Err(RouterError::Io(io::Error::other(err))),
            };
            let _ = res_tx.send(TileLoadResult { key, result });
            if let Ok(guard) = repaint_cb.lock()
                && let Some(cb) = guard.as_ref()
            {
                cb();
            }
        });
    }

    fn load_blocking(&self, _key: &TileKey) -> Result<Option<Arc<SignalSeries>>, RouterError> {
        Ok(None)
    }
}

enum WebCloudEvent {
    CatalogLoaded {
        catalog: CatalogResponse,
    },
    AnnotationsPolled {
        annotations: Vec<Annotation>,
    },
    DerivedSignalsPolled {
        signals: Vec<DerivedSignalDef>,
    },
    PreloadLoaded {
        partition_id: String,
        payload: PartitionPreloadPayload,
    },
    PreloadFailed {
        partition_id: String,
        error: String,
    },
    AnnotationSaved(Annotation),
    DerivedSignalSaved(DerivedSignalDef),
    Error {
        url: String,
        error: String,
    },
}

/// Cloud-backed [`RouterBackend`] for `daqweb` that synchronizes catalog metadata, annotations,
/// and `DAQP\x01` partition preload bundles over HTTP (`ehttp::fetch`).
pub struct WebCloudRouterBackend {
    catalog: InMemoryCatalog,
    loader: Arc<WebTileLoader>,
    base_url: Option<String>,
    status: CloudSyncStatus,
    repaint_cb: Option<RepaintCallback>,
    ev_tx: mpsc::Sender<WebCloudEvent>,
    ev_rx: mpsc::Receiver<WebCloudEvent>,
    in_flight_preloads: HashSet<String>,
    preloaded_partitions: HashSet<String>,
    last_catalog_poll: Option<web_time::Instant>,
    catalog_in_flight: bool,
    annotations_in_flight: bool,
    derived_in_flight: bool,
    initial_catalog_loaded: bool,
    last_annotation_sync_ms: i64,
    last_derived_sync_ms: i64,
}

impl WebCloudRouterBackend {
    pub fn new(loader: Arc<WebTileLoader>) -> Self {
        let (ev_tx, ev_rx) = mpsc::channel();
        Self {
            catalog: InMemoryCatalog::new(),
            loader,
            base_url: None,
            status: CloudSyncStatus::Disconnected,
            repaint_cb: None,
            ev_tx,
            ev_rx,
            in_flight_preloads: HashSet::new(),
            preloaded_partitions: HashSet::new(),
            last_catalog_poll: None,
            catalog_in_flight: false,
            annotations_in_flight: false,
            derived_in_flight: false,
            initial_catalog_loaded: false,
            last_annotation_sync_ms: 0,
            last_derived_sync_ms: 0,
        }
    }

    fn spawn_catalog_fetch(&mut self) {
        let Some(base_url) = self.base_url.clone() else {
            return;
        };
        if self.catalog_in_flight {
            return;
        }
        self.catalog_in_flight = true;
        self.last_catalog_poll = Some(web_time::Instant::now());

        let trimmed = base_url.trim_end_matches('/');
        let catalog_url = format!("{trimmed}/api/v1/catalog");
        let tx = self.ev_tx.clone();
        let repaint = self.repaint_cb.clone();
        let url_for_ev = base_url;

        ehttp::fetch(ehttp::Request::get(&catalog_url), move |resp_res| {
            let ev = match resp_res {
                Ok(resp) if resp.ok => match serde_json::from_slice::<CatalogResponse>(&resp.bytes)
                {
                    Ok(catalog) => WebCloudEvent::CatalogLoaded { catalog },
                    Err(e) => WebCloudEvent::Error {
                        url: url_for_ev,
                        error: format!("Invalid catalog JSON: {e}"),
                    },
                },
                Ok(resp) => WebCloudEvent::Error {
                    url: url_for_ev,
                    error: format!("Catalog HTTP {}", resp.status),
                },
                Err(err) => WebCloudEvent::Error {
                    url: url_for_ev,
                    error: err,
                },
            };
            let _ = tx.send(ev);
            if let Some(cb) = &repaint {
                cb();
            }
        });
    }

    fn spawn_annotations_poll(&mut self) {
        let Some(base_url) = self.base_url.clone() else {
            return;
        };
        if self.annotations_in_flight || self.derived_in_flight {
            return;
        }
        self.annotations_in_flight = true;
        self.derived_in_flight = true;
        self.last_catalog_poll = Some(web_time::Instant::now());

        let trimmed = base_url.trim_end_matches('/');
        let since_ms = self.last_annotation_sync_ms.saturating_add(1);
        let url =
            format!("{trimmed}/api/v1/annotations?since_utc_ms={since_ms}&include_deleted=true");
        let tx = self.ev_tx.clone();
        let repaint = self.repaint_cb.clone();
        let url_for_ev = base_url.clone();

        ehttp::fetch(ehttp::Request::get(&url), move |resp_res| {
            let ev = match resp_res {
                Ok(resp) if resp.ok => {
                    match serde_json::from_slice::<Vec<Annotation>>(&resp.bytes) {
                        Ok(annotations) => WebCloudEvent::AnnotationsPolled { annotations },
                        Err(e) => WebCloudEvent::Error {
                            url: url_for_ev,
                            error: format!("Invalid annotations JSON: {e}"),
                        },
                    }
                }
                Ok(resp) => WebCloudEvent::Error {
                    url: url_for_ev,
                    error: format!("Annotations HTTP {}", resp.status),
                },
                Err(err) => WebCloudEvent::Error {
                    url: url_for_ev,
                    error: err,
                },
            };
            let _ = tx.send(ev);
            if let Some(cb) = &repaint {
                cb();
            }
        });

        let since_sig_ms = self.last_derived_sync_ms.saturating_add(1);
        let sig_url = format!(
            "{trimmed}/api/v1/derived-signals?since_utc_ms={since_sig_ms}&include_deleted=true"
        );
        let tx_sig = self.ev_tx.clone();
        let repaint_sig = self.repaint_cb.clone();
        let url_for_sig_ev = base_url;

        ehttp::fetch(ehttp::Request::get(&sig_url), move |resp_res| {
            let ev = match resp_res {
                Ok(resp) if resp.ok => {
                    match serde_json::from_slice::<Vec<DerivedSignalDef>>(&resp.bytes) {
                        Ok(signals) => WebCloudEvent::DerivedSignalsPolled { signals },
                        Err(e) => WebCloudEvent::Error {
                            url: url_for_sig_ev,
                            error: format!("Invalid derived-signals JSON: {e}"),
                        },
                    }
                }
                Ok(resp) => WebCloudEvent::Error {
                    url: url_for_sig_ev,
                    error: format!("Derived signals HTTP {}", resp.status),
                },
                Err(err) => WebCloudEvent::Error {
                    url: url_for_sig_ev,
                    error: err,
                },
            };
            let _ = tx_sig.send(ev);
            if let Some(cb) = &repaint_sig {
                cb();
            }
        });
    }

    fn spawn_preload_fetch(&mut self, partition_id: String) {
        let Some(base_url) = self.base_url.clone() else {
            return;
        };
        if self.preloaded_partitions.contains(&partition_id)
            || !self.in_flight_preloads.insert(partition_id.clone())
        {
            return;
        }

        let trimmed = base_url.trim_end_matches('/');
        let preload_url = format!("{trimmed}/api/v1/partitions/{partition_id}/preload");
        let tx = self.ev_tx.clone();
        let repaint = self.repaint_cb.clone();

        ehttp::fetch(ehttp::Request::get(&preload_url), move |resp_res| {
            let ev = match resp_res {
                Ok(resp) if resp.ok => match decode_preload_payload(&resp.bytes) {
                    Ok(payload) => WebCloudEvent::PreloadLoaded {
                        partition_id,
                        payload,
                    },
                    Err(error) => WebCloudEvent::PreloadFailed {
                        partition_id,
                        error,
                    },
                },
                Ok(resp) => WebCloudEvent::PreloadFailed {
                    partition_id,
                    error: format!("Preload HTTP {}", resp.status),
                },
                Err(error) => WebCloudEvent::PreloadFailed {
                    partition_id,
                    error,
                },
            };
            let _ = tx.send(ev);
            if let Some(cb) = &repaint {
                cb();
            }
        });
    }

    fn update_connected_status(&mut self) {
        if let Some(url) = &self.base_url {
            self.status = CloudSyncStatus::Connected {
                url: url.clone(),
                synced_partitions: self.preloaded_partitions.len(),
                pending_downloads: self.in_flight_preloads.len(),
                pending_uploads: 0,
                ws_connected: true,
            };
        }
    }
}

impl RouterBackend for WebCloudRouterBackend {
    fn register_and_preload_manifest(
        &mut self,
        manifest: &PartitionManifest,
        _partition_dir: &path::Path,
        _tile_cache: &mut TileCache,
    ) -> Result<Option<RegisteredPartition>, RouterError> {
        self.catalog.register_manifest(manifest, false);
        Ok(self.catalog.get_partition(&manifest.partition_id))
    }

    fn list_track_days(&self) -> Result<Vec<TrackDay>, RouterError> {
        Ok(self.catalog.list_track_days())
    }

    fn get_track_day(&self, track_day_id: &str) -> Result<Option<TrackDay>, RouterError> {
        Ok(self.catalog.get_track_day(track_day_id))
    }

    fn set_track_day_label(
        &mut self,
        track_day_id: &str,
        label: &str,
    ) -> Result<TrackDay, RouterError> {
        let updated = self.catalog.set_track_day_label(track_day_id, label);
        if let Some(base_url) = &self.base_url
            && let Ok(body) = serde_json::to_vec(&updated)
        {
            let trimmed = base_url.trim_end_matches('/');
            let mut req = ehttp::Request::post(format!("{trimmed}/api/v1/track-days"), body);
            req.headers.insert("Content-Type", "application/json");
            ehttp::fetch(req, |_| {});
        }
        Ok(updated)
    }

    fn list_partitions_for_day(
        &self,
        track_day_id: &str,
    ) -> Result<Vec<RegisteredPartition>, RouterError> {
        Ok(self
            .catalog
            .list_partitions()
            .into_iter()
            .filter(|p| p.track_day_id == track_day_id)
            .collect())
    }

    fn get_partition(
        &self,
        partition_id: &str,
    ) -> Result<Option<RegisteredPartition>, RouterError> {
        Ok(self.catalog.get_partition(partition_id))
    }

    fn query_all_annotations(&self) -> Result<Vec<Annotation>, RouterError> {
        Ok(self.catalog.query_annotations(false))
    }

    fn upsert_annotation(&mut self, annotation: &Annotation) -> Result<bool, RouterError> {
        let changed = self.catalog.upsert_annotation(annotation);
        if let Some(base_url) = &self.base_url
            && let Ok(body) = serde_json::to_vec(&AnnotationUpsertPayload::Single(Box::new(
                annotation.clone(),
            )))
        {
            let trimmed = base_url.trim_end_matches('/');
            let mut req = ehttp::Request::post(format!("{trimmed}/api/v1/annotations"), body);
            req.headers.insert("Content-Type", "application/json");
            let tx = self.ev_tx.clone();
            let ann_clone = annotation.clone();
            ehttp::fetch(req, move |res| {
                if let Ok(resp) = res
                    && resp.ok
                {
                    let _ = tx.send(WebCloudEvent::AnnotationSaved(ann_clone));
                }
            });
        }
        Ok(changed)
    }

    fn soft_delete_annotation(
        &mut self,
        annotation_id: &str,
        updated_at_ms: i64,
    ) -> Result<bool, RouterError> {
        let deleted = self
            .catalog
            .soft_delete_annotation(annotation_id, updated_at_ms);
        if let Some(base_url) = &self.base_url {
            let trimmed = base_url.trim_end_matches('/');
            let mut req = ehttp::Request::get(format!(
                "{trimmed}/api/v1/annotations/{annotation_id}?updated_at_ms={updated_at_ms}"
            ));
            req.method = "DELETE".to_owned();
            ehttp::fetch(req, |_| {});
        }
        Ok(deleted)
    }

    fn query_all_derived_signals(
        &self,
        include_deleted: bool,
    ) -> Result<Vec<DerivedSignalDef>, RouterError> {
        Ok(self.catalog.query_derived_signals(include_deleted))
    }

    fn upsert_derived_signal(&mut self, def: &DerivedSignalDef) -> Result<bool, RouterError> {
        let changed = self.catalog.upsert_derived_signal(def);
        if let Some(base_url) = &self.base_url
            && let Ok(body) =
                serde_json::to_vec(&DerivedSignalUpsertPayload::Single(Box::new(def.clone())))
        {
            let trimmed = base_url.trim_end_matches('/');
            let mut req = ehttp::Request::post(format!("{trimmed}/api/v1/derived-signals"), body);
            req.headers.insert("Content-Type", "application/json");
            let tx = self.ev_tx.clone();
            let def_clone = def.clone();
            ehttp::fetch(req, move |res| {
                if let Ok(resp) = res
                    && resp.ok
                {
                    let _ = tx.send(WebCloudEvent::DerivedSignalSaved(def_clone));
                }
            });
        }
        Ok(changed)
    }

    fn soft_delete_derived_signal(
        &mut self,
        id: &str,
        updated_at_ms: i64,
    ) -> Result<bool, RouterError> {
        let deleted = self.catalog.soft_delete_derived_signal(id, updated_at_ms);
        if let Some(base_url) = &self.base_url {
            let trimmed = base_url.trim_end_matches('/');
            let mut req = ehttp::Request::get(format!(
                "{trimmed}/api/v1/derived-signals/{id}?updated_at_ms={updated_at_ms}"
            ));
            req.method = "DELETE".to_owned();
            ehttp::fetch(req, |_| {});
        }
        Ok(deleted)
    }

    fn set_repaint_callback(&mut self, callback: Option<RepaintCallback>) {
        self.repaint_cb = callback.clone();
        self.loader.set_repaint_callback(callback);
    }

    fn connect_cloud(
        &mut self,
        base_url: String,
        _active_track_day_id: Option<String>,
    ) -> Result<(), RouterError> {
        let normalized = base_url.trim().trim_end_matches('/').to_owned();
        self.loader.set_base_url(Some(normalized.clone()));
        self.base_url = Some(normalized.clone());
        self.status = CloudSyncStatus::Connecting { url: normalized };
        self.catalog_in_flight = false;
        self.annotations_in_flight = false;
        self.derived_in_flight = false;
        self.initial_catalog_loaded = false;
        self.last_annotation_sync_ms = 0;
        self.last_derived_sync_ms = 0;
        self.spawn_catalog_fetch();
        Ok(())
    }

    fn disconnect_cloud(&mut self) {
        self.loader.set_base_url(None);
        self.base_url = None;
        self.status = CloudSyncStatus::Disconnected;
        self.catalog = InMemoryCatalog::new();
        self.preloaded_partitions.clear();
        self.in_flight_preloads.clear();
        self.catalog_in_flight = false;
        self.annotations_in_flight = false;
        self.derived_in_flight = false;
        self.initial_catalog_loaded = false;
        self.last_annotation_sync_ms = 0;
        self.last_derived_sync_ms = 0;
    }

    fn cloud_status(&self) -> CloudSyncStatus {
        self.status.clone()
    }

    fn trigger_cloud_sync(&mut self) {
        self.catalog_in_flight = false;
        self.annotations_in_flight = false;
        self.derived_in_flight = false;
        self.spawn_catalog_fetch();
    }

    fn poll_cloud_sync(&mut self, tile_cache: &mut TileCache) -> CloudPollOutcome {
        // Once the initial catalog is loaded, poll lightweight `/api/v1/annotations?since_utc_ms=...`
        // and `/api/v1/derived-signals?since_utc_ms=...` every 2.5 seconds.
        if self.base_url.is_some()
            && !self.catalog_in_flight
            && !self.annotations_in_flight
            && !self.derived_in_flight
            && self
                .last_catalog_poll
                .is_none_or(|t| t.elapsed().as_secs_f32() >= 2.5)
        {
            if self.initial_catalog_loaded {
                self.spawn_annotations_poll();
            } else {
                self.spawn_catalog_fetch();
            }
        }

        let mut outcome = CloudPollOutcome::default();
        let mut pending_preloads = Vec::new();

        while let Ok(ev) = self.ev_rx.try_recv() {
            outcome.events_processed += 1;
            match ev {
                WebCloudEvent::CatalogLoaded { catalog } => {
                    self.catalog_in_flight = false;
                    self.initial_catalog_loaded = true;
                    let prev_part_count = self.catalog.list_partitions().len();
                    for td in &catalog.track_days {
                        self.catalog.upsert_track_day(td);
                    }
                    for manifest in &catalog.partitions {
                        let is_ready = tile_cache.is_partition_preloaded(&manifest.partition_id);
                        self.catalog.register_manifest(manifest, is_ready);
                        if !is_ready {
                            pending_preloads.push(manifest.partition_id.clone());
                        } else {
                            self.preloaded_partitions
                                .insert(manifest.partition_id.clone());
                        }
                    }
                    for ann in &catalog.annotations {
                        self.last_annotation_sync_ms =
                            self.last_annotation_sync_ms.max(ann.updated_at_ms);
                        if self.catalog.upsert_annotation(ann) {
                            outcome.invalidate_annotations = true;
                        }
                    }
                    for sig in &catalog.derived_signals {
                        self.last_derived_sync_ms =
                            self.last_derived_sync_ms.max(sig.updated_at_ms);
                        if self.catalog.upsert_derived_signal(sig) {
                            outcome.reload_derived_signals = true;
                        }
                    }
                    if catalog.partitions.len() != prev_part_count {
                        outcome.reload_partitions = true;
                    }
                    self.update_connected_status();
                }
                WebCloudEvent::AnnotationsPolled { annotations } => {
                    self.annotations_in_flight = false;
                    for ann in &annotations {
                        self.last_annotation_sync_ms =
                            self.last_annotation_sync_ms.max(ann.updated_at_ms);
                        if self.catalog.upsert_annotation(ann) {
                            outcome.invalidate_annotations = true;
                        }
                    }
                    self.update_connected_status();
                }
                WebCloudEvent::DerivedSignalsPolled { signals } => {
                    self.derived_in_flight = false;
                    for sig in &signals {
                        self.last_derived_sync_ms =
                            self.last_derived_sync_ms.max(sig.updated_at_ms);
                        if self.catalog.upsert_derived_signal(sig) {
                            outcome.reload_derived_signals = true;
                        }
                    }
                    self.update_connected_status();
                }
                WebCloudEvent::PreloadLoaded {
                    partition_id,
                    payload,
                } => {
                    self.in_flight_preloads.remove(&partition_id);
                    self.preloaded_partitions.insert(partition_id.clone());
                    tile_cache.insert_preload_payload(payload);
                    if let Some(part) = self.catalog.get_partition(&partition_id) {
                        self.catalog.register_manifest(&part.manifest, true);
                    }
                    outcome.reload_partitions = true;
                    self.update_connected_status();
                }
                WebCloudEvent::PreloadFailed {
                    partition_id,
                    error,
                } => {
                    self.in_flight_preloads.remove(&partition_id);
                    log::warn!("Failed to preload partition {partition_id}: {error}");
                    self.update_connected_status();
                }
                WebCloudEvent::AnnotationSaved(ann) => {
                    self.last_annotation_sync_ms =
                        self.last_annotation_sync_ms.max(ann.updated_at_ms);
                    self.catalog.upsert_annotation(&ann);
                    outcome.invalidate_annotations = true;
                }
                WebCloudEvent::DerivedSignalSaved(sig) => {
                    self.last_derived_sync_ms = self.last_derived_sync_ms.max(sig.updated_at_ms);
                    self.catalog.upsert_derived_signal(&sig);
                    outcome.reload_derived_signals = true;
                }
                WebCloudEvent::Error { url, error } => {
                    self.catalog_in_flight = false;
                    self.annotations_in_flight = false;
                    self.derived_in_flight = false;
                    self.status = CloudSyncStatus::Error { url, error };
                }
            }
        }

        for part_id in pending_preloads {
            self.spawn_preload_fetch(part_id);
        }
        if !self.in_flight_preloads.is_empty() {
            self.update_connected_status();
        }

        outcome
    }
}

/// Create a [`VirtualTimelineRouter`] configured with [`WebCloudRouterBackend`] and [`WebTileLoader`].
pub fn web_cloud_router(
    base_url: Option<String>,
    now: Time,
    window_secs: f64,
) -> Result<VirtualTimelineRouter, RouterError> {
    let loader = Arc::new(WebTileLoader::new(base_url.clone()));
    let backend = WebCloudRouterBackend::new(Arc::clone(&loader));
    let mut router =
        VirtualTimelineRouter::with_backend(Box::new(backend), Some(loader), now, window_secs)?;
    if let Some(url) = base_url
        && !url.trim().is_empty()
    {
        let _ = router.connect_cloud(url);
    }
    Ok(router)
}
