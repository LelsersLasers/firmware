use crate::{
    router::{
        CloudPollOutcome, PartitionPreloadPayload, RepaintCallback, Resolution, RouterBackend,
        RouterError, SignalSeries, TileCache, TileKey, TileLoadResult, TileLoader,
        VirtualTimelineRouter, wire::decode_tile_payload,
    },
    storage::{
        Annotation, CloudSyncEvent, CloudSyncHandle, CloudSyncStatus, PartitionManifest,
        RegisteredPartition, Registry, RegistryError, SignalManifestEntry, TrackDay,
        parquet_reader::{
            preload_partition_dir_from_parquet, read_single_signal_full_res,
            read_single_signal_overview,
        },
    },
    time::Time,
};
use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

impl From<RegistryError> for RouterError {
    fn from(value: RegistryError) -> Self {
        Self::Registry(value.to_string())
    }
}

struct NativeLoadJob {
    key: TileKey,
    part_dir: Option<PathBuf>,
    cloud_base_url: Option<String>,
    res_tx: mpsc::Sender<TileLoadResult>,
}

/// Native filesystem + remote `daqcloud` tile loader for [`TileCache`].
///
/// 1. Reads local ZSTD `.parquet` files (`overview_10hz.parquet`, `full_res.parquet`) on a
///    dedicated background OS thread when present in `<store_dir>/partitions/...`.
/// 2. When viewing an unlocalized remote `daqcloud` partition before its full `.parquet` files
///    have finished downloading, fetches the individual signal tile on demand from
///    `GET /api/v1/partitions/{partition_id}/tile` (`DAQT\x01` binary codec) so zooming in on
///    cloud data is instantaneous on Desktop just like Web.
pub struct NativeTileLoader {
    partition_dirs: Mutex<HashMap<String, PathBuf>>,
    cloud_base_url: Arc<Mutex<Option<String>>>,
    repaint_cb: Arc<Mutex<Option<RepaintCallback>>>,
    req_tx: mpsc::Sender<NativeLoadJob>,
}

impl Default for NativeTileLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl NativeTileLoader {
    pub fn new() -> Self {
        let (req_tx, req_rx) = mpsc::channel::<NativeLoadJob>();
        let repaint_cb: Arc<Mutex<Option<RepaintCallback>>> = Arc::new(Mutex::new(None));
        let worker_repaint = Arc::clone(&repaint_cb);

        if let Err(err) = thread::Builder::new()
            .name("daq-tile-loader".into())
            .spawn(move || Self::worker_loop(req_rx, worker_repaint))
        {
            log::error!("Failed to spawn daq-tile-loader background thread: {err}");
        }

        Self {
            partition_dirs: Mutex::new(HashMap::new()),
            cloud_base_url: Arc::new(Mutex::new(None)),
            repaint_cb,
            req_tx,
        }
    }

    pub fn set_cloud_base_url(&self, base_url: Option<String>) {
        if let Ok(mut guard) = self.cloud_base_url.lock() {
            *guard = base_url;
        }
    }

    fn worker_loop(
        req_rx: mpsc::Receiver<NativeLoadJob>,
        repaint_cb: Arc<Mutex<Option<RepaintCallback>>>,
    ) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok();
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .ok();

        while let Ok(job) = req_rx.recv() {
            let result = Self::execute_load(
                &job.key,
                job.part_dir.as_deref(),
                job.cloud_base_url.as_deref(),
                rt.as_ref(),
                client.as_ref(),
            );
            let _ = job.res_tx.send(TileLoadResult {
                key: job.key,
                result,
            });
            if let Ok(guard) = repaint_cb.lock()
                && let Some(cb) = guard.as_ref()
            {
                cb();
            }
        }
    }

    fn resolution_filename(resolution: Resolution) -> &'static str {
        match resolution {
            Resolution::Overview1Hz => "overview_1hz.parquet",
            Resolution::Overview10Hz => "overview_10hz.parquet",
            Resolution::FullRes => "full_res.parquet",
        }
    }

    fn resolution_query_param(resolution: Resolution) -> &'static str {
        match resolution {
            Resolution::Overview1Hz => "1hz",
            Resolution::Overview10Hz => "10hz",
            Resolution::FullRes => "full",
        }
    }

    fn execute_load(
        key: &TileKey,
        part_dir: Option<&Path>,
        cloud_base_url: Option<&str>,
        rt: Option<&tokio::runtime::Runtime>,
        client: Option<&reqwest::Client>,
    ) -> Result<Arc<SignalSeries>, RouterError> {
        if let Some(dir) = part_dir {
            let file_path = dir.join(Self::resolution_filename(key.resolution));
            if file_path.is_file() {
                let series = match key.resolution {
                    Resolution::Overview1Hz | Resolution::Overview10Hz => {
                        read_single_signal_overview(&file_path, &key.signal_id)?
                    }
                    Resolution::FullRes => read_single_signal_full_res(&file_path, &key.signal_id)?,
                };
                return Ok(Arc::new(series));
            }
        }

        if let (Some(base_url), Some(rt), Some(client)) = (cloud_base_url, rt, client) {
            let trimmed = base_url.trim_end_matches('/');
            let url = format!("{trimmed}/api/v1/partitions/{}/tile", key.partition_id);
            let res_str = Self::resolution_query_param(key.resolution);
            let sig_id = key.signal_id.clone();
            let bytes = rt.block_on(async {
                let resp = client
                    .get(&url)
                    .query(&[("signal_id", sig_id.as_str()), ("resolution", res_str)])
                    .send()
                    .await
                    .map_err(|e| RouterError::Io(io::Error::other(e.to_string())))?;
                if !resp.status().is_success() {
                    return Err(RouterError::Io(io::Error::other(format!(
                        "remote tile HTTP {}",
                        resp.status()
                    ))));
                }
                resp.bytes()
                    .await
                    .map_err(|e| RouterError::Io(io::Error::other(e.to_string())))
            })?;
            let series = decode_tile_payload(&bytes).map_err(RouterError::Format)?;
            return Ok(Arc::new(series));
        }

        Err(RouterError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "tile file missing for partition '{}' ({:?})",
                key.partition_id, key.resolution
            ),
        )))
    }
}

impl TileLoader for NativeTileLoader {
    fn set_repaint_callback(&self, callback: Option<RepaintCallback>) {
        if let Ok(mut guard) = self.repaint_cb.lock() {
            *guard = callback;
        }
    }

    fn register_partition_dir(&self, partition_id: &str, partition_dir: &Path) {
        if let Ok(mut guard) = self.partition_dirs.lock() {
            guard.insert(partition_id.to_owned(), partition_dir.to_path_buf());
        }
    }

    fn clear(&self) {
        if let Ok(mut guard) = self.partition_dirs.lock() {
            guard.clear();
        }
    }

    fn can_load_async(&self, key: &TileKey) -> bool {
        let local_exists = self
            .partition_dirs
            .lock()
            .ok()
            .and_then(|dirs| dirs.get(&key.partition_id).cloned())
            .is_some_and(|dir| {
                dir.join(Self::resolution_filename(key.resolution))
                    .is_file()
            });
        if local_exists {
            return true;
        }
        self.cloud_base_url
            .lock()
            .ok()
            .is_some_and(|url| url.is_some())
    }

    fn request_load(&self, key: TileKey, res_tx: mpsc::Sender<TileLoadResult>) {
        let part_dir = self
            .partition_dirs
            .lock()
            .ok()
            .and_then(|dirs| dirs.get(&key.partition_id).cloned());
        let cloud_base_url = self.cloud_base_url.lock().ok().and_then(|u| u.clone());
        let _ = self.req_tx.send(NativeLoadJob {
            key,
            part_dir,
            cloud_base_url,
            res_tx,
        });
    }

    fn load_blocking(&self, key: &TileKey) -> Result<Option<Arc<SignalSeries>>, RouterError> {
        let part_dir = self
            .partition_dirs
            .lock()
            .ok()
            .and_then(|dirs| dirs.get(&key.partition_id).cloned());
        if let Some(dir) = &part_dir {
            let file_path = dir.join(Self::resolution_filename(key.resolution));
            if file_path.is_file() {
                let series = match key.resolution {
                    Resolution::Overview1Hz | Resolution::Overview10Hz => {
                        read_single_signal_overview(&file_path, &key.signal_id)?
                    }
                    Resolution::FullRes => read_single_signal_full_res(&file_path, &key.signal_id)?,
                };
                return Ok(Some(Arc::new(series)));
            }
        }

        let cloud_base_url = self.cloud_base_url.lock().ok().and_then(|u| u.clone());
        if let Some(base_url) = cloud_base_url {
            let (res_tx, res_rx) = mpsc::channel();
            if self
                .req_tx
                .send(NativeLoadJob {
                    key: key.clone(),
                    part_dir,
                    cloud_base_url: Some(base_url),
                    res_tx,
                })
                .is_ok()
                && let Ok(load_res) = res_rx.recv()
                && let Ok(series) = load_res.result
            {
                return Ok(Some(series));
            }
        }

        Ok(None)
    }

    fn preload_partition_dir(
        &self,
        partition_id: &str,
        partition_dir: &Path,
        known_signals: Option<&[SignalManifestEntry]>,
    ) -> Result<Option<PartitionPreloadPayload>, RouterError> {
        preload_partition_dir_from_parquet(partition_id, partition_dir, known_signals)
    }
}

/// Native SQLite (`Registry`) + filesystem Parquet + `CloudSyncHandle` implementation
/// of [`RouterBackend`].
pub struct NativeRouterBackend {
    store_dir: Option<PathBuf>,
    registry: Registry,
    cloud_sync: Option<CloudSyncHandle>,
    repaint_cb: Option<RepaintCallback>,
    loader: Arc<NativeTileLoader>,
}

impl NativeRouterBackend {
    pub fn open_store(
        store_dir: impl AsRef<Path>,
        loader: Arc<NativeTileLoader>,
    ) -> Result<Self, RouterError> {
        let store_dir = store_dir.as_ref().to_path_buf();
        let registry = Registry::open_store(&store_dir)?;
        Ok(Self {
            store_dir: Some(store_dir),
            registry,
            cloud_sync: None,
            repaint_cb: None,
            loader,
        })
    }

    pub fn in_memory(loader: Arc<NativeTileLoader>) -> Result<Self, RouterError> {
        let registry = Registry::open_in_memory()?;
        Ok(Self {
            store_dir: None,
            registry,
            cloud_sync: None,
            repaint_cb: None,
            loader,
        })
    }
}

impl RouterBackend for NativeRouterBackend {
    fn store_dir(&self) -> Option<&Path> {
        self.store_dir.as_deref()
    }

    fn attach_store(
        &mut self,
        store_dir: &Path,
        _tile_cache: &mut TileCache,
    ) -> Result<usize, RouterError> {
        self.registry = Registry::open_store(store_dir)?;
        self.store_dir = Some(store_dir.to_path_buf());
        Ok(0)
    }

    fn sync_store(&mut self) -> Result<usize, RouterError> {
        if let Some(store_dir) = &self.store_dir {
            Ok(self.registry.sync_from_store_dir(store_dir)?)
        } else {
            Ok(0)
        }
    }

    fn register_and_preload_manifest(
        &mut self,
        manifest: &PartitionManifest,
        partition_dir: &Path,
        tile_cache: &mut TileCache,
    ) -> Result<Option<RegisteredPartition>, RouterError> {
        let is_local = partition_dir.join("full_res.parquet").is_file();
        self.registry.register_manifest(manifest, is_local)?;
        tile_cache.preload_partition(
            &manifest.partition_id,
            partition_dir,
            Some(&manifest.signals),
        )?;
        Ok(self.registry.get_partition(&manifest.partition_id)?)
    }

    fn list_track_days(&self) -> Result<Vec<TrackDay>, RouterError> {
        Ok(self.registry.list_track_days()?)
    }

    fn get_track_day(&self, track_day_id: &str) -> Result<Option<TrackDay>, RouterError> {
        Ok(self.registry.get_track_day(track_day_id)?)
    }

    fn set_track_day_label(
        &mut self,
        track_day_id: &str,
        label: &str,
    ) -> Result<TrackDay, RouterError> {
        let day = self.registry.set_track_day_label(track_day_id, label)?;
        if let Some(cloud) = &self.cloud_sync {
            cloud.push_track_day(day.clone());
        }
        Ok(day)
    }

    fn list_partitions_for_day(
        &self,
        track_day_id: &str,
    ) -> Result<Vec<RegisteredPartition>, RouterError> {
        Ok(self.registry.list_partitions_for_day(track_day_id)?)
    }

    fn get_partition(
        &self,
        partition_id: &str,
    ) -> Result<Option<RegisteredPartition>, RouterError> {
        Ok(self.registry.get_partition(partition_id)?)
    }

    fn query_all_annotations(&self) -> Result<Vec<Annotation>, RouterError> {
        Ok(self
            .registry
            .query_annotations_in_range(i64::MIN / 2, i64::MAX / 2, false)?)
    }

    fn upsert_annotation(&mut self, annotation: &Annotation) -> Result<bool, RouterError> {
        let applied = self.registry.upsert_annotation(annotation)?;
        if applied && let Some(cloud) = &self.cloud_sync {
            cloud.push_annotation(annotation.clone());
        }
        Ok(applied)
    }

    fn soft_delete_annotation(
        &mut self,
        annotation_id: &str,
        updated_at_ms: i64,
    ) -> Result<bool, RouterError> {
        let deleted = self
            .registry
            .soft_delete_annotation(annotation_id, updated_at_ms)?;
        if deleted && let Some(cloud) = &self.cloud_sync {
            cloud.delete_annotation(annotation_id.to_owned(), updated_at_ms);
        }
        Ok(deleted)
    }

    fn query_all_derived_signals(
        &self,
        include_deleted: bool,
    ) -> Result<Vec<daqcore::analysis::DerivedSignalDef>, RouterError> {
        Ok(self.registry.list_derived_signals(include_deleted)?)
    }

    fn upsert_derived_signal(
        &mut self,
        def: &daqcore::analysis::DerivedSignalDef,
    ) -> Result<bool, RouterError> {
        let applied = self.registry.upsert_derived_signal(def)?;
        if applied && let Some(cloud) = &self.cloud_sync {
            cloud.push_derived_signal(def.clone());
        }
        Ok(applied)
    }

    fn soft_delete_derived_signal(
        &mut self,
        id: &str,
        updated_at_ms: i64,
    ) -> Result<bool, RouterError> {
        let deleted = self
            .registry
            .soft_delete_derived_signal(id, updated_at_ms)?;
        if deleted && let Some(cloud) = &self.cloud_sync {
            cloud.delete_derived_signal(id.trim().to_owned(), updated_at_ms);
        }
        Ok(deleted)
    }

    fn set_repaint_callback(&mut self, callback: Option<RepaintCallback>) {
        self.repaint_cb = callback.clone();
        self.loader.set_repaint_callback(callback.clone());
        if let Some(cloud) = &self.cloud_sync {
            cloud.set_repaint_callback(callback);
        }
    }

    fn connect_cloud(
        &mut self,
        base_url: String,
        active_track_day_id: Option<String>,
    ) -> Result<(), RouterError> {
        let Some(store_dir) = self.store_dir.clone() else {
            return Err(RouterError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Cannot connect to daqcloud without an attached store directory",
            )));
        };

        self.loader.set_cloud_base_url(Some(base_url.clone()));

        if let Some(mut existing) = self.cloud_sync.take() {
            existing.shutdown();
        }
        let handle = CloudSyncHandle::spawn(
            base_url,
            store_dir,
            active_track_day_id,
            self.repaint_cb.clone(),
        );
        self.cloud_sync = Some(handle);
        Ok(())
    }

    fn disconnect_cloud(&mut self) {
        self.loader.set_cloud_base_url(None);
        if let Some(mut handle) = self.cloud_sync.take() {
            handle.shutdown();
        }
    }

    fn cloud_status(&self) -> CloudSyncStatus {
        self.cloud_sync
            .as_ref()
            .map(CloudSyncHandle::status)
            .unwrap_or(CloudSyncStatus::Disconnected)
    }

    fn trigger_cloud_sync(&mut self) {
        if let Some(cloud) = &self.cloud_sync {
            cloud.trigger_sync();
        }
    }

    fn trigger_cloud_upload(&mut self) {
        if let Some(cloud) = &self.cloud_sync {
            cloud.trigger_upload();
        }
    }

    fn set_active_track_day(&mut self, track_day_id: Option<String>) {
        if let Some(cloud) = &self.cloud_sync {
            cloud.set_active_track_day(track_day_id);
        }
    }

    fn request_partition_download(&mut self, partition_id: &str) -> bool {
        self.cloud_sync
            .as_ref()
            .is_some_and(|cloud| cloud.request_partition_download(partition_id))
    }

    fn is_partition_localized(&self, partition_id: &str) -> bool {
        let Some(store_dir) = &self.store_dir else {
            return false;
        };
        if let Ok(Some(part)) = self.registry.get_partition(partition_id) {
            let part_dir = store_dir.join(&part.relative_path);
            part_dir.join("full_res.parquet").is_file()
        } else {
            false
        }
    }

    fn poll_cloud_sync(&mut self, _tile_cache: &mut TileCache) -> CloudPollOutcome {
        let events = match &self.cloud_sync {
            Some(cloud) => cloud.drain_events(),
            None => return CloudPollOutcome::default(),
        };
        if events.is_empty() {
            return CloudPollOutcome::default();
        }

        let mut outcome = CloudPollOutcome {
            events_processed: events.len(),
            reload_partitions: false,
            invalidate_annotations: false,
            reload_derived_signals: false,
        };

        for ev in events {
            match ev {
                CloudSyncEvent::CatalogSynced { .. }
                | CloudSyncEvent::OverviewDownloaded { .. }
                | CloudSyncEvent::PartitionLocalized { .. } => {
                    outcome.reload_partitions = true;
                }
                CloudSyncEvent::AnnotationsUpdated => {
                    outcome.invalidate_annotations = true;
                }
                CloudSyncEvent::DerivedSignalsUpdated => {
                    outcome.reload_derived_signals = true;
                }
                CloudSyncEvent::PartitionUploaded { .. } | CloudSyncEvent::Error(_) => {}
            }
        }

        outcome
    }
}

/// Create a live-only [`VirtualTimelineRouter`] backed by [`NativeRouterBackend::in_memory`]
/// and [`NativeTileLoader`].
pub fn live_router(now: Time, window_secs: f64) -> Result<VirtualTimelineRouter, RouterError> {
    let loader = Arc::new(NativeTileLoader::new());
    let backend = NativeRouterBackend::in_memory(Arc::clone(&loader))?;
    VirtualTimelineRouter::with_backend(Box::new(backend), Some(loader), now, window_secs)
}

/// Open `<store_dir>/local_registry.sqlite`, synchronize with `<store_dir>/partitions/`,
/// preload all local `overview_1hz.parquet` + `states.parquet` files into [`TileCache`],
/// and return a configured [`VirtualTimelineRouter`].
pub fn open_store_router(
    store_dir: impl AsRef<Path>,
    now: Time,
    window_secs: f64,
) -> Result<VirtualTimelineRouter, RouterError> {
    let loader = Arc::new(NativeTileLoader::new());
    let backend = NativeRouterBackend::open_store(store_dir, Arc::clone(&loader))?;
    VirtualTimelineRouter::with_backend(Box::new(backend), Some(loader), now, window_secs)
}

/// Extension trait adding [`VirtualTimelineRouter::open_store`] for native desktop/CLI/cloud callers.
pub trait NativeRouterExt {
    fn open_store(
        store_dir: impl AsRef<Path>,
        now: Time,
        window_secs: f64,
    ) -> Result<VirtualTimelineRouter, RouterError>;
}

impl NativeRouterExt for VirtualTimelineRouter {
    fn open_store(
        store_dir: impl AsRef<Path>,
        now: Time,
        window_secs: f64,
    ) -> Result<VirtualTimelineRouter, RouterError> {
        open_store_router(store_dir, now, window_secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        frame::{FrameKind, ParsedFrame},
        router::{ClockProvenance, DataSourceKind},
        superdbc::database::Database,
    };
    use std::fs;

    #[test]
    fn router_stitches_canonical_partitions_and_live_provisional_frames_seamlessly() {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let dbc_path = manifest_dir.join("../../superdbc/superdbc_per26.json");
        let log_path =
            manifest_dir.join("../../PER26/2026-09-06/2_regen/raw/log-2026-09-06--20-21-44.log");
        if !dbc_path.is_file() || !log_path.is_file() {
            return;
        }

        let db = Database::load(&dbc_path).expect("load PER26 database");
        let tmp_raw = tempfile::tempdir().expect("temp raw dir");
        fs::copy(
            &log_path,
            tmp_raw.path().join("log-2026-09-06--20-21-44.log"),
        )
        .expect("copy PER26 log");

        let tmp_store = tempfile::tempdir().expect("temp store dir");
        let summary = crate::log_parse::import_logs_to_store(tmp_raw.path(), tmp_store.path(), &db)
            .expect("import PER26 log");
        assert!(!summary.manifests.is_empty());

        let m0 = &summary.manifests[0];
        let part_start = m0.start_utc_ms;
        let part_end = m0.end_utc_ms;

        // Open store router
        let mut router = open_store_router(
            tmp_store.path(),
            Time::from_unix_millis(part_end + 5_000),
            30.0,
        )
        .expect("open store router");

        assert_eq!(router.partitions().len(), 1);
        assert_eq!(
            router.timeline().active_track_day_id(),
            Some(m0.track_day_id.as_str())
        );

        // Provenance inside canonical partition is CanonicalPartition
        let prov_inside = router.clock_provenance_at(part_start + 1_000);
        assert!(matches!(
            prov_inside,
            Some(ClockProvenance::CanonicalPartition(_))
        ));

        // Ingest live frames both overlapping the end of the partition (should be deduplicated in favor
        // of canonical SD data) and extending 2 seconds after `part_end` (should be stitched in!)
        let imu_msg = db
            .messages()
            .find(|m| m.name() == "IMU_acceleration")
            .expect("IMU_acceleration in PER26");
        let imu_key = imu_msg.key();

        let synthetic_raw = vec![0xE8, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        let decoded = db
            .decode(imu_key.bus_id, imu_key.identity, &synthetic_raw)
            .expect("decode synthetic IMU_acceleration");

        // Frame 1: inside canonical partition (at `part_end - 500`) -> overshadowed by canonical SD
        router.ingest_frame(ParsedFrame {
            timestamp: Time::from_unix_millis(part_end - 500),
            bus_id: imu_key.bus_id,
            identity: imu_key.identity,
            kind: FrameKind::Data,
            dlc: 8,
            raw_bytes: synthetic_raw.clone(),
            decoded: Some(decoded.clone()),
        });

        // Frame 2 & 3: after canonical partition (`part_end + 1000` and `part_end + 2000`) -> LiveProvisional
        for offset_ms in [1_000_i64, 2_000_i64] {
            router.ingest_frame(ParsedFrame {
                timestamp: Time::from_unix_millis(part_end + offset_ms),
                bus_id: imu_key.bus_id,
                identity: imu_key.identity,
                kind: FrameKind::Data,
                dlc: 8,
                raw_bytes: synthetic_raw.clone(),
                decoded: Some(decoded.clone()),
            });
        }

        // Coverage spans should show CanonicalSd followed by non-overlapping LiveProvisional
        let spans = router.coverage_spans(part_start..=(part_end + 5_000));
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].source, DataSourceKind::CanonicalSd);
        assert_eq!(spans[0].start_utc_ms, part_start);
        assert_eq!(spans[0].end_utc_ms, part_end);
        assert_eq!(spans[1].source, DataSourceKind::LiveProvisional);
        assert!(spans[1].start_utc_ms > part_end);

        // Query across the boundary (`part_end - 5_000 ..= part_end + 2_500`) at Overview1Hz
        let stitched_1hz = router.query_signal(
            "IMU_acceleration.X_axis",
            (part_end - 5_000)..=(part_end + 2_500),
            Resolution::Overview1Hz,
        );
        let (_, max_bound) = stitched_1hz.time_bounds().expect("non-empty stitched 1Hz");
        assert!(
            max_bound >= part_end + 1_000,
            "stitched series should include live provisional samples after partition end"
        );
    }
}
