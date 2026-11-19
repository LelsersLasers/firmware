pub mod live_buffer;
pub mod tile_cache;
pub mod types;
pub mod wire;

pub use live_buffer::{
    DEFAULT_MAX_INSPECTOR_FRAMES, DEFAULT_MAX_SAMPLES_PER_SIGNAL, LiveColumnBuffer,
};
pub use tile_cache::{
    DEFAULT_TILE_CACHE_BYTES, LruTileMap, RepaintCallback, RouterError, TileCache,
    TileInFlightSummary, TileKey, TileLoadResult, TileLoader, TileLookup,
};
pub use types::{
    ActiveSignalMeta, BUCKET_1HZ_MS, BUCKET_10HZ_MS, ClockProvenance, CoverageSpan, DataSourceKind,
    EnvelopePoint, OVERVIEW_1HZ_THRESHOLD_MS, OVERVIEW_10HZ_THRESHOLD_MS, Resolution, SignalSeries,
    StateInterval, build_envelope_buckets,
};
pub use wire::{
    PRELOAD_WIRE_MAGIC, PartitionPreloadPayload, TILE_WIRE_MAGIC, decode_preload_payload,
    decode_tile_payload, encode_preload_payload, encode_tile_payload,
};

use crate::{
    analysis::{
        AnalysisError, DerivedSignalDef, DistanceSeries, Expr, FormulaEngine, TargetGrid,
        canonicalize_signal_ref, map_series_to_distance,
    },
    frame::ParsedFrame,
    storage::{
        Annotation, BackendLoadingActivity, CloudStatsSnapshot, CloudSyncStatus, InMemoryCatalog,
        PartitionManifest, RegisteredPartition, TrackDay,
    },
    time::Time,
    timeline::MasterTimeline,
};
use std::{
    collections::HashMap,
    io, ops, path,
    sync::{Arc, Mutex},
    time,
};

#[derive(Debug, Clone, Default)]
pub struct CloudPollOutcome {
    pub events_processed: usize,
    pub reload_partitions: bool,
    pub invalidate_annotations: bool,
    pub reload_derived_signals: bool,
}

/// Platform-agnostic storage, metadata registry, and cloud synchronization backend
/// for [`VirtualTimelineRouter`].
pub trait RouterBackend: Send {
    fn store_dir(&self) -> Option<&path::Path> {
        None
    }

    fn attach_store(
        &mut self,
        _store_dir: &path::Path,
        _tile_cache: &mut TileCache,
    ) -> Result<usize, RouterError> {
        Err(RouterError::Io(io::Error::new(
            io::ErrorKind::Unsupported,
            "Filesystem store directories are not supported on this platform backend",
        )))
    }

    fn sync_store(&mut self) -> Result<usize, RouterError> {
        Ok(0)
    }

    fn register_and_preload_manifest(
        &mut self,
        manifest: &PartitionManifest,
        partition_dir: &path::Path,
        tile_cache: &mut TileCache,
    ) -> Result<Option<RegisteredPartition>, RouterError>;

    fn list_track_days(&self) -> Result<Vec<TrackDay>, RouterError>;

    fn get_track_day(&self, track_day_id: &str) -> Result<Option<TrackDay>, RouterError>;

    fn set_track_day_label(
        &mut self,
        track_day_id: &str,
        label: &str,
    ) -> Result<TrackDay, RouterError>;

    fn list_partitions_for_day(
        &self,
        track_day_id: &str,
    ) -> Result<Vec<RegisteredPartition>, RouterError>;

    fn get_partition(&self, partition_id: &str)
    -> Result<Option<RegisteredPartition>, RouterError>;

    fn query_all_annotations(&self) -> Result<Vec<Annotation>, RouterError>;

    fn upsert_annotation(&mut self, annotation: &Annotation) -> Result<bool, RouterError>;

    fn soft_delete_annotation(
        &mut self,
        annotation_id: &str,
        updated_at_ms: i64,
    ) -> Result<bool, RouterError>;

    fn query_all_derived_signals(
        &self,
        _include_deleted: bool,
    ) -> Result<Vec<DerivedSignalDef>, RouterError> {
        Ok(Vec::new())
    }

    fn upsert_derived_signal(&mut self, _def: &DerivedSignalDef) -> Result<bool, RouterError> {
        Ok(false)
    }

    fn soft_delete_derived_signal(
        &mut self,
        _id: &str,
        _updated_at_ms: i64,
    ) -> Result<bool, RouterError> {
        Ok(false)
    }

    fn set_repaint_callback(&mut self, _callback: Option<RepaintCallback>) {}

    fn connect_cloud(
        &mut self,
        _base_url: String,
        _active_track_day_id: Option<String>,
    ) -> Result<(), RouterError> {
        Err(RouterError::Io(io::Error::new(
            io::ErrorKind::Unsupported,
            "Cloud sync is not configured on this backend",
        )))
    }

    fn disconnect_cloud(&mut self) {}

    fn cloud_status(&self) -> CloudSyncStatus {
        CloudSyncStatus::Disconnected
    }

    fn trigger_cloud_sync(&mut self) {}

    fn trigger_cloud_upload(&mut self) {}

    fn set_active_track_day(&mut self, _track_day_id: Option<String>) {}

    fn request_partition_download(&mut self, _partition_id: &str) -> bool {
        false
    }

    fn is_partition_localized(&self, _partition_id: &str) -> bool {
        false
    }

    fn poll_cloud_sync(&mut self, _tile_cache: &mut TileCache) -> CloudPollOutcome {
        CloudPollOutcome::default()
    }

    fn request_cloud_stats(&self) {}

    fn cloud_stats_snapshot(&self) -> CloudStatsSnapshot {
        CloudStatsSnapshot::default()
    }

    fn loading_activity(&self) -> BackendLoadingActivity {
        BackendLoadingActivity::default()
    }
}

/// Default portable in-memory [`RouterBackend`] used by `VirtualTimelineRouter::live`.
#[derive(Default)]
pub struct InMemoryRouterBackend {
    catalog: InMemoryCatalog,
}

impl InMemoryRouterBackend {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn catalog(&self) -> &InMemoryCatalog {
        &self.catalog
    }

    pub fn catalog_mut(&mut self) -> &mut InMemoryCatalog {
        &mut self.catalog
    }
}

impl RouterBackend for InMemoryRouterBackend {
    fn register_and_preload_manifest(
        &mut self,
        manifest: &PartitionManifest,
        partition_dir: &path::Path,
        tile_cache: &mut TileCache,
    ) -> Result<Option<RegisteredPartition>, RouterError> {
        self.catalog.register_manifest(manifest, true);
        if tile_cache.loader().is_some() {
            tile_cache.preload_partition(
                &manifest.partition_id,
                partition_dir,
                Some(&manifest.signals),
            )?;
        }
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
        Ok(self.catalog.set_track_day_label(track_day_id, label))
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
        Ok(self.catalog.upsert_annotation(annotation))
    }

    fn soft_delete_annotation(
        &mut self,
        annotation_id: &str,
        updated_at_ms: i64,
    ) -> Result<bool, RouterError> {
        Ok(self
            .catalog
            .soft_delete_annotation(annotation_id, updated_at_ms))
    }

    fn query_all_derived_signals(
        &self,
        include_deleted: bool,
    ) -> Result<Vec<DerivedSignalDef>, RouterError> {
        Ok(self.catalog.query_derived_signals(include_deleted))
    }

    fn upsert_derived_signal(&mut self, def: &DerivedSignalDef) -> Result<bool, RouterError> {
        Ok(self.catalog.upsert_derived_signal(def))
    }

    fn soft_delete_derived_signal(
        &mut self,
        id: &str,
        updated_at_ms: i64,
    ) -> Result<bool, RouterError> {
        Ok(self.catalog.soft_delete_derived_signal(id, updated_at_ms))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct QueryCacheKey {
    signal_id: String,
    start_ms: i64,
    end_ms: i64,
    resolution: Resolution,
}

#[derive(Default)]
struct QuerySignalCache {
    revision: u64,
    entries: HashMap<QueryCacheKey, SignalSeries>,
}

/// Unified timeline router combining canonical SD partitions (`TileCache` + [`RouterBackend`])
/// with provisional live CAN traffic (`LiveColumnBuffer`) on a single `utc_ms` coordinate axis.
pub struct VirtualTimelineRouter {
    live_buffer: LiveColumnBuffer,
    tile_cache: TileCache,
    backend: Box<dyn RouterBackend>,
    partitions: Vec<RegisteredPartition>,
    timeline: MasterTimeline,
    explicit_all_track_days: bool,
    formula_engine: FormulaEngine,
    router_revision: u64,
    query_cache: Mutex<QuerySignalCache>,
    cached_track_days: Mutex<Option<Vec<TrackDay>>>,
    cached_annotations: Mutex<Option<Vec<Annotation>>>,
    repaint_cb: Mutex<Option<RepaintCallback>>,
}

impl VirtualTimelineRouter {
    /// Create a live-only router backed by an in-memory metadata catalog.
    pub fn live(now: Time, window_secs: f64) -> Result<Self, String> {
        let timeline = MasterTimeline::live(now, window_secs)?;
        Ok(Self {
            live_buffer: LiveColumnBuffer::default(),
            tile_cache: TileCache::default(),
            backend: Box::new(InMemoryRouterBackend::new()),
            partitions: Vec::new(),
            timeline,
            explicit_all_track_days: false,
            formula_engine: FormulaEngine::with_builtin_config(),
            router_revision: 1,
            query_cache: Mutex::new(QuerySignalCache::default()),
            cached_track_days: Mutex::new(None),
            cached_annotations: Mutex::new(None),
            repaint_cb: Mutex::new(None),
        })
    }

    /// Create a router with an explicit platform [`RouterBackend`] and optional [`TileLoader`].
    pub fn with_backend(
        backend: Box<dyn RouterBackend>,
        loader: Option<Arc<dyn TileLoader>>,
        now: Time,
        window_secs: f64,
    ) -> Result<Self, RouterError> {
        let timeline = MasterTimeline::live(now, window_secs).map_err(io::Error::other)?;
        let mut router = Self {
            live_buffer: LiveColumnBuffer::default(),
            tile_cache: TileCache::with_loader(DEFAULT_TILE_CACHE_BYTES, loader),
            backend,
            partitions: Vec::new(),
            timeline,
            explicit_all_track_days: false,
            formula_engine: FormulaEngine::with_builtin_config(),
            router_revision: 1,
            query_cache: Mutex::new(QuerySignalCache::default()),
            cached_track_days: Mutex::new(None),
            cached_annotations: Mutex::new(None),
            repaint_cb: Mutex::new(None),
        };
        router.refresh_store()?;
        Ok(router)
    }

    /// Replace the active [`RouterBackend`] and [`TileLoader`] on this router.
    pub fn set_backend(
        &mut self,
        mut backend: Box<dyn RouterBackend>,
        loader: Option<Arc<dyn TileLoader>>,
    ) {
        let cb = self.repaint_cb.lock().ok().and_then(|g| g.clone());
        backend.set_repaint_callback(cb.clone());
        self.backend = backend;
        self.tile_cache.set_loader(loader);
        self.tile_cache.set_repaint_callback(cb);
        self.invalidate_metadata_cache();
    }

    /// Monotonically increasing revision counter that increments whenever partitions,
    /// background tiles, live frames, formulas, or annotations change.
    pub fn data_revision(&self) -> u64 {
        self.router_revision
            .wrapping_add(self.tile_cache.revision())
    }

    fn invalidate_metadata_cache(&mut self) {
        self.router_revision = self.router_revision.wrapping_add(1);
        if let Ok(mut guard) = self.cached_track_days.lock() {
            *guard = None;
        }
        if let Ok(mut guard) = self.cached_annotations.lock() {
            *guard = None;
        }
    }

    /// Attach or switch to a new on-disk store directory and preload its local partitions.
    pub fn attach_store(
        &mut self,
        store_dir: impl AsRef<path::Path>,
    ) -> Result<usize, RouterError> {
        let store_dir = store_dir.as_ref();
        self.tile_cache.clear();
        self.invalidate_metadata_cache();
        self.backend.attach_store(store_dir, &mut self.tile_cache)?;
        self.refresh_store()
    }

    /// Synchronize the backend with its underlying store, reload the partition list,
    /// preload any new `1Hz` overviews and state intervals into `TileCache`, and update day bounds.
    pub fn refresh_store(&mut self) -> Result<usize, RouterError> {
        self.invalidate_metadata_cache();
        let _ = self.backend.sync_store()?;
        let newly_preloaded = self.reload_registry_state()?;
        self.backend.trigger_cloud_upload();
        Ok(newly_preloaded)
    }

    /// Reload track days and partitions from the backend, preload any new `1Hz` overviews
    /// and state intervals into `TileCache`, and update timeline day bounds.
    pub fn reload_registry_state(&mut self) -> Result<usize, RouterError> {
        self.invalidate_metadata_cache();
        let had_bounds = self.timeline.day_bounds_ms().is_some();
        let days = self.backend.list_track_days()?;
        if let Ok(mut guard) = self.cached_track_days.lock() {
            *guard = Some(days.clone());
        }
        let mut all_partitions = Vec::new();
        for day in &days {
            let mut day_parts = self.backend.list_partitions_for_day(&day.id)?;
            all_partitions.append(&mut day_parts);
        }
        all_partitions.sort_by(|a, b| {
            a.start_utc_ms
                .cmp(&b.start_utc_ms)
                .then_with(|| a.partition_id.cmp(&b.partition_id))
        });

        let newly_preloaded =
            if let Some(store_dir) = self.backend.store_dir().map(path::Path::to_path_buf) {
                self.tile_cache
                    .preload_registered_partitions(&store_dir, &all_partitions)?
            } else {
                0
            };

        let active_missing = self
            .timeline
            .active_track_day_id()
            .is_some_and(|active_id| !days.iter().any(|d| d.id == active_id));
        if active_missing {
            self.explicit_all_track_days = false;
        }
        if !self.explicit_all_track_days
            && (self.timeline.active_track_day_id().is_none() || active_missing)
            && let Some(last_day) = days.last()
        {
            self.timeline
                .set_active_track_day_id(Some(last_day.id.clone()));
            self.backend.set_active_track_day(Some(last_day.id.clone()));
        }

        let active_day_id = self.timeline.active_track_day_id().map(str::to_owned);
        let mut min_ms: Option<i64> = None;
        let mut max_ms: Option<i64> = None;
        for part in &all_partitions {
            if active_day_id
                .as_deref()
                .is_none_or(|day_id| part.track_day_id == day_id)
            {
                min_ms = Some(min_ms.map_or(part.start_utc_ms, |m| m.min(part.start_utc_ms)));
                max_ms = Some(max_ms.map_or(part.end_utc_ms, |m| m.max(part.end_utc_ms)));
            }
        }
        if let (Some(lo), Some(hi)) = (min_ms, max_ms) {
            self.timeline.set_day_bounds_ms(Some((lo, hi.max(lo))));
        }
        if (!had_bounds || active_missing)
            && !self.explicit_all_track_days
            && self.live_buffer.is_empty()
            && let Some((lo, hi)) = self.timeline.day_bounds_ms()
        {
            self.timeline.snap_to_range_ms(lo, hi);
        }

        self.partitions = all_partitions;
        self.reload_derived_signals();
        Ok(newly_preloaded)
    }

    /// Synchronize `self.formula_engine` with the backend's persisted/synced derived signal records.
    pub fn reload_derived_signals(&mut self) -> bool {
        let Ok(defs) = self.backend.query_all_derived_signals(true) else {
            return false;
        };
        if defs.is_empty() {
            return false;
        }
        let changed = self.formula_engine.apply_synced_defs(&defs);
        if changed {
            self.router_revision = self.router_revision.wrapping_add(1);
        }
        changed
    }

    /// Validate, save, and sync a derived signal definition to the active backend and cloud.
    pub fn save_derived_signal(&mut self, mut def: DerivedSignalDef) -> Result<(), AnalysisError> {
        let now_ms = Time::now().unix_millis();
        let prev_ts = self
            .formula_engine
            .get(&def.id)
            .map_or(0, |c| c.def.updated_at_ms);
        if def.updated_at_ms <= prev_ts {
            def.updated_at_ms = now_ms.max(prev_ts.saturating_add(1));
        }
        def.deleted = false;
        self.formula_engine.upsert_formula(def.clone())?;
        let _ = self.backend.upsert_derived_signal(&def);
        self.router_revision = self.router_revision.wrapping_add(1);
        Ok(())
    }

    /// Delete (tombstone) a derived signal by ID and sync the tombstone to the backend and cloud.
    pub fn delete_derived_signal(&mut self, id: &str) -> Result<bool, RouterError> {
        let trimmed = id.trim();
        let now_ms = Time::now().unix_millis();
        let prev_ts = self
            .formula_engine
            .get(trimmed)
            .map_or(0, |c| c.def.updated_at_ms);
        let updated_at_ms = now_ms.max(prev_ts.saturating_add(1));
        let removed_local = self.formula_engine.remove_formula(trimmed);
        let removed_backend = self
            .backend
            .soft_delete_derived_signal(trimmed, updated_at_ms)?;
        if removed_local || removed_backend {
            self.router_revision = self.router_revision.wrapping_add(1);
        }
        Ok(removed_local || removed_backend)
    }

    /// Register a newly written [`PartitionManifest`] and preload its 1Hz overview and states.
    pub fn register_and_preload_manifest(
        &mut self,
        manifest: &PartitionManifest,
        partition_dir: impl AsRef<path::Path>,
    ) -> Result<(), RouterError> {
        let maybe_part = self.backend.register_and_preload_manifest(
            manifest,
            partition_dir.as_ref(),
            &mut self.tile_cache,
        )?;
        self.invalidate_metadata_cache();
        if let Some(day_parts) = maybe_part {
            self.partitions
                .retain(|p| p.partition_id != day_parts.partition_id);
            self.partitions.push(day_parts);
            self.partitions.sort_by(|a, b| {
                a.start_utc_ms
                    .cmp(&b.start_utc_ms)
                    .then_with(|| a.partition_id.cmp(&b.partition_id))
            });
        }
        self.timeline
            .observe_day_span_ms(manifest.start_utc_ms, manifest.end_utc_ms);
        if self.timeline.active_track_day_id().is_none() {
            self.timeline
                .set_active_track_day_id(Some(manifest.track_day_id.clone()));
            self.backend
                .set_active_track_day(Some(manifest.track_day_id.clone()));
        }
        self.backend.trigger_cloud_upload();
        Ok(())
    }

    pub fn set_repaint_callback(&mut self, callback: Option<RepaintCallback>) {
        if let Ok(mut guard) = self.repaint_cb.lock() {
            *guard = callback.clone();
        }
        self.tile_cache.set_repaint_callback(callback.clone());
        self.backend.set_repaint_callback(callback);
    }

    /// Connect this router to a `daqcloud` server at `base_url`.
    pub fn connect_cloud(&mut self, base_url: impl Into<String>) -> Result<(), RouterError> {
        let active_day = self.timeline.active_track_day_id().map(str::to_owned);
        self.backend.connect_cloud(base_url.into(), active_day)
    }

    /// Disconnect from `daqcloud` and terminate any background sync worker.
    pub fn disconnect_cloud(&mut self) {
        self.backend.disconnect_cloud();
    }

    /// Return the current connection and sync status for `daqcloud`.
    pub fn cloud_status(&self) -> CloudSyncStatus {
        self.backend.cloud_status()
    }

    /// Trigger an asynchronous fetch of `GET /api/v1/stats` from `daqcloud`.
    pub fn request_cloud_stats(&self) {
        self.backend.request_cloud_stats();
    }

    /// Return the latest `daqcloud` stats snapshot and rolling history.
    pub fn cloud_stats_snapshot(&self) -> CloudStatsSnapshot {
        self.backend.cloud_stats_snapshot()
    }

    /// Return the active background network/storage loading activity from the backend.
    pub fn backend_loading_activity(&self) -> BackendLoadingActivity {
        self.backend.loading_activity()
    }

    /// Trigger an immediate catalog sync and local partition upload pass.
    pub fn trigger_cloud_sync(&mut self) {
        self.backend.trigger_cloud_sync();
    }

    /// Request an on-demand download of `overview_10hz.parquet` + `full_res.parquet` for `partition_id`.
    pub fn request_partition_download(&mut self, partition_id: &str) -> bool {
        self.backend.request_partition_download(partition_id)
    }

    /// Drain pending cloud sync events from the backend, preload newly downloaded overviews
    /// into `TileCache`, and trigger on-demand downloads if the viewport is zoomed in.
    pub fn poll_cloud_sync(&mut self) -> usize {
        let outcome = self.backend.poll_cloud_sync(&mut self.tile_cache);
        if outcome.reload_partitions {
            let _ = self.reload_registry_state();
        } else {
            if outcome.invalidate_annotations {
                self.invalidate_metadata_cache();
            }
            if outcome.reload_derived_signals {
                self.reload_derived_signals();
            }
        }

        if self.timeline.window_ms() <= OVERVIEW_1HZ_THRESHOLD_MS {
            let win = self.timeline.range_ms();
            let start_ms = *win.start();
            let end_ms = *win.end();
            for part in &self.partitions {
                if !part.is_local && part.start_utc_ms <= end_ms && part.end_utc_ms >= start_ms {
                    self.backend.request_partition_download(&part.partition_id);
                }
            }
        }
        if self.timeline.is_comparison_mode()
            && let Ok(anns) = self.query_annotations_in_range(i64::MIN..=i64::MAX)
        {
            for selected_id in self.timeline.selected_annotation_ids() {
                if let Some(ann) = anns.iter().find(|a| &a.id == selected_id) {
                    let lo = ann.start_utc_ms;
                    let hi = ann.end_utc_ms.unwrap_or(lo).max(lo);
                    for part in &self.partitions {
                        if !part.is_local && part.start_utc_ms <= hi && part.end_utc_ms >= lo {
                            self.backend.request_partition_download(&part.partition_id);
                        }
                    }
                }
            }
        }

        outcome.events_processed
    }

    fn is_partition_readable(&self, part: &RegisteredPartition) -> bool {
        part.is_local || self.tile_cache.is_partition_preloaded(&part.partition_id)
    }

    pub fn store_dir(&self) -> Option<&path::Path> {
        self.backend.store_dir()
    }

    pub fn backend(&self) -> &dyn RouterBackend {
        self.backend.as_ref()
    }

    pub fn backend_mut(&mut self) -> &mut dyn RouterBackend {
        self.invalidate_metadata_cache();
        self.router_revision = self.router_revision.wrapping_add(1);
        self.backend.as_mut()
    }

    pub fn partitions(&self) -> &[RegisteredPartition] {
        &self.partitions
    }

    pub fn live_buffer(&self) -> &LiveColumnBuffer {
        &self.live_buffer
    }

    pub fn live_buffer_mut(&mut self) -> &mut LiveColumnBuffer {
        self.router_revision = self.router_revision.wrapping_add(1);
        &mut self.live_buffer
    }

    pub fn tile_cache(&self) -> &TileCache {
        &self.tile_cache
    }

    pub fn tile_cache_mut(&mut self) -> &mut TileCache {
        self.router_revision = self.router_revision.wrapping_add(1);
        &mut self.tile_cache
    }

    pub fn timeline(&self) -> &MasterTimeline {
        &self.timeline
    }

    pub fn timeline_mut(&mut self) -> &mut MasterTimeline {
        &mut self.timeline
    }

    pub fn formula_engine(&self) -> &FormulaEngine {
        &self.formula_engine
    }

    pub fn formula_engine_mut(&mut self) -> &mut FormulaEngine {
        self.router_revision = self.router_revision.wrapping_add(1);
        &mut self.formula_engine
    }

    /// Evaluate an arbitrary [`Expr`] across `range` (`[start_utc_ms..=end_utc_ms]`) at `resolution`
    /// without blocking the calling thread.
    pub fn evaluate_expression(
        &self,
        expr: &Expr,
        range: ops::RangeInclusive<i64>,
        resolution: Resolution,
    ) -> Result<SignalSeries, AnalysisError> {
        self.tile_cache.poll_completed();
        if range.is_empty() {
            return Ok(SignalSeries::empty_for(resolution));
        }
        let raw_resolution = if self.formula_engine.plan_contains_series_fn(expr) {
            if resolution == Resolution::FullRes
                || range.end().saturating_sub(*range.start()) <= 30_000
            {
                Resolution::FullRes
            } else {
                Resolution::Overview10Hz
            }
        } else {
            resolution
        };
        self.formula_engine
            .evaluate_expr_with(expr, &TargetGrid::Union, resolution, |raw_id| {
                self.query_raw_signal(raw_id, range.clone(), raw_resolution)
            })
    }

    /// Synchronously evaluate an arbitrary [`Expr`] across `range` at exact `resolution`.
    pub fn evaluate_expression_blocking(
        &self,
        expr: &Expr,
        range: ops::RangeInclusive<i64>,
        resolution: Resolution,
    ) -> Result<SignalSeries, RouterError> {
        if range.is_empty() {
            return Ok(SignalSeries::empty_for(resolution));
        }
        let (_, raw_ids) = self
            .formula_engine
            .execution_plan_for_expr(expr)
            .map_err(|e| RouterError::Io(io::Error::other(e.to_string())))?;

        let raw_resolution = if self.formula_engine.plan_contains_series_fn(expr) {
            Resolution::FullRes
        } else {
            resolution
        };

        let mut prefetched = std::collections::BTreeMap::new();
        for raw_id in raw_ids {
            let series = self.query_raw_signal_blocking(&raw_id, range.clone(), raw_resolution)?;
            prefetched.insert(raw_id, series);
        }

        self.formula_engine
            .evaluate_expr_with(expr, &TargetGrid::Union, resolution, |raw_id| {
                prefetched
                    .get(raw_id)
                    .cloned()
                    .unwrap_or_else(|| SignalSeries::empty_for(raw_resolution))
            })
            .map_err(|e| RouterError::Io(io::Error::other(e.to_string())))
    }

    /// Query `signal_id` (raw or derived) re-indexed against cumulative track distance integrated
    /// from `speed_signal_id` over `range`.
    pub fn query_signal_vs_distance(
        &self,
        signal_id: &str,
        speed_signal_id: &str,
        range: ops::RangeInclusive<i64>,
        resolution: Resolution,
    ) -> DistanceSeries {
        let sig_series = self.query_signal(signal_id, range.clone(), resolution);
        let spd_series = self.query_signal(speed_signal_id, range.clone(), resolution);
        map_series_to_distance(
            &sig_series,
            &spd_series,
            range,
            self.formula_engine.max_staleness_ms(),
        )
    }

    /// Synchronously query `signal_id` re-indexed against cumulative track distance.
    pub fn query_signal_vs_distance_blocking(
        &self,
        signal_id: &str,
        speed_signal_id: &str,
        range: ops::RangeInclusive<i64>,
        resolution: Resolution,
    ) -> Result<DistanceSeries, RouterError> {
        let sig_series = self.query_signal_blocking(signal_id, range.clone(), resolution)?;
        let spd_series = self.query_signal_blocking(speed_signal_id, range.clone(), resolution)?;
        Ok(map_series_to_distance(
            &sig_series,
            &spd_series,
            range,
            self.formula_engine.max_staleness_ms(),
        ))
    }

    /// Poll completed background tile reads into the LRU cache.
    pub fn poll_background_tiles(&self) -> usize {
        self.tile_cache.poll_completed()
    }

    /// Block until all currently in-flight background tile reads finish (or `timeout` elapses).
    pub fn wait_for_in_flight(&self, timeout: time::Duration) -> usize {
        self.tile_cache.wait_for_in_flight(timeout)
    }

    /// Ingest a single live [`ParsedFrame`] into `live_buffer` and advance `timeline`.
    pub fn ingest_frame(&mut self, frame: ParsedFrame) -> bool {
        let timestamp = frame.timestamp;
        self.live_buffer.push(frame);
        self.router_revision = self.router_revision.wrapping_add(1);
        self.timeline.observe(timestamp)
    }

    /// Ingest a batch of live [`ParsedFrame`]s into `live_buffer` and advance `timeline`.
    pub fn push_batch(&mut self, frames: Vec<ParsedFrame>) {
        let latest = frames.iter().map(|f| f.timestamp).max();
        self.live_buffer.push_batch(frames);
        self.router_revision = self.router_revision.wrapping_add(1);
        if let Some(timestamp) = latest {
            self.timeline.observe(timestamp);
        }
    }

    /// Clear live provisional history and reset the timeline to `now` while preserving
    /// registered canonical SD partitions.
    pub fn reset_live(&mut self, now: Time) {
        self.live_buffer.clear();
        self.router_revision = self.router_revision.wrapping_add(1);
        self.timeline.reset(now);
        for part in &self.partitions {
            self.timeline
                .observe_day_span_ms(part.start_utc_ms, part.end_utc_ms);
        }
    }

    /// Combined time span across all registered canonical SD partitions and the live buffer.
    pub fn captured_time_span(&self) -> Option<(Time, Time)> {
        let mut min_ms: Option<i64> = None;
        let mut max_ms: Option<i64> = None;

        for part in &self.partitions {
            if self.is_partition_readable(part) {
                min_ms = Some(min_ms.map_or(part.start_utc_ms, |m| m.min(part.start_utc_ms)));
                max_ms = Some(max_ms.map_or(part.end_utc_ms, |m| m.max(part.end_utc_ms)));
            }
        }
        if let Some((live_min, live_max)) = self.live_buffer.time_bounds_ms() {
            min_ms = Some(min_ms.map_or(live_min, |m| m.min(live_min)));
            max_ms = Some(max_ms.map_or(live_max, |m| m.max(live_max)));
        }

        Some((
            Time::from_unix_millis(min_ms?),
            Time::from_unix_millis(max_ms?),
        ))
    }

    /// Return the maximum `end_utc_ms` among readable canonical SD partitions that start at or before `up_to_utc_ms`.
    fn max_canonical_end_ms(&self, up_to_utc_ms: i64) -> Option<i64> {
        self.partitions
            .iter()
            .filter(|p| self.is_partition_readable(p) && p.start_utc_ms <= up_to_utc_ms)
            .map(|p| p.end_utc_ms)
            .max()
    }

    fn live_cutoff_start_ms(&self, start_ms: i64, end_ms: i64) -> i64 {
        match self.max_canonical_end_ms(end_ms) {
            Some(max_end) => start_ms.max(max_end.saturating_add(1)),
            None => start_ms,
        }
    }

    fn query_raw_signal_with_status(
        &self,
        signal_id: &str,
        range: ops::RangeInclusive<i64>,
        resolution: Resolution,
    ) -> (SignalSeries, bool) {
        self.tile_cache.poll_completed();
        if range.is_empty() {
            return (SignalSeries::empty_for(resolution), false);
        }

        let canonical_id = canonicalize_signal_ref(signal_id);
        let start_ms = *range.start();
        let end_ms = *range.end();

        let mut overlapping_ids: Vec<String> = Vec::new();
        for part in &self.partitions {
            if part.start_utc_ms <= end_ms
                && part.end_utc_ms >= start_ms
                && self.is_partition_readable(part)
            {
                overlapping_ids.push(part.partition_id.clone());
            }
        }

        let mut chunks = Vec::new();
        let mut any_fallback = false;
        for part_id in &overlapping_ids {
            if let Some(lookup) = self
                .tile_cache
                .query_signal(part_id, &canonical_id, resolution)
            {
                if lookup.is_fallback() {
                    any_fallback = true;
                }
                let sliced = lookup.series().slice_range(range.clone());
                if !sliced.is_empty() {
                    chunks.push(sliced);
                }
            }
        }

        let live_start_ms = self.live_cutoff_start_ms(start_ms, end_ms);
        if live_start_ms <= end_ms
            && let Some(live_series) =
                self.live_buffer
                    .query_signal(&canonical_id, live_start_ms..=end_ms, resolution)
            && !live_series.is_empty()
        {
            chunks.push(live_series);
        }

        (SignalSeries::stitch(chunks, resolution), any_fallback)
    }

    /// Query a raw CAN `signal_id` directly from `TileCache` and `LiveColumnBuffer`.
    pub fn query_raw_signal(
        &self,
        signal_id: &str,
        range: ops::RangeInclusive<i64>,
        resolution: Resolution,
    ) -> SignalSeries {
        self.query_raw_signal_with_status(signal_id, range, resolution)
            .0
    }

    /// Query `signal_id` (raw CAN signal or derived formula channel) across `range`
    /// (`[start_utc_ms..=end_utc_ms]`) at `resolution` without blocking the calling thread.
    pub fn query_signal(
        &self,
        signal_id: &str,
        range: ops::RangeInclusive<i64>,
        resolution: Resolution,
    ) -> SignalSeries {
        self.tile_cache.poll_completed();
        if range.is_empty() {
            return SignalSeries::empty_for(resolution);
        }

        let rev_before = self.data_revision();
        let cache_key = QueryCacheKey {
            signal_id: signal_id.to_owned(),
            start_ms: *range.start(),
            end_ms: *range.end(),
            resolution,
        };
        if let Ok(mut guard) = self.query_cache.lock() {
            if guard.revision != rev_before {
                guard.entries.clear();
                guard.revision = rev_before;
            } else if let Some(cached) = guard.entries.get(&cache_key) {
                return cached.clone();
            }
        }

        let (series, is_fallback) = if let Some(compiled) = self.formula_engine.get(signal_id) {
            let ast = compiled.ast.clone();
            let s = self
                .evaluate_expression(&ast, range, resolution)
                .unwrap_or_else(|_| SignalSeries::empty_for(resolution));
            let fb = resolution == Resolution::FullRes && matches!(s, SignalSeries::Envelope(_));
            (s, fb)
        } else {
            self.query_raw_signal_with_status(signal_id, range, resolution)
        };

        // Never cache a lower-resolution fallback under the requested resolution key.
        if !is_fallback {
            let rev_after = self.data_revision();
            if let Ok(mut guard) = self.query_cache.lock() {
                if guard.revision != rev_after || guard.entries.len() >= 128 {
                    guard.entries.clear();
                    guard.revision = rev_after;
                }
                guard.entries.insert(cache_key, series.clone());
            }
        }

        series
    }

    /// Synchronously query a raw CAN `signal_id` across `range` at exact `resolution`.
    pub fn query_raw_signal_blocking(
        &self,
        signal_id: &str,
        range: ops::RangeInclusive<i64>,
        resolution: Resolution,
    ) -> Result<SignalSeries, RouterError> {
        if range.is_empty() {
            return Ok(SignalSeries::empty_for(resolution));
        }

        let canonical_id = canonicalize_signal_ref(signal_id);
        let start_ms = *range.start();
        let end_ms = *range.end();

        let mut overlapping_ids: Vec<String> = Vec::new();
        for part in &self.partitions {
            if part.start_utc_ms <= end_ms
                && part.end_utc_ms >= start_ms
                && (part.is_local
                    || self.is_partition_readable(part)
                    || self.backend.is_partition_localized(&part.partition_id))
            {
                overlapping_ids.push(part.partition_id.clone());
            }
        }

        let mut chunks = Vec::new();
        for part_id in &overlapping_ids {
            if let Some(series) =
                self.tile_cache
                    .query_signal_blocking(part_id, &canonical_id, resolution)?
            {
                let sliced = series.slice_range(range.clone());
                if !sliced.is_empty() {
                    chunks.push(sliced);
                }
            }
        }

        let live_start_ms = match self.max_canonical_end_ms(end_ms) {
            Some(max_end) => start_ms.max(max_end.saturating_add(1)),
            None => start_ms,
        };
        if live_start_ms <= end_ms
            && let Some(live_series) =
                self.live_buffer
                    .query_signal(&canonical_id, live_start_ms..=end_ms, resolution)
            && !live_series.is_empty()
        {
            chunks.push(live_series);
        }

        Ok(SignalSeries::stitch(chunks, resolution))
    }

    /// Synchronously query `signal_id` (raw CAN signal or derived formula channel) across `range`
    /// at exact `resolution`.
    pub fn query_signal_blocking(
        &self,
        signal_id: &str,
        range: ops::RangeInclusive<i64>,
        resolution: Resolution,
    ) -> Result<SignalSeries, RouterError> {
        if range.is_empty() {
            return Ok(SignalSeries::empty_for(resolution));
        }

        if let Some(compiled) = self.formula_engine.get(signal_id) {
            let ast = compiled.ast.clone();
            return self.evaluate_expression_blocking(&ast, range, resolution);
        }

        self.query_raw_signal_blocking(signal_id, range, resolution)
    }

    /// Query discrete state intervals for `signal_id` across `range`, stitching canonical SD
    /// partitions and provisional live transitions.
    pub fn query_states(
        &self,
        signal_id: &str,
        range: ops::RangeInclusive<i64>,
    ) -> Vec<StateInterval> {
        if range.is_empty() {
            return Vec::new();
        }
        let canonical_id = canonicalize_signal_ref(signal_id);
        let start_ms = *range.start();
        let end_ms = *range.end();

        let mut out = Vec::new();
        for part in &self.partitions {
            if !self.is_partition_readable(part)
                || part.start_utc_ms > end_ms
                || part.end_utc_ms < start_ms
            {
                continue;
            }
            if let Some(intervals) = self
                .tile_cache
                .query_states(&part.partition_id, &canonical_id)
            {
                for interval in intervals.iter() {
                    if let Some(clamped) = interval.clamped_to(&range) {
                        out.push(clamped);
                    }
                }
            }
        }

        let live_start_ms = match self.max_canonical_end_ms(end_ms) {
            Some(max_end) => start_ms.max(max_end.saturating_add(1)),
            None => start_ms,
        };
        if live_start_ms <= end_ms {
            out.extend(
                self.live_buffer
                    .query_states(&canonical_id, live_start_ms..=end_ms),
            );
        }

        if out.is_empty() {
            let res = Resolution::for_window_ms((end_ms - start_ms).max(1), 1600.0);
            let series = self.query_signal(signal_id, range.clone(), res);
            out = rle_intervals_from_series(&series, &range);
        }

        out
    }

    /// Find the latest sample `(utc_ms, value)` for `signal_id` (raw or derived) at or before
    /// `target_utc_ms` without blocking.
    pub fn latest_signal_value_at(
        &self,
        signal_id: &str,
        target_utc_ms: i64,
    ) -> Option<(i64, f64)> {
        if self.formula_engine.contains(signal_id) {
            let cur_rev = self.data_revision();
            if let Ok(guard) = self.query_cache.lock()
                && guard.revision == cur_rev
            {
                for (k, series) in &guard.entries {
                    if k.signal_id == signal_id
                        && k.start_ms <= target_utc_ms
                        && target_utc_ms <= k.end_ms
                        && let Some(found) = series.latest_at_or_before(target_utc_ms)
                    {
                        return Some(found);
                    }
                }
            }
            let is_series_fn = self
                .formula_engine
                .get(signal_id)
                .is_some_and(|c| self.formula_engine.plan_contains_series_fn(&c.ast));
            if is_series_fn {
                let start_ms = self.timeline.start_utc_ms().min(target_utc_ms);
                let end_ms = self.timeline.end_utc_ms().max(target_utc_ms);
                let res = Resolution::for_window_ms((end_ms - start_ms).max(1), 1600.0);
                let series = self.query_signal(signal_id, start_ms..=end_ms, res);
                return series.latest_at_or_before(target_utc_ms);
            }
            let lookback_ms = (self.formula_engine.max_staleness_ms() * 8).max(2_000);
            let window = target_utc_ms.saturating_sub(lookback_ms)
                ..=target_utc_ms.saturating_add(lookback_ms);
            let series = self.query_signal(signal_id, window, Resolution::FullRes);
            return series.latest_at_or_before(target_utc_ms);
        }

        let canonical_id = canonicalize_signal_ref(signal_id);
        let max_canonical = self.max_canonical_end_ms(target_utc_ms);

        if max_canonical.is_none_or(|max_end| target_utc_ms > max_end)
            && let Some((live_t, live_v)) = self
                .live_buffer
                .latest_signal_value_at(&canonical_id, target_utc_ms)
            && max_canonical.is_none_or(|max_end| live_t > max_end)
        {
            return Some((live_t, live_v));
        }

        for part in self.partitions.iter().rev() {
            if !self.is_partition_readable(part) || part.start_utc_ms > target_utc_ms {
                continue;
            }
            if let Some(found) = self.tile_cache.latest_resident_at_or_before(
                &part.partition_id,
                &canonical_id,
                target_utc_ms,
            ) {
                return Some(found);
            }
        }

        self.live_buffer
            .latest_signal_value_at(&canonical_id, target_utc_ms)
    }

    /// Find the latest sample `(utc_ms, value)` for `signal_id` within `range` without blocking.
    pub fn latest_signal_value_in_range(
        &self,
        signal_id: &str,
        range: ops::RangeInclusive<i64>,
    ) -> Option<(i64, f64)> {
        if range.is_empty() {
            return None;
        }
        let start_ms = *range.start();
        let end_ms = *range.end();

        if self.formula_engine.contains(signal_id) {
            let found = self.latest_signal_value_at(signal_id, end_ms)?;
            return (found.0 >= start_ms).then_some(found);
        }

        let canonical_id = canonicalize_signal_ref(signal_id);
        let max_canonical = self.max_canonical_end_ms(end_ms);

        let live_start_ms = match max_canonical {
            Some(max_end) => start_ms.max(max_end.saturating_add(1)),
            None => start_ms,
        };
        if live_start_ms <= end_ms
            && let Some(found) = self
                .live_buffer
                .latest_signal_value_in_range(&canonical_id, live_start_ms..=end_ms)
        {
            return Some(found);
        }

        for part in self.partitions.iter().rev() {
            if !self.is_partition_readable(part)
                || part.start_utc_ms > end_ms
                || part.end_utc_ms < start_ms
            {
                continue;
            }
            if let Some(sig_entry) = part
                .manifest
                .signals
                .iter()
                .find(|s| s.signal_id == canonical_id)
                && (sig_entry.sample_count == 0
                    || sig_entry.min_utc_ms > end_ms
                    || sig_entry.max_utc_ms < start_ms)
            {
                continue;
            }
            let part_range = start_ms.max(part.start_utc_ms)..=end_ms.min(part.end_utc_ms);
            if let Some(found) = self.tile_cache.latest_resident_in_range(
                &part.partition_id,
                &canonical_id,
                part_range,
            ) {
                return Some(found);
            }
        }

        self.live_buffer
            .latest_signal_value_in_range(&canonical_id, start_ms..=end_ms)
    }

    /// Return sorted, deduplicated metadata for all raw signals that have at least one sample
    /// inside `range`.
    pub fn active_signals_in_range(
        &self,
        range: ops::RangeInclusive<i64>,
    ) -> Vec<ActiveSignalMeta> {
        if range.is_empty() {
            return Vec::new();
        }
        let start_ms = *range.start();
        let end_ms = *range.end();
        let mut by_id: std::collections::BTreeMap<String, ActiveSignalMeta> =
            std::collections::BTreeMap::new();

        for part in &self.partitions {
            if !self.is_partition_readable(part)
                || part.start_utc_ms > end_ms
                || part.end_utc_ms < start_ms
            {
                continue;
            }
            let clamped = start_ms.max(part.start_utc_ms)..=end_ms.min(part.end_utc_ms);
            for sig_entry in &part.manifest.signals {
                if by_id.contains_key(&sig_entry.signal_id) {
                    continue;
                }
                if self.tile_cache.partition_signal_has_samples_in_range(
                    &part.partition_id,
                    sig_entry,
                    clamped.clone(),
                ) {
                    by_id.insert(
                        sig_entry.signal_id.clone(),
                        ActiveSignalMeta::from_manifest_entry(sig_entry),
                    );
                }
            }
        }

        let live_start_ms = match self.max_canonical_end_ms(end_ms) {
            Some(max_end) => start_ms.max(max_end.saturating_add(1)),
            None => start_ms,
        };
        if live_start_ms <= end_ms {
            for meta in self
                .live_buffer
                .active_signals_in_range(live_start_ms..=end_ms)
            {
                by_id.entry(meta.signal_id.clone()).or_insert(meta);
            }
        }

        by_id.into_values().collect()
    }

    /// Return metadata for all raw signals that have samples in the active viewport.
    pub fn active_signals_in_viewport(&self) -> Vec<ActiveSignalMeta> {
        if !self.timeline.is_comparison_mode() {
            return self.active_signals_in_range(self.timeline.range_ms());
        }
        let mut by_id: std::collections::BTreeMap<String, ActiveSignalMeta> =
            std::collections::BTreeMap::new();
        for meta in self.active_signals_in_range(self.timeline.range_ms()) {
            by_id.insert(meta.signal_id.clone(), meta);
        }
        if let Ok(anns) = self.query_annotations_in_range(i64::MIN..=i64::MAX) {
            for selected_id in self.timeline.selected_annotation_ids() {
                if let Some(ann) = anns.iter().find(|a| &a.id == selected_id) {
                    let hi = ann
                        .end_utc_ms
                        .unwrap_or(ann.start_utc_ms)
                        .max(ann.start_utc_ms);
                    for meta in self.active_signals_in_range(ann.start_utc_ms..=hi) {
                        by_id.entry(meta.signal_id.clone()).or_insert(meta);
                    }
                }
            }
        }
        by_id.into_values().collect()
    }

    /// Return metadata for all raw signals that have samples in `[start_utc_ms..=setpoint_utc_ms]`.
    pub fn active_signals_to_setpoint(&self) -> Vec<ActiveSignalMeta> {
        self.active_signals_in_range(self.timeline.setpoint_range_ms())
    }

    /// Look up self-contained metadata for `signal_id` from registered partition manifests or the live buffer.
    pub fn signal_meta(&self, signal_id: &str) -> Option<ActiveSignalMeta> {
        let canonical_id = canonicalize_signal_ref(signal_id);
        let win = self.timeline.range_ms();
        for part in self.partitions.iter().rev() {
            if !self.is_partition_readable(part)
                || part.start_utc_ms > *win.end()
                || part.end_utc_ms < *win.start()
            {
                continue;
            }
            if let Some(entry) = part
                .manifest
                .signals
                .iter()
                .find(|s| s.signal_id == canonical_id)
            {
                return Some(ActiveSignalMeta::from_manifest_entry(entry));
            }
        }
        for part in self.partitions.iter().rev() {
            if let Some(entry) = part
                .manifest
                .signals
                .iter()
                .find(|s| s.signal_id == canonical_id)
            {
                return Some(ActiveSignalMeta::from_manifest_entry(entry));
            }
        }
        self.live_buffer.signal_meta(&canonical_id).cloned()
    }

    /// Return the data coverage spans (`CanonicalSd` and `LiveProvisional`) overlapping `range`.
    pub fn coverage_spans(&self, range: ops::RangeInclusive<i64>) -> Vec<CoverageSpan> {
        if range.is_empty() {
            return Vec::new();
        }
        let start_ms = *range.start();
        let end_ms = *range.end();
        let mut spans = Vec::new();

        for part in &self.partitions {
            if !self.is_partition_readable(part)
                || part.start_utc_ms > end_ms
                || part.end_utc_ms < start_ms
            {
                continue;
            }
            spans.push(CoverageSpan {
                start_utc_ms: part.start_utc_ms.max(start_ms),
                end_utc_ms: part.end_utc_ms.min(end_ms),
                source: DataSourceKind::CanonicalSd,
                partition_id: Some(part.partition_id.clone()),
            });
        }

        if let Some((live_min, live_max)) = self.live_buffer.time_bounds_ms() {
            let live_start = match self.max_canonical_end_ms(end_ms) {
                Some(max_end) => live_min.max(max_end.saturating_add(1)),
                None => live_min,
            }
            .max(start_ms);
            let live_end = live_max.min(end_ms);
            if live_start <= live_end {
                spans.push(CoverageSpan {
                    start_utc_ms: live_start,
                    end_utc_ms: live_end,
                    source: DataSourceKind::LiveProvisional,
                    partition_id: None,
                });
            }
        }

        spans
    }

    pub fn list_track_days(&self) -> Result<Vec<TrackDay>, RouterError> {
        if let Ok(guard) = self.cached_track_days.lock()
            && let Some(cached) = guard.as_ref()
        {
            return Ok(cached.clone());
        }
        let days = self.backend.list_track_days()?;
        if let Ok(mut guard) = self.cached_track_days.lock() {
            *guard = Some(days.clone());
        }
        Ok(days)
    }

    /// Update the human-readable label (`location` and `title`) for `track_day_id`.
    pub fn set_track_day_label(
        &mut self,
        track_day_id: &str,
        label: &str,
    ) -> Result<TrackDay, RouterError> {
        let updated = self.backend.set_track_day_label(track_day_id, label)?;
        self.invalidate_metadata_cache();
        Ok(updated)
    }

    /// Select a specific `TrackDay` (or `None` for all captured days).
    pub fn select_track_day(
        &mut self,
        track_day_id: Option<String>,
    ) -> Result<Option<(i64, i64)>, RouterError> {
        self.explicit_all_track_days = track_day_id.is_none();
        self.timeline.set_active_track_day_id(track_day_id.clone());
        self.timeline.clear_selected_annotations();
        self.backend.set_active_track_day(track_day_id.clone());

        let mut min_ms: Option<i64> = None;
        let mut max_ms: Option<i64> = None;

        match track_day_id.as_deref() {
            Some(day_id) => {
                for part in &self.partitions {
                    if part.track_day_id == day_id {
                        min_ms =
                            Some(min_ms.map_or(part.start_utc_ms, |m| m.min(part.start_utc_ms)));
                        max_ms = Some(max_ms.map_or(part.end_utc_ms, |m| m.max(part.end_utc_ms)));
                    }
                }
                if min_ms.is_none()
                    && let Some(day) = self.backend.get_track_day(day_id)?
                {
                    min_ms = Some(day.start_utc_ms);
                    max_ms = Some(day.end_utc_ms);
                }
            }
            None => {
                for part in &self.partitions {
                    min_ms = Some(min_ms.map_or(part.start_utc_ms, |m| m.min(part.start_utc_ms)));
                    max_ms = Some(max_ms.map_or(part.end_utc_ms, |m| m.max(part.end_utc_ms)));
                }
                if let Some((live_min, live_max)) = self.live_buffer.time_bounds_ms() {
                    min_ms = Some(min_ms.map_or(live_min, |m| m.min(live_min)));
                    max_ms = Some(max_ms.map_or(live_max, |m| m.max(live_max)));
                }
            }
        }

        let bounds = min_ms.zip(max_ms).map(|(lo, hi)| (lo, hi.max(lo)));
        self.timeline.set_day_bounds_ms(bounds);
        if track_day_id.is_some()
            && let Some((lo, hi)) = bounds
        {
            self.timeline.snap_to_range_ms(lo, hi);
        }
        Ok(bounds)
    }

    /// Return normalized `(bucket_utc_ms, normalized_intensity)` (`0.0..=1.0`) 1Hz activity buckets
    /// across `range`.
    pub fn activity_envelope_1hz(&self, range: ops::RangeInclusive<i64>) -> Vec<(i64, f32)> {
        if range.is_empty() {
            return Vec::new();
        }
        let start_ms = *range.start();
        let end_ms = *range.end();
        let lo_bucket = start_ms
            .div_euclid(BUCKET_1HZ_MS)
            .saturating_mul(BUCKET_1HZ_MS);
        let hi_bucket = end_ms
            .div_euclid(BUCKET_1HZ_MS)
            .saturating_mul(BUCKET_1HZ_MS);

        let mut merged: std::collections::BTreeMap<i64, u32> = std::collections::BTreeMap::new();
        for part in &self.partitions {
            if !self.is_partition_readable(part)
                || part.start_utc_ms > end_ms
                || part.end_utc_ms < start_ms
            {
                continue;
            }
            if let Some(buckets) = self.tile_cache.partition_activity_1hz(&part.partition_id) {
                let lo = buckets.partition_point(|&(b, _)| b < lo_bucket);
                let hi = buckets.partition_point(|&(b, _)| b <= hi_bucket);
                for &(b, count) in &buckets[lo..hi] {
                    let entry = merged.entry(b).or_insert(0);
                    *entry = entry.saturating_add(count);
                }
            }
        }

        let live_start_ms = match self.max_canonical_end_ms(end_ms) {
            Some(max_end) => start_ms.max(max_end.saturating_add(1)),
            None => start_ms,
        };
        if live_start_ms <= end_ms {
            for (b, count) in self
                .live_buffer
                .activity_1hz_in_range(live_start_ms..=end_ms)
            {
                let entry = merged.entry(b).or_insert(0);
                *entry = entry.saturating_add(count);
            }
        }

        let max_count = merged.values().copied().max().unwrap_or(0);
        if max_count == 0 {
            return Vec::new();
        }
        let denom = max_count as f32;
        merged
            .into_iter()
            .map(|(b, c)| (b, ((c as f32) / denom).clamp(0.05, 1.0)))
            .collect()
    }

    /// Resolve the clock correlation provenance at `target_utc_ms`.
    pub fn clock_provenance_at(&self, target_utc_ms: i64) -> Option<ClockProvenance> {
        for part in &self.partitions {
            if self.is_partition_readable(part)
                && part.start_utc_ms <= target_utc_ms
                && target_utc_ms <= part.end_utc_ms
            {
                return Some(ClockProvenance::CanonicalPartition(
                    part.clock_mapping.clone(),
                ));
            }
        }

        let max_canonical = self.max_canonical_end_ms(target_utc_ms);
        if let Some((live_min, live_max)) = self.live_buffer.time_bounds_ms()
            && max_canonical.is_none_or(|max_end| target_utc_ms > max_end)
            && target_utc_ms >= live_min
            && target_utc_ms <= live_max
        {
            return Some(ClockProvenance::LiveProvisional);
        }

        let win_start = self.timeline.start_utc_ms();
        let win_end = self.timeline.end_utc_ms();
        if let Some(part) = self
            .partitions
            .iter()
            .filter(|p| {
                self.is_partition_readable(p)
                    && p.start_utc_ms <= win_end
                    && p.end_utc_ms >= win_start
            })
            .min_by_key(|p| {
                if target_utc_ms < p.start_utc_ms {
                    p.start_utc_ms - target_utc_ms
                } else {
                    target_utc_ms.saturating_sub(p.end_utc_ms)
                }
            })
        {
            return Some(ClockProvenance::CanonicalPartition(
                part.clock_mapping.clone(),
            ));
        }

        if !self.live_buffer.is_empty() {
            return Some(ClockProvenance::LiveProvisional);
        }

        self.partitions
            .last()
            .map(|p| ClockProvenance::CanonicalPartition(p.clock_mapping.clone()))
    }

    pub fn query_annotations_in_range(
        &self,
        range: ops::RangeInclusive<i64>,
    ) -> Result<Vec<Annotation>, RouterError> {
        if range.is_empty() {
            return Ok(Vec::new());
        }
        let q_start = *range.start();
        let q_end = *range.end();
        if let Ok(guard) = self.cached_annotations.lock()
            && let Some(anns) = guard.as_ref()
        {
            return Ok(anns
                .iter()
                .filter(|a| match a.end_utc_ms {
                    None => a.start_utc_ms >= q_start && a.start_utc_ms <= q_end,
                    Some(end_ms) => a.start_utc_ms <= q_end && end_ms >= q_start,
                })
                .cloned()
                .collect());
        }
        let all = self.backend.query_all_annotations()?;
        let filtered = all
            .iter()
            .filter(|a| match a.end_utc_ms {
                None => a.start_utc_ms >= q_start && a.start_utc_ms <= q_end,
                Some(end_ms) => a.start_utc_ms <= q_end && end_ms >= q_start,
            })
            .cloned()
            .collect();
        if let Ok(mut guard) = self.cached_annotations.lock() {
            *guard = Some(all);
        }
        Ok(filtered)
    }

    /// Insert or update an annotation in the backend and expand day bounds if needed.
    pub fn upsert_annotation(&mut self, annotation: &Annotation) -> Result<bool, RouterError> {
        let changed = self.backend.upsert_annotation(annotation)?;
        self.invalidate_metadata_cache();
        let end_ms = annotation
            .end_utc_ms
            .unwrap_or(annotation.start_utc_ms)
            .max(annotation.start_utc_ms);
        if self
            .timeline
            .active_track_day_id()
            .is_none_or(|d| d == annotation.track_day_id)
        {
            self.timeline
                .observe_day_span_ms(annotation.start_utc_ms, end_ms);
        }
        if self.timeline.active_track_day_id().is_none() {
            self.timeline
                .set_active_track_day_id(Some(annotation.track_day_id.clone()));
        }
        Ok(changed)
    }

    /// Soft-delete an annotation in the backend and remove it from active timeline selection.
    pub fn soft_delete_annotation(
        &mut self,
        annotation_id: &str,
        updated_at_ms: i64,
    ) -> Result<bool, RouterError> {
        let deleted = self
            .backend
            .soft_delete_annotation(annotation_id, updated_at_ms)?;
        self.invalidate_metadata_cache();
        if self.timeline.is_annotation_selected(annotation_id) {
            self.timeline
                .toggle_comparison_annotation(annotation_id.to_owned(), 0, None);
        }
        Ok(deleted)
    }
}

fn rle_intervals_from_series(
    series: &SignalSeries,
    range: &ops::RangeInclusive<i64>,
) -> Vec<StateInterval> {
    let pairs: Vec<(i64, i64)> = match series {
        SignalSeries::Exact {
            timestamps_ms,
            values,
        } => timestamps_ms
            .iter()
            .copied()
            .zip(values.iter().copied())
            .filter(|&(_, v)| v.is_finite())
            .map(|(t, v)| (t, v.round() as i64))
            .collect(),
        SignalSeries::Envelope(buckets) => buckets
            .iter()
            .filter(|b| b.mean.is_finite())
            .map(|b| (b.utc_ms, b.mean.round() as i64))
            .collect(),
    };
    let Some(&(first_t, first_raw)) = pairs.first() else {
        return Vec::new();
    };
    let mut intervals = Vec::new();
    let mut cur_start = first_t;
    let mut last_t = first_t;
    let mut cur_raw = first_raw;
    for &(t, raw) in &pairs[1..] {
        if raw == cur_raw {
            last_t = t;
        } else {
            let interval = StateInterval {
                start_utc_ms: cur_start,
                end_utc_ms: t,
                raw_int: cur_raw,
                label: format!("RAW_{cur_raw}"),
            };
            if let Some(clamped) = interval.clamped_to(range) {
                intervals.push(clamped);
            }
            cur_start = t;
            last_t = t;
            cur_raw = raw;
        }
    }
    let tail = StateInterval {
        start_utc_ms: cur_start,
        end_utc_ms: last_t.max(cur_start),
        raw_int: cur_raw,
        label: format!("RAW_{cur_raw}"),
    };
    if let Some(clamped) = tail.clamped_to(range) {
        intervals.push(clamped);
    }
    intervals
}
