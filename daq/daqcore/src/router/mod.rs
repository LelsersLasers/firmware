pub mod live_buffer;
pub mod tile_cache;
pub mod types;

pub use live_buffer::{
    DEFAULT_MAX_INSPECTOR_FRAMES, DEFAULT_MAX_SAMPLES_PER_SIGNAL, LiveColumnBuffer,
};
pub use tile_cache::{
    DEFAULT_TILE_CACHE_BYTES, RepaintCallback, RouterError, TileCache, TileKey, TileLookup,
    read_all_signals_overview, read_all_signals_overview_with_activity, read_all_signals_states,
    read_single_signal_full_res, read_single_signal_overview,
};
pub use types::{
    BUCKET_1HZ_MS, BUCKET_10HZ_MS, ClockProvenance, CoverageSpan, DataSourceKind, EnvelopePoint,
    OVERVIEW_1HZ_THRESHOLD_MS, OVERVIEW_10HZ_THRESHOLD_MS, Resolution, SignalSeries, StateInterval,
    build_envelope_buckets,
};

use crate::{
    analysis::{
        AnalysisError, DistanceSeries, Expr, FormulaEngine, TargetGrid, canonicalize_signal_ref,
        map_series_to_distance,
    },
    frame::ParsedFrame,
    storage::{Annotation, PartitionManifest, RegisteredPartition, Registry, TrackDay},
    time::Time,
    timeline::MasterTimeline,
};
use std::{ops, path, time};

/// Unified timeline router combining canonical SD Parquet partitions (`TileCache` + `Registry`)
/// with provisional live CAN traffic (`LiveColumnBuffer`) on a single `utc_ms` coordinate axis.
///
/// ### Source Precedence Rule
/// When querying `[range.start(), range.end()]`:
/// 1. All registered local canonical SD partitions overlapping `range` are queried and stitched
///    in chronological order.
/// 2. If `range.end()` extends past the latest canonical SD partition into the time span covered
///    by `live_buffer`, provisional live samples for `t > max_partition_end_utc_ms` are appended.
pub struct VirtualTimelineRouter {
    live_buffer: LiveColumnBuffer,
    tile_cache: TileCache,
    registry: Registry,
    store_dir: Option<path::PathBuf>,
    partitions: Vec<RegisteredPartition>,
    timeline: MasterTimeline,
    formula_engine: FormulaEngine,
}

impl VirtualTimelineRouter {
    /// Create a live-only router backed by an in-memory metadata registry.
    pub fn live(now: Time, window_secs: f64) -> Result<Self, String> {
        let timeline = MasterTimeline::live(now, window_secs)?;
        let registry = Registry::open_in_memory().map_err(|e| e.to_string())?;
        Ok(Self {
            live_buffer: LiveColumnBuffer::default(),
            tile_cache: TileCache::default(),
            registry,
            store_dir: None,
            partitions: Vec::new(),
            timeline,
            formula_engine: FormulaEngine::with_builtin_config(),
        })
    }

    /// Create a router attached to an on-disk store directory (`<store_dir>/local_registry.sqlite`
    /// and `<store_dir>/partitions/`), preloading all local `1 Hz` overviews and state intervals.
    pub fn open_store(
        store_dir: impl AsRef<path::Path>,
        now: Time,
        window_secs: f64,
    ) -> Result<Self, RouterError> {
        let store_dir = store_dir.as_ref().to_path_buf();
        let timeline = MasterTimeline::live(now, window_secs).map_err(io::Error::other)?;
        let registry = Registry::open_store(&store_dir)?;
        let mut router = Self {
            live_buffer: LiveColumnBuffer::default(),
            tile_cache: TileCache::default(),
            registry,
            store_dir: Some(store_dir),
            partitions: Vec::new(),
            timeline,
            formula_engine: FormulaEngine::with_builtin_config(),
        };
        router.refresh_store()?;
        Ok(router)
    }

    /// Attach or switch to a new on-disk store directory and preload its local partitions.
    pub fn attach_store(
        &mut self,
        store_dir: impl AsRef<path::Path>,
    ) -> Result<usize, RouterError> {
        let store_dir = store_dir.as_ref().to_path_buf();
        let registry = Registry::open_store(&store_dir)?;
        self.registry = registry;
        self.store_dir = Some(store_dir);
        self.tile_cache.clear();
        self.refresh_store()
    }

    /// Synchronize the registry with `<store_dir>/partitions/*/manifest.json`, reload the
    /// partition list, preload any new `overview_1hz.parquet` and `states.parquet` files into
    /// `TileCache`, and update `MasterTimeline` day bounds.
    pub fn refresh_store(&mut self) -> Result<usize, RouterError> {
        let Some(store_dir) = self.store_dir.clone() else {
            return Ok(0);
        };
        let _ = self.registry.sync_from_store_dir(&store_dir)?;
        let days = self.registry.list_track_days()?;
        let mut all_partitions = Vec::new();
        for day in &days {
            let mut day_parts = self.registry.list_partitions_for_day(&day.id)?;
            all_partitions.append(&mut day_parts);
        }
        all_partitions.sort_by(|a, b| {
            a.start_utc_ms
                .cmp(&b.start_utc_ms)
                .then_with(|| a.partition_id.cmp(&b.partition_id))
        });

        let newly_preloaded = self
            .tile_cache
            .preload_registered_partitions(&store_dir, &all_partitions)?;

        for part in &all_partitions {
            self.timeline
                .observe_day_span_ms(part.start_utc_ms, part.end_utc_ms);
        }
        if self.timeline.active_track_day_id().is_none()
            && let Some(last_day) = days.last()
        {
            self.timeline
                .set_active_track_day_id(Some(last_day.id.clone()));
        }

        self.partitions = all_partitions;
        Ok(newly_preloaded)
    }

    /// Register a newly written [`PartitionManifest`] and preload its 1Hz overview and states.
    pub fn register_and_preload_manifest(
        &mut self,
        manifest: &PartitionManifest,
        partition_dir: impl AsRef<path::Path>,
    ) -> Result<(), RouterError> {
        self.registry.register_manifest(manifest, true)?;
        self.tile_cache.preload_partition(
            &manifest.partition_id,
            partition_dir.as_ref(),
            Some(&manifest.signals),
        )?;
        if let Some(day_parts) = self.registry.get_partition(&manifest.partition_id)? {
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
        }
        Ok(())
    }

    pub fn set_repaint_callback(&self, callback: Option<RepaintCallback>) {
        self.tile_cache.set_repaint_callback(callback);
    }

    pub fn store_dir(&self) -> Option<&path::Path> {
        self.store_dir.as_deref()
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    pub fn partitions(&self) -> &[RegisteredPartition] {
        &self.partitions
    }

    pub fn live_buffer(&self) -> &LiveColumnBuffer {
        &self.live_buffer
    }

    pub fn live_buffer_mut(&mut self) -> &mut LiveColumnBuffer {
        &mut self.live_buffer
    }

    pub fn tile_cache(&self) -> &TileCache {
        &self.tile_cache
    }

    pub fn tile_cache_mut(&mut self) -> &mut TileCache {
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
        self.formula_engine
            .evaluate_expr_with(expr, &TargetGrid::Union, resolution, |raw_id| {
                self.query_raw_signal(raw_id, range.clone(), resolution)
            })
    }

    /// Synchronously evaluate an arbitrary [`Expr`] across `range` at exact `resolution`
    /// (blocking if any referenced raw partition column is not yet cached in `TileCache`).
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

        let mut prefetched = std::collections::BTreeMap::new();
        for raw_id in raw_ids {
            let series = self.query_raw_signal_blocking(&raw_id, range.clone(), resolution)?;
            prefetched.insert(raw_id, series);
        }

        self.formula_engine
            .evaluate_expr_with(expr, &TargetGrid::Union, resolution, |raw_id| {
                prefetched
                    .get(raw_id)
                    .cloned()
                    .unwrap_or_else(|| SignalSeries::empty_for(resolution))
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

    /// Synchronously query `signal_id` (raw or derived) re-indexed against cumulative track
    /// distance integrated from `speed_signal_id` over `range` at exact `resolution`.
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
        self.timeline.observe(timestamp)
    }

    /// Ingest a batch of live [`ParsedFrame`]s into `live_buffer` and advance `timeline`.
    pub fn push_batch(&mut self, frames: Vec<ParsedFrame>) {
        let latest = frames.iter().map(|f| f.timestamp).max();
        self.live_buffer.push_batch(frames);
        if let Some(timestamp) = latest {
            self.timeline.observe(timestamp);
        }
    }

    /// Clear live provisional history and reset the timeline to `now` while preserving
    /// registered canonical SD partitions.
    pub fn reset_live(&mut self, now: Time) {
        self.live_buffer.clear();
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
            if part.is_local {
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

    /// Return the maximum `end_utc_ms` among local canonical SD partitions that start at or before `up_to_utc_ms`.
    fn max_canonical_end_ms(&self, up_to_utc_ms: i64) -> Option<i64> {
        self.partitions
            .iter()
            .filter(|p| p.is_local && p.start_utc_ms <= up_to_utc_ms)
            .map(|p| p.end_utc_ms)
            .max()
    }

    /// Query a raw CAN `signal_id` directly from `TileCache` and `LiveColumnBuffer` without
    /// checking `FormulaEngine`.
    pub fn query_raw_signal(
        &self,
        signal_id: &str,
        range: ops::RangeInclusive<i64>,
        resolution: Resolution,
    ) -> SignalSeries {
        self.tile_cache.poll_completed();
        if range.is_empty() {
            return SignalSeries::empty_for(resolution);
        }

        let canonical_id = canonicalize_signal_ref(signal_id);
        let start_ms = *range.start();
        let end_ms = *range.end();

        let overlapping_ids: Vec<String> = self
            .partitions
            .iter()
            .filter(|p| p.is_local && p.start_utc_ms <= end_ms && p.end_utc_ms >= start_ms)
            .map(|p| p.partition_id.clone())
            .collect();

        let mut chunks = Vec::new();
        for part_id in &overlapping_ids {
            if let Some(lookup) = self
                .tile_cache
                .query_signal(part_id, &canonical_id, resolution)
            {
                let sliced = lookup.series().slice_range(range.clone());
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

        SignalSeries::stitch(chunks, resolution)
    }

    /// Query `signal_id` (raw CAN signal or derived formula channel) across `range`
    /// (`[start_utc_ms..=end_utc_ms]`) at `resolution` without blocking the calling thread.
    ///
    /// Canonical SD partitions take precedence over `live_buffer`. If `range.end()` extends past
    /// the latest canonical SD partition into the live buffer's range, live samples for
    /// `t > max_partition_end_utc_ms` are appended seamlessly.
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

        if let Some(compiled) = self.formula_engine.get(signal_id) {
            let ast = compiled.ast.clone();
            return self
                .evaluate_expression(&ast, range, resolution)
                .unwrap_or_else(|_| SignalSeries::empty_for(resolution));
        }

        self.query_raw_signal(signal_id, range, resolution)
    }

    /// Synchronously query a raw CAN `signal_id` across `range` at exact `resolution`
    /// (blocking if any overlapping partition column is not yet cached in `TileCache`).
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

        let overlapping_ids: Vec<String> = self
            .partitions
            .iter()
            .filter(|p| p.is_local && p.start_utc_ms <= end_ms && p.end_utc_ms >= start_ms)
            .map(|p| p.partition_id.clone())
            .collect();

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
    /// at exact `resolution` (blocking if any overlapping partition column is not yet cached in `TileCache`).
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
            if !part.is_local || part.start_utc_ms > end_ms || part.end_utc_ms < start_ms {
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
            let lookback_ms = (self.formula_engine.max_staleness_ms() * 8).max(2_000);
            let window = target_utc_ms.saturating_sub(lookback_ms)..=target_utc_ms;
            let series = self.query_signal(signal_id, window, Resolution::FullRes);
            return series.latest_at_or_before(target_utc_ms);
        }

        let canonical_id = canonicalize_signal_ref(signal_id);
        let max_canonical = self.max_canonical_end_ms(target_utc_ms);

        // If target_utc_ms is beyond all canonical partitions (or no partitions exist),
        // check the provisional live buffer first.
        if max_canonical.is_none_or(|max_end| target_utc_ms > max_end)
            && let Some((live_t, live_v)) = self
                .live_buffer
                .latest_signal_value_at(&canonical_id, target_utc_ms)
            && max_canonical.is_none_or(|max_end| live_t > max_end)
        {
            return Some((live_t, live_v));
        }

        // Otherwise search canonical partitions in reverse chronological order.
        for part in self.partitions.iter().rev() {
            if !part.is_local || part.start_utc_ms > target_utc_ms {
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

    /// Return the data coverage spans (`CanonicalSd` and `LiveProvisional`) overlapping `range`.
    pub fn coverage_spans(&self, range: ops::RangeInclusive<i64>) -> Vec<CoverageSpan> {
        if range.is_empty() {
            return Vec::new();
        }
        let start_ms = *range.start();
        let end_ms = *range.end();
        let mut spans = Vec::new();

        for part in &self.partitions {
            if !part.is_local || part.start_utc_ms > end_ms || part.end_utc_ms < start_ms {
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
        Ok(self.registry.list_track_days()?)
    }

    /// Select a specific `TrackDay` (or `None` for all captured days), recomputing `day_bounds_ms`
    /// and snapping the viewport `[start, end]` to the selected day's bounds.
    pub fn select_track_day(
        &mut self,
        track_day_id: Option<String>,
    ) -> Result<Option<(i64, i64)>, RouterError> {
        self.timeline.set_active_track_day_id(track_day_id.clone());
        self.timeline.clear_selected_annotations();

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
                    && let Some(day) = self.registry.get_track_day(day_id)?
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
        if let Some((lo, hi)) = bounds {
            self.timeline.snap_to_range_ms(lo, hi);
        }
        Ok(bounds)
    }

    /// Return normalized `(bucket_utc_ms, normalized_intensity)` (`0.0..=1.0`) 1Hz activity buckets
    /// across `range` from preloaded partition overviews and the live buffer.
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
            if !part.is_local || part.start_utc_ms > end_ms || part.end_utc_ms < start_ms {
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

    /// Resolve the clock correlation provenance (`GPS Synced`, `RTC Estimated`, `Mtime Estimated`,
    /// or `Live UDP`) at `target_utc_ms` (falling back to the active viewport if `target_utc_ms`
    /// sits between partitions).
    pub fn clock_provenance_at(&self, target_utc_ms: i64) -> Option<ClockProvenance> {
        // 1. Exact containment inside a canonical SD partition
        for part in &self.partitions {
            if part.is_local
                && part.start_utc_ms <= target_utc_ms
                && target_utc_ms <= part.end_utc_ms
            {
                return Some(ClockProvenance::CanonicalPartition(
                    part.clock_mapping.clone(),
                ));
            }
        }

        // 2. If in live mode or covered by live buffer beyond canonical partitions
        let max_canonical = self.max_canonical_end_ms(target_utc_ms);
        if let Some((live_min, live_max)) = self.live_buffer.time_bounds_ms()
            && max_canonical.is_none_or(|max_end| target_utc_ms > max_end)
            && target_utc_ms >= live_min
            && target_utc_ms <= live_max
        {
            return Some(ClockProvenance::LiveProvisional);
        }

        // 3. Nearest canonical partition overlapping the active timeline viewport
        let win_start = self.timeline.start_utc_ms();
        let win_end = self.timeline.end_utc_ms();
        if let Some(part) = self
            .partitions
            .iter()
            .filter(|p| p.is_local && p.start_utc_ms <= win_end && p.end_utc_ms >= win_start)
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
        Ok(self
            .registry
            .query_annotations_in_range(*range.start(), *range.end(), false)?)
    }

    /// Insert or update an annotation in the registry and expand day bounds if needed.
    pub fn upsert_annotation(&mut self, annotation: &Annotation) -> Result<bool, RouterError> {
        let changed = self.registry.upsert_annotation(annotation)?;
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

    /// Soft-delete an annotation in the registry and remove it from active timeline selection.
    pub fn soft_delete_annotation(
        &mut self,
        annotation_id: &str,
        updated_at_ms: i64,
    ) -> Result<bool, RouterError> {
        let deleted = self
            .registry
            .soft_delete_annotation(annotation_id, updated_at_ms)?;
        if self.timeline.is_annotation_selected(annotation_id) {
            self.timeline
                .toggle_comparison_annotation(annotation_id.to_owned(), 0, None);
        }
        Ok(deleted)
    }
}

use std::io;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{FrameKind, ParsedFrame};
    use crate::log_parse::{correlate, parse};
    use crate::storage::write_correlated_epochs_to_store;
    use crate::superdbc::database::Database;
    use std::fs;

    fn router_test_database() -> Database {
        let json = r#"{
          "content_hash": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
          "versions": { "schema_version": "1.1", "hash": "test_v1" },
          "buses": {
            "VCAN": {
              "bus_id": 0,
              "baud_rate": 500000,
              "nodes": [{ "name": "MAIN_MODULE", "is_external": false }],
              "messages": [
                {
                  "id": 100,
                  "is_extended_id": false,
                  "message_name": "wheel_speeds",
                  "transmitter": "MAIN_MODULE",
                  "receivers": [],
                  "length_bytes": 8,
                  "nominal_period_ms": 100,
                  "priority": 1,
                  "description": "Wheel speed",
                  "signals": [
                    {
                      "signal_name": "speed_mps",
                      "description": "",
                      "data_type": "uint16_t",
                      "raw_type": "unsigned",
                      "start_bit": 0,
                      "bit_length": 16,
                      "byte_order": "little_endian",
                      "scale": 0.1,
                      "offset": 0.0,
                      "limits": null,
                      "unit": "m/s",
                      "choices": null,
                      "display_format": null
                    }
                  ]
                },
                {
                  "id": 200,
                  "is_extended_id": false,
                  "message_name": "main_hb",
                  "transmitter": "MAIN_MODULE",
                  "receivers": [],
                  "length_bytes": 1,
                  "nominal_period_ms": 100,
                  "priority": 2,
                  "description": "Main heartbeat",
                  "signals": [
                    {
                      "signal_name": "car_state",
                      "description": "",
                      "data_type": "uint8_t",
                      "raw_type": "unsigned",
                      "start_bit": 0,
                      "bit_length": 8,
                      "byte_order": "little_endian",
                      "scale": 1.0,
                      "offset": 0.0,
                      "limits": null,
                      "unit": "",
                      "choices": { "0": "IDLE", "1": "READY", "2": "FAULT" },
                      "display_format": null
                    }
                  ]
                }
              ]
            }
          }
        }"#;
        Database::from_json(json).expect("valid test superdbc")
    }

    fn make_parsed_msg(
        db: &Database,
        ticks_ms: u32,
        source_file_idx: usize,
        id: u32,
        values: &[(&str, f64)],
    ) -> parse::ParsedMessage {
        let payload = db.encode(0, id, values).unwrap();
        let decoded = db.decode(0, id, &payload).unwrap();
        parse::ParsedMessage {
            timestamp: ticks_ms,
            bus_name: "VCAN".to_owned(),
            source_file_idx,
            decoded,
        }
    }

    fn make_live_frame(db: &Database, utc_ms: i64, id: u32, values: &[(&str, f64)]) -> ParsedFrame {
        let payload = db.encode(0, id, values).unwrap();
        let decoded = db.decode(0, id, &payload).ok();
        ParsedFrame {
            timestamp: Time::from_unix_millis(utc_ms),
            bus_id: 0,
            kind: FrameKind::Data,
            dlc: payload.len() as u8,
            identity: id,
            raw_bytes: payload,
            decoded,
        }
    }

    /// Milestone 3 Verification:
    /// Load two separate SD partitions (`10:00–10:15` and `10:45–11:00`) plus live frames at `11:05`,
    /// and verify `VirtualTimelineRouter` seamlessly queries across the entire `10:00–11:06` span
    /// at `Overview1Hz`, `Overview10Hz`, and `FullRes`, while enforcing Canonical SD > Live precedence.
    #[test]
    fn router_queries_across_two_sd_partitions_and_live_buffer_at_all_resolutions() {
        let db = router_test_database();
        let tmp_dir =
            std::env::temp_dir().join(format!("daq_router_m3_verify_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&tmp_dir).unwrap();

        // Anchor 10:00:00 UTC on 2026-11-12
        let t_10_00_ms: i64 = 1_794_477_600_000;
        let min_ms: i64 = 60_000;

        // Partition 1: 10:00 ..= 10:14 (every 60s, speed = 10.0 m/s, state = 0 IDLE)
        let mut epoch1_msgs = Vec::new();
        for minute in 0..15u32 {
            let ticks_ms = minute * 60_000;
            epoch1_msgs.push(make_parsed_msg(
                &db,
                ticks_ms,
                0,
                100,
                &[("speed_mps", 10.0)],
            ));
            epoch1_msgs.push(make_parsed_msg(
                &db,
                ticks_ms,
                0,
                200,
                &[("car_state", 0.0)],
            ));
        }

        // Partition 2: 10:45 ..= 10:59 (every 60s, speed = 25.0 m/s, state = 1 READY)
        let t_10_45_ms = t_10_00_ms + 45 * min_ms;
        let mut epoch2_msgs = Vec::new();
        for minute in 0..15u32 {
            let ticks_ms = minute * 60_000;
            epoch2_msgs.push(make_parsed_msg(
                &db,
                ticks_ms,
                1,
                100,
                &[("speed_mps", 25.0)],
            ));
            epoch2_msgs.push(make_parsed_msg(
                &db,
                ticks_ms,
                1,
                200,
                &[("car_state", 1.0)],
            ));
        }

        let log_files = vec![
            parse::SourceLogFile {
                file_name: "log-2026-11-12--10-00-00.log".to_owned(),
                sha256_hex: "11111111".repeat(8),
                rtc_timestamp_ms: Some(t_10_00_ms),
                mtime_unix_ms: t_10_00_ms,
            },
            parse::SourceLogFile {
                file_name: "log-2026-11-12--10-45-00.log".to_owned(),
                sha256_hex: "22222222".repeat(8),
                rtc_timestamp_ms: Some(t_10_45_ms),
                mtime_unix_ms: t_10_45_ms,
            },
        ];

        let epochs = vec![
            correlate::CorrelationChunkResult {
                parsed_msgs: epoch1_msgs,
                clock_mapping: correlate::ClockMapping {
                    slope: 1.0,
                    intercept_ms: t_10_00_ms as f64,
                    confidence: correlate::ClockConfidence::RtcEstimated,
                },
            },
            correlate::CorrelationChunkResult {
                parsed_msgs: epoch2_msgs,
                clock_mapping: correlate::ClockMapping {
                    slope: 1.0,
                    intercept_ms: t_10_45_ms as f64,
                    confidence: correlate::ClockConfidence::RtcEstimated,
                },
            },
        ];

        let summary = write_correlated_epochs_to_store(&tmp_dir, &log_files, epochs, &db).unwrap();
        assert_eq!(summary.partitions_written, 2);

        let t_11_05_ms = t_10_00_ms + 65 * min_ms;
        let t_11_06_ms = t_10_00_ms + 66 * min_ms;

        let mut router =
            VirtualTimelineRouter::open_store(&tmp_dir, Time::from_unix_millis(t_11_05_ms), 30.0)
                .expect("open store");
        assert_eq!(router.partitions().len(), 2);

        // Also push a provisional live frame inside Partition 2's window (10:50) with a bogus speed (99.0 m/s)
        // to verify Canonical SD > Live Buffer precedence, plus real live frames at 11:05 (40.0 m/s, state = 2 FAULT).
        let t_10_50_ms = t_10_00_ms + 50 * min_ms;
        router.ingest_frame(make_live_frame(
            &db,
            t_10_50_ms,
            100,
            &[("speed_mps", 99.0)],
        ));
        router.ingest_frame(make_live_frame(
            &db,
            t_11_05_ms,
            100,
            &[("speed_mps", 40.0)],
        ));
        router.ingest_frame(make_live_frame(&db, t_11_05_ms, 200, &[("car_state", 2.0)]));

        let full_span = t_10_00_ms..=t_11_06_ms;

        // 1. Query at Overview1Hz (resident in RAM)
        let series_1hz = router.query_signal(
            "wheel_speeds.speed_mps",
            full_span.clone(),
            Resolution::Overview1Hz,
        );
        match series_1hz {
            SignalSeries::Envelope(pts) => {
                // 15 points in Partition 1 + 15 points in Partition 2 + 1 live point at 11:05 = 31 buckets
                // (Notice the bogus 99.0 m/s live frame at 10:50 is ignored because Partition 2 covers 10:45..10:59!)
                assert_eq!(pts.len(), 31);
                assert!((pts[0].mean - 10.0).abs() < 1e-6);
                assert!((pts[14].mean - 10.0).abs() < 1e-6);
                assert!((pts[15].mean - 25.0).abs() < 1e-6);
                assert!((pts[29].mean - 25.0).abs() < 1e-6);
                assert_eq!(pts[30].utc_ms, t_11_05_ms);
                assert!((pts[30].mean - 40.0).abs() < 1e-6);
            }
            other => panic!("Expected Overview1Hz Envelope, got {other:?}"),
        }

        // 2. Query at Overview10Hz (triggers async load on first call, then Ready after wait_for_in_flight)
        let _cold_10hz = router.query_signal(
            "wheel_speeds.speed_mps",
            full_span.clone(),
            Resolution::Overview10Hz,
        );
        router.wait_for_in_flight(time::Duration::from_secs(2));
        let warm_10hz = router.query_signal(
            "wheel_speeds.speed_mps",
            full_span.clone(),
            Resolution::Overview10Hz,
        );
        match warm_10hz {
            SignalSeries::Envelope(pts) => {
                assert_eq!(pts.len(), 31);
                assert!((pts[0].mean - 10.0).abs() < 1e-6);
                assert!((pts[15].mean - 25.0).abs() < 1e-6);
                assert!((pts[30].mean - 40.0).abs() < 1e-6);
            }
            other => panic!("Expected Overview10Hz Envelope, got {other:?}"),
        }

        // 3. Query at FullRes (triggers async load on first call, then Exact after wait_for_in_flight)
        let _cold_full = router.query_signal(
            "wheel_speeds.speed_mps",
            full_span.clone(),
            Resolution::FullRes,
        );
        router.wait_for_in_flight(time::Duration::from_secs(2));
        let warm_full = router.query_signal(
            "wheel_speeds.speed_mps",
            full_span.clone(),
            Resolution::FullRes,
        );
        match warm_full {
            SignalSeries::Exact {
                timestamps_ms,
                values,
            } => {
                assert_eq!(timestamps_ms.len(), 31);
                assert_eq!(timestamps_ms[0], t_10_00_ms);
                assert!((values[0] - 10.0).abs() < 1e-6);
                assert_eq!(timestamps_ms[15], t_10_45_ms);
                assert!((values[15] - 25.0).abs() < 1e-6);
                assert_eq!(timestamps_ms[30], t_11_05_ms);
                assert!((values[30] - 40.0).abs() < 1e-6);
            }
            other => panic!("Expected FullRes Exact, got {other:?}"),
        }

        // 4. Verify discrete state intervals across Partition 1, Partition 2, and Live
        let states = router.query_states("main_hb.car_state", full_span.clone());
        assert_eq!(states.len(), 3);
        assert_eq!(states[0].label, "IDLE");
        assert_eq!(states[1].label, "READY");
        assert_eq!(states[2].label, "FAULT");
        assert_eq!(states[2].start_utc_ms, t_11_05_ms);

        // 5. Verify coverage spans report 2 CanonicalSd spans + 1 LiveProvisional span
        let spans = router.coverage_spans(full_span);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].source, DataSourceKind::CanonicalSd);
        assert_eq!(spans[1].source, DataSourceKind::CanonicalSd);
        assert_eq!(spans[2].source, DataSourceKind::LiveProvisional);

        let _ = fs::remove_dir_all(&tmp_dir);
    }

    /// Real PER26 Dataset Validation:
    /// Imports real SD card `.log` files from `PER26/2026-09-06/1_no_cooling/raw` and
    /// `PER26/2026-09-06/2_regen/raw` using `superdbc/superdbc.json`, opens `VirtualTimelineRouter`,
    /// and verifies multi-partition queries across the multi-hour day span at `Overview1Hz`,
    /// `Overview10Hz`, and `FullRes`, plus hot partition registration via `reload_partitions`.
    #[test]
    fn router_validates_against_per26_real_datasets() {
        let manifest_dir = path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let repo_root = manifest_dir
            .parent()
            .and_then(|p| p.parent())
            .expect("repo root");
        let per26_dbc = repo_root.join("superdbc/superdbc_per26.json");
        let dbc_path = if per26_dbc.exists() {
            per26_dbc
        } else {
            repo_root.join("superdbc/superdbc_1e6fa38.json")
        };
        let run1_raw = repo_root.join("PER26/2026-09-06/1_no_cooling/raw");
        let run2_raw = repo_root.join("PER26/2026-09-06/2_regen/raw");
        let run3_raw = repo_root.join("PER26/2026-09-06/3_happy_laps_endurance_regen/raw");

        if !dbc_path.exists() || !run1_raw.exists() || !run2_raw.exists() {
            eprintln!(
                "Skipping router_validates_against_per26_real_datasets: local PER26/ and superdbc/ fixtures not present"
            );
            return;
        }

        let db = Database::load(&dbc_path).expect("load real superdbc.json");

        let tmp_dir =
            std::env::temp_dir().join(format!("daq_router_per26_verify_{}", uuid::Uuid::new_v4()));
        let staging_run1 = tmp_dir.join("run1_logs");
        let staging_run2 = tmp_dir.join("run2_logs");
        let staging_run3 = tmp_dir.join("run3_logs");
        let store_dir = tmp_dir.join("store");
        fs::create_dir_all(&staging_run1).unwrap();
        fs::create_dir_all(&staging_run2).unwrap();
        fs::create_dir_all(&staging_run3).unwrap();

        // Stage real log slices from Run 1 (18:36 UTC) and Run 2 (20:21 UTC)
        for name in [
            "log-2026-09-06--18-36-02.log",
            "log-2026-09-06--18-37-02.log",
        ] {
            let src = run1_raw.join(name);
            let dst = staging_run1.join(name);
            if fs::hard_link(&src, &dst).is_err() {
                fs::copy(&src, &dst).unwrap();
            }
        }
        for name in [
            "log-2026-09-06--20-21-44.log",
            "log-2026-09-06--20-22-44.log",
        ] {
            let src = run2_raw.join(name);
            let dst = staging_run2.join(name);
            if fs::hard_link(&src, &dst).is_err() {
                fs::copy(&src, &dst).unwrap();
            }
        }

        let summary1 =
            crate::log_parse::import_logs_to_store(&staging_run1, &store_dir, &db).unwrap();
        let summary2 =
            crate::log_parse::import_logs_to_store(&staging_run2, &store_dir, &db).unwrap();
        assert!(summary1.partitions_written >= 1);
        assert!(summary2.partitions_written >= 1);

        let router = VirtualTimelineRouter::open_store(&store_dir, Time::now(), 30.0)
            .expect("open PER26 store");
        assert_eq!(
            router.partitions().len(),
            summary1.partitions_written + summary2.partitions_written
        );

        let (day_min_ms, day_max_ms) = router
            .timeline()
            .day_bounds_ms()
            .expect("global day bounds populated");
        assert!(
            day_max_ms - day_min_ms > 3_600_000,
            "Multi-run track day should span > 1 hour across Run 1 (18:36) and Run 2 (20:21)"
        );

        // Pick a signal present in both partitions and verify its exact sample count across the day
        let sig_entry_1 = summary1.manifests[0]
            .signals
            .iter()
            .find(|s| {
                s.sample_count > 100
                    && summary2.manifests[0]
                        .signals
                        .iter()
                        .any(|s2| s2.signal_id == s.signal_id && s2.sample_count > 100)
            })
            .expect("common high-rate signal across PER26 Run 1 and Run 2");
        let signal_id = sig_entry_1.signal_id.clone();

        let expected_total_samples: u64 = summary1
            .manifests
            .iter()
            .chain(summary2.manifests.iter())
            .flat_map(|m| m.signals.iter())
            .filter(|s| s.signal_id == signal_id)
            .map(|s| s.sample_count)
            .sum();

        let day_range = day_min_ms..=day_max_ms;

        // 1. Overview1Hz is preloaded in RAM and returns immediately
        let overview_1hz =
            router.query_signal(&signal_id, day_range.clone(), Resolution::Overview1Hz);
        match &overview_1hz {
            SignalSeries::Envelope(buckets) => {
                assert!(
                    !buckets.is_empty(),
                    "Overview1Hz should return preloaded buckets across PER26 day"
                );
                assert!(
                    buckets.iter().all(|b| b.min <= b.mean && b.mean <= b.max),
                    "All Overview1Hz envelope buckets must satisfy min <= mean <= max"
                );
                assert!(
                    buckets.windows(2).all(|w| w[0].utc_ms < w[1].utc_ms),
                    "Overview1Hz bucket timestamps must be strictly increasing"
                );
            }
            other => panic!("Expected Overview1Hz Envelope, got {other:?}"),
        }

        // 2. Cold FullRes query falls back non-blockingly to preloaded Overview1Hz on frame 1,
        // and once background reader finishes, returns Exact samples matching manifest sample_count.
        let cold_full = router.query_signal(&signal_id, day_range.clone(), Resolution::FullRes);
        assert!(
            matches!(cold_full, SignalSeries::Envelope(_)),
            "Cold FullRes query should fall back to preloaded overview without blocking UI"
        );

        router.wait_for_in_flight(time::Duration::from_secs(5));
        let warm_full = router.query_signal(&signal_id, day_range.clone(), Resolution::FullRes);
        match warm_full {
            SignalSeries::Exact {
                timestamps_ms,
                values,
            } => {
                assert_eq!(timestamps_ms.len() as u64, expected_total_samples);
                assert_eq!(values.len() as u64, expected_total_samples);
                assert!(timestamps_ms.windows(2).all(|w| w[0] <= w[1]));
            }
            other => panic!("Expected warm FullRes Exact series, got {other:?}"),
        }

        // 3. Hot partition registration via register_and_preload_manifest with Run 3 (03:00 UTC)
        let run3_file = run3_raw.join("log-2026-09-07--03-00-09.log");
        if run3_file.exists() {
            let dst = staging_run3.join("log-2026-09-07--03-00-09.log");
            if fs::hard_link(&run3_file, &dst).is_err() {
                fs::copy(&run3_file, &dst).unwrap();
            }
            let summary3 =
                crate::log_parse::import_logs_to_store(&staging_run3, &store_dir, &db).unwrap();
            assert!(summary3.partitions_written >= 1);

            let mut router = router;
            for manifest in &summary3.manifests {
                let partition_dir = store_dir.join("partitions").join(&manifest.partition_id);
                router
                    .register_and_preload_manifest(manifest, &partition_dir)
                    .expect("hot register and preload manifest");
            }
            assert_eq!(
                router.partitions().len(),
                summary1.partitions_written
                    + summary2.partitions_written
                    + summary3.partitions_written
            );
        }

        let _ = fs::remove_dir_all(&tmp_dir);
    }

    /// Milestone 4 Real PER26 Dataset Validation:
    /// Imports real SD card `.log` slices from `PER26/2026-09-06/1_no_cooling/raw` and
    /// `PER26/2026-09-06/2_regen/raw`, cross-validates `FormulaEngine` derived channels
    /// (`battery.voltage_v`, `battery.power_w`, `battery.energy_wh`, `vd.front_heave_mm`,
    /// `vd.fl_damper_vel_mms`, `vd.total_accel_g`, `thermals.max_igbt_temp_c`) against
    /// `PER26/2026-09-06/1_no_cooling/parsed/no_cooling_000_2026-09-06T18-34-51.252Z.csv`,
    /// and verifies gap-safe track-distance integration (`query_signal_vs_distance`) across stints.
    #[test]
    fn formula_engine_and_distance_integrator_validate_against_per26_datasets() {
        let manifest_dir = path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let repo_root = manifest_dir
            .parent()
            .and_then(|p| p.parent())
            .expect("repo root");
        let per26_dbc = repo_root.join("superdbc/superdbc_per26.json");
        let dbc_path = if per26_dbc.exists() {
            per26_dbc
        } else {
            repo_root.join("superdbc/superdbc_1e6fa38.json")
        };
        let run1_raw = repo_root.join("PER26/2026-09-06/1_no_cooling/raw");
        let run1_csv = repo_root.join(
            "PER26/2026-09-06/1_no_cooling/parsed/no_cooling_000_2026-09-06T18-34-51.252Z.csv",
        );
        let run2_raw = repo_root.join("PER26/2026-09-06/2_regen/raw");

        if !dbc_path.exists() || !run1_raw.exists() || !run1_csv.exists() || !run2_raw.exists() {
            eprintln!(
                "Skipping formula_engine_and_distance_integrator_validate_against_per26_datasets: local PER26/ and superdbc/ fixtures not present"
            );
            return;
        }

        let db = Database::load(&dbc_path).expect("load PER26 superdbc JSON");

        let tmp_dir =
            std::env::temp_dir().join(format!("daq_m4_per26_verify_{}", uuid::Uuid::new_v4()));
        let staging_run1 = tmp_dir.join("run1_logs");
        let staging_run2 = tmp_dir.join("run2_logs");
        let store_dir = tmp_dir.join("store");
        fs::create_dir_all(&staging_run1).unwrap();
        fs::create_dir_all(&staging_run2).unwrap();

        for name in [
            "log-2026-09-06--18-36-02.log",
            "log-2026-09-06--18-37-02.log",
        ] {
            let src = run1_raw.join(name);
            let dst = staging_run1.join(name);
            if fs::hard_link(&src, &dst).is_err() {
                fs::copy(&src, &dst).unwrap();
            }
        }
        for name in [
            "log-2026-09-06--20-21-44.log",
            "log-2026-09-06--20-22-44.log",
        ] {
            let src = run2_raw.join(name);
            let dst = staging_run2.join(name);
            if fs::hard_link(&src, &dst).is_err() {
                fs::copy(&src, &dst).unwrap();
            }
        }

        crate::log_parse::import_logs_to_store(&staging_run1, &store_dir, &db).unwrap();
        crate::log_parse::import_logs_to_store(&staging_run2, &store_dir, &db).unwrap();

        let mut router = VirtualTimelineRouter::open_store(&store_dir, Time::now(), 30.0)
            .expect("open PER26 store with default FormulaEngine");

        // 1. Verify all 21 default derived signals from daq/config/derived_signals.json are loaded
        let descriptors = router.formula_engine().definitions();
        assert_eq!(descriptors.len(), 21);
        for id in [
            "battery.voltage_v",
            "battery.power_w",
            "battery.power_kw",
            "battery.energy_wh",
            "vd.front_heave_mm",
            "vd.front_roll_displacement_mm",
            "vd.fl_damper_vel_mms",
            "vd.total_accel_g",
            "vd.understeer_gradient",
            "thermals.max_igbt_temp_c",
        ] {
            assert!(
                router.formula_engine().contains(id),
                "Expected default derived signal `{id}` to be registered"
            );
        }

        let (day_min_ms, day_max_ms) = router.timeline().day_bounds_ms().unwrap();
        let day_range = day_min_ms..=day_max_ms;

        // 2. Cross-validate battery.voltage_v and battery.power_w against PER26 Run 1 parsed CSV
        // Extract the first 5 pack_stats rows from where log-2026-09-06--18-36-02.log begins (daq_ts >= 60.201).
        let csv_expected_samples = {
            use std::io::{BufRead, BufReader};
            let file = fs::File::open(&run1_csv).expect("open PER26 Run 1 CSV");
            let mut lines = BufReader::new(file).lines();
            let _buses = lines.next().unwrap().unwrap();
            let _nodes = lines.next().unwrap().unwrap();
            let msgs: Vec<String> = lines
                .next()
                .unwrap()
                .unwrap()
                .split(',')
                .map(|s| s.to_string())
                .collect();
            let _msg_desc = lines.next().unwrap().unwrap();
            let sigs: Vec<String> = lines
                .next()
                .unwrap()
                .unwrap()
                .split(',')
                .map(|s| s.to_string())
                .collect();
            let _sig_desc = lines.next().unwrap().unwrap();
            let _units = lines.next().unwrap().unwrap();

            let mut vbatt_col = None;
            let mut curr_col = None;
            for i in 3..msgs.len().min(sigs.len()) {
                let key = format!("{}.{}", msgs[i], sigs[i]);
                if key == "pack_stats.vbatt_adc" {
                    vbatt_col = Some(i);
                } else if key == "pack_stats.pack_current" {
                    curr_col = Some(i);
                }
            }
            let vbatt_col = vbatt_col.expect("pack_stats.vbatt_adc column in CSV");
            let curr_col = curr_col.expect("pack_stats.pack_current column in CSV");

            let mut samples = Vec::new();
            for line in lines {
                let line = line.unwrap();
                let cols: Vec<&str> = line.split(',').collect();
                if cols.len() <= vbatt_col.max(curr_col) {
                    continue;
                }
                let Ok(daq_ts) = cols[1].parse::<f64>() else {
                    continue;
                };
                if daq_ts < 60.201 {
                    continue;
                }
                if !cols[vbatt_col].is_empty() && !cols[curr_col].is_empty() {
                    let vbatt: f64 = cols[vbatt_col].parse().unwrap();
                    let curr: f64 = cols[curr_col].parse().unwrap();
                    samples.push((vbatt, curr));
                    if samples.len() >= 5 {
                        break;
                    }
                }
            }
            samples
        };
        assert_eq!(csv_expected_samples.len(), 5);

        // Query FullRes for battery.voltage_v and battery.power_w across Run 1
        let voltage_series = router
            .query_signal_blocking("battery.voltage_v", day_range.clone(), Resolution::FullRes)
            .expect("query battery.voltage_v FullRes");
        let power_series = router
            .query_signal_blocking("battery.power_w", day_range.clone(), Resolution::FullRes)
            .expect("query battery.power_w FullRes");

        match (&voltage_series, &power_series) {
            (
                SignalSeries::Exact {
                    timestamps_ms: v_ts,
                    values: v_vals,
                },
                SignalSeries::Exact {
                    timestamps_ms: p_ts,
                    values: p_vals,
                },
            ) => {
                assert_eq!(v_ts.len(), p_ts.len());
                assert!(v_vals.len() > 10_000);
                // Verify first 5 samples of Run 1 match the CSV and plot_factory equations exactly
                for (i, &(csv_vbatt, csv_curr)) in csv_expected_samples.iter().enumerate() {
                    let expected_v = csv_vbatt * 0.14659808276;
                    let expected_p = expected_v * csv_curr;
                    assert!(
                        (v_vals[i] - expected_v).abs() < 1e-6,
                        "Sample {i} voltage mismatch: router={} vs CSV plot_factory={}",
                        v_vals[i],
                        expected_v
                    );
                    assert!(
                        (p_vals[i] - expected_p).abs() < 1e-6,
                        "Sample {i} power mismatch: router={} vs CSV plot_factory={}",
                        p_vals[i],
                        expected_p
                    );
                }
            }
            other => panic!("Expected FullRes Exact for voltage and power, got {other:?}"),
        }

        // 3. Verify Overview1Hz and Overview10Hz for derived channels across the day
        for sig in [
            "battery.voltage_v",
            "battery.power_w",
            "vd.front_heave_mm",
            "vd.total_accel_g",
            "thermals.max_igbt_temp_c",
        ] {
            let ov1 = router.query_signal(sig, day_range.clone(), Resolution::Overview1Hz);
            match ov1 {
                SignalSeries::Envelope(buckets) => {
                    assert!(
                        !buckets.is_empty(),
                        "Overview1Hz for `{sig}` should be non-empty"
                    );
                    assert!(
                        buckets
                            .iter()
                            .all(|b| b.min.is_finite() && b.mean.is_finite() && b.max.is_finite()),
                        "Overview1Hz buckets for `{sig}` must not contain NaN"
                    );
                }
                other => panic!("Expected Overview1Hz Envelope for `{sig}`, got {other:?}"),
            }
        }

        // 4. Verify calculus/window derived signals (energy integral + damper velocity derivative)
        let energy_series = router
            .query_signal_blocking("battery.energy_wh", day_range.clone(), Resolution::FullRes)
            .expect("query battery.energy_wh FullRes");
        if let SignalSeries::Exact {
            timestamps_ms,
            values,
        } = energy_series
        {
            assert!(!values.is_empty());
            // Find the ~1h45m gap between Run 1 (18:38 UTC) and Run 2 (20:21 UTC)
            let gap_idx = timestamps_ms
                .windows(2)
                .position(|w| w[1] - w[0] > 3_600_000)
                .expect("multi-hour gap between PER26 Run 1 and Run 2");
            // Energy must not integrate across the 1h45m gap (resets to 0.0 Wh at start of new stint)!
            let energy_before_gap = values[gap_idx];
            let energy_after_gap = values[gap_idx + 1];
            assert!(
                energy_before_gap > 10.0 && energy_before_gap < 500.0,
                "Expected realistic Run 1 cumulative energy (10..500 Wh), got {energy_before_gap}"
            );
            assert!(
                energy_after_gap.abs() < 1e-9,
                "battery.energy_wh must reset to 0.0 Wh across multi-hour gap instead of integrating across gap: before={}, after={}",
                energy_before_gap,
                energy_after_gap
            );
        } else {
            panic!("Expected Exact series for battery.energy_wh");
        }

        let damper_vel = router
            .query_signal_blocking(
                "vd.fl_damper_vel_mms",
                day_range.clone(),
                Resolution::FullRes,
            )
            .expect("query vd.fl_damper_vel_mms FullRes");
        if let SignalSeries::Exact { values, .. } = damper_vel {
            let finite_count = values.iter().filter(|v| v.is_finite()).count();
            assert!(
                finite_count > 5_000,
                "Expected > 5,000 finite damper velocity samples across PER26 stints"
            );
        } else {
            panic!("Expected Exact series for vd.fl_damper_vel_mms");
        }

        // 5. Register a custom runtime derived signal via upsert_formula and verify against GPS speed
        router
            .formula_engine_mut()
            .upsert_formula(crate::analysis::DerivedSignalDef {
                id: "vd.gps_speed_hypot_mps".to_string(),
                expression: "hypot(gps_velocity.gps_vel_e, gps_velocity.gps_vel_n)".to_string(),
                unit: "m/s".to_string(),
                label: "GPS Ground Speed (E/N Hypot)".to_string(),
            })
            .expect("upsert custom GPS speed formula");

        let custom_gps = router
            .query_signal_blocking(
                "vd.gps_speed_hypot_mps",
                day_range.clone(),
                Resolution::FullRes,
            )
            .expect("query custom vd.gps_speed_hypot_mps FullRes");
        if let SignalSeries::Exact { values, .. } = custom_gps {
            let max_speed = values
                .iter()
                .copied()
                .filter(|v| v.is_finite())
                .fold(0.0_f64, f64::max);
            assert!(
                max_speed > 10.0 && max_speed < 40.0,
                "Expected realistic FSAE peak GPS speed (10..40 m/s), got {max_speed}"
            );
        } else {
            panic!("Expected Exact series for custom vd.gps_speed_hypot_mps");
        }

        // 6. Validate track-distance axis integration across PER26 Run 1 and Run 2
        let shock_vs_dist = router
            .query_signal_vs_distance_blocking(
                "front_shockpots.left",
                "gps_speed_heading.gps_speed",
                day_range.clone(),
                Resolution::FullRes,
            )
            .expect("query front_shockpots.left vs track distance");
        assert!(shock_vs_dist.timestamps_ms.len() > 10_000);
        assert!(
            shock_vs_dist.distances_m.windows(2).all(|w| w[1] >= w[0]),
            "Track distance axis must be monotonically non-decreasing"
        );
        assert!(
            shock_vs_dist.total_distance_m() > 200.0,
            "Expected > 200 m of cumulative track distance across PER26 stints, got {} m",
            shock_vs_dist.total_distance_m()
        );

        // Verify zero phantom distance accumulated across the ~1h45m gap between Run 1 and Run 2
        let dist_gap_idx = shock_vs_dist
            .timestamps_ms
            .windows(2)
            .position(|w| w[1] - w[0] > 3_600_000)
            .expect("multi-hour gap in distance series");
        assert!(
            (shock_vs_dist.distances_m[dist_gap_idx + 1] - shock_vs_dist.distances_m[dist_gap_idx])
                .abs()
                < 1e-9,
            "Distance integrator must pause across multi-hour gap between stints"
        );

        // Also verify plotting a derived signal (vd.front_heave_mm) against a derived speed signal
        let heave_vs_dist = router
            .query_signal_vs_distance_blocking(
                "vd.front_heave_mm",
                "vd.gps_speed_hypot_mps",
                day_range,
                Resolution::FullRes,
            )
            .expect("query derived heave vs derived GPS speed distance");
        assert!(
            (heave_vs_dist.total_distance_m() - shock_vs_dist.total_distance_m()).abs() < 15.0,
            "Distance integrated from hypot(vel_e, vel_n) ({:.1} m) should closely match gps_speed ({:.1} m)",
            heave_vs_dist.total_distance_m(),
            shock_vs_dist.total_distance_m()
        );

        let _ = fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn router_activity_envelope_clock_provenance_and_annotation_crud() {
        let db = router_test_database();
        let tmp_dir =
            std::env::temp_dir().join(format!("daq_router_m5_verify_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&tmp_dir).unwrap();

        let p1_start_ms: i64 = 1_794_477_600_000;
        let files = vec![parse::SourceLogFile {
            file_name: "log-2026-11-12--10-00-00.log".to_owned(),
            rtc_timestamp_ms: Some(p1_start_ms),
            mtime_unix_ms: p1_start_ms,
            sha256_hex: "aa11".repeat(16),
        }];
        let mut epoch1_msgs = Vec::new();
        for sec in 0..10u32 {
            epoch1_msgs.push(make_parsed_msg(
                &db,
                sec * 1000,
                0,
                100,
                &[("speed_mps", 10.0)],
            ));
        }
        let correlated = correlate::time_correlate_chunks(vec![epoch1_msgs], &files);
        write_correlated_epochs_to_store(&tmp_dir, &files, correlated, &db).unwrap();

        let mut router = VirtualTimelineRouter::open_store(
            &tmp_dir,
            Time::from_unix_millis(p1_start_ms + 20_000),
            30.0,
        )
        .unwrap();

        // Add live frame at p1_start_ms + 20_000
        router.ingest_frame(make_live_frame(
            &db,
            p1_start_ms + 20_000,
            100,
            &[("speed_mps", 15.0)],
        ));

        // 1Hz activity envelope covers both the 10-second partition and the live bucket
        let env = router.activity_envelope_1hz(p1_start_ms..=(p1_start_ms + 25_000));
        assert_eq!(env.len(), 11);
        assert!(
            env.iter()
                .all(|&(_, intensity)| intensity > 0.0 && intensity <= 1.0)
        );

        // Clock provenance inside partition is RtcEstimated; at live frame is LiveProvisional
        let prov_part = router.clock_provenance_at(p1_start_ms + 5_000).unwrap();
        assert_eq!(prov_part.badge_label(), "RTC Estimated");
        let prov_live = router.clock_provenance_at(p1_start_ms + 20_000).unwrap();
        assert_eq!(prov_live.badge_label(), "Live UDP");

        // Annotation CRUD and track day selection
        let day_id = router.partitions()[0].track_day_id.clone();
        let manual_ann = Annotation {
            id: "manual_bookmark_1".to_owned(),
            track_day_id: day_id.clone(),
            name: "Skidpad Run 1".to_owned(),
            category: "event".to_owned(),
            color: "#3b82f6".to_owned(),
            start_utc_ms: p1_start_ms + 1_000,
            end_utc_ms: Some(p1_start_ms + 8_000),
            author: "pitwall".to_owned(),
            source: "manual".to_owned(),
            notes: "Wet track".to_owned(),
            updated_at_ms: p1_start_ms + 25_000,
            deleted: false,
        };
        assert!(router.upsert_annotation(&manual_ann).unwrap());
        let anns = router
            .query_annotations_in_range(p1_start_ms..=(p1_start_ms + 10_000))
            .unwrap();
        assert!(anns.iter().any(|a| a.id == "manual_bookmark_1"));

        router.timeline_mut().select_single_annotation(
            "manual_bookmark_1".to_owned(),
            manual_ann.start_utc_ms,
            manual_ann.end_utc_ms,
        );
        assert!(
            router
                .timeline()
                .is_annotation_selected("manual_bookmark_1")
        );

        assert!(
            router
                .soft_delete_annotation("manual_bookmark_1", p1_start_ms + 26_000)
                .unwrap()
        );
        assert!(
            !router
                .timeline()
                .is_annotation_selected("manual_bookmark_1")
        );

        let bounds = router.select_track_day(Some(day_id)).unwrap();
        assert_eq!(bounds, Some((p1_start_ms, p1_start_ms + 9_000)));

        let _ = fs::remove_dir_all(&tmp_dir);
    }
}
