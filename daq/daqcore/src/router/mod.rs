pub mod live_buffer;
pub mod tile_cache;
pub mod types;

pub use live_buffer::{
    DEFAULT_MAX_INSPECTOR_FRAMES, DEFAULT_MAX_SAMPLES_PER_SIGNAL, LiveColumnBuffer,
};
pub use tile_cache::{
    DEFAULT_TILE_CACHE_BYTES, RepaintCallback, RouterError, TileCache, TileKey, TileLookup,
    read_all_signals_overview, read_all_signals_states, read_single_signal_full_res,
    read_single_signal_overview,
};
pub use types::{
    BUCKET_1HZ_MS, BUCKET_10HZ_MS, CoverageSpan, DataSourceKind, EnvelopePoint,
    OVERVIEW_1HZ_THRESHOLD_MS, OVERVIEW_10HZ_THRESHOLD_MS, Resolution, SignalSeries, StateInterval,
    build_envelope_buckets,
};

use crate::{
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
        let timeline =
            MasterTimeline::live(now, window_secs).map_err(|e| io::Error::other(e))?;
        let registry = Registry::open_store(&store_dir)?;
        let mut router = Self {
            live_buffer: LiveColumnBuffer::default(),
            tile_cache: TileCache::default(),
            registry,
            store_dir: Some(store_dir),
            partitions: Vec::new(),
            timeline,
        };
        router.refresh_store()?;
        Ok(router)
    }

    /// Attach or switch to a new on-disk store directory and preload its local partitions.
    pub fn attach_store(&mut self, store_dir: impl AsRef<path::Path>) -> Result<usize, RouterError> {
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
            self.timeline.set_active_track_day_id(Some(last_day.id.clone()));
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
        if let Some( day_parts ) = self
            .registry
            .get_partition(&manifest.partition_id)?
        {
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

    /// Query `signal_id` across `range` (`[start_utc_ms..=end_utc_ms]`) at `resolution` without
    /// blocking the calling thread.
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
            if let Some(lookup) = self.tile_cache.query_signal(part_id, signal_id, resolution) {
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
                    .query_signal(signal_id, live_start_ms..=end_ms, resolution)
            && !live_series.is_empty()
        {
            chunks.push(live_series);
        }

        SignalSeries::stitch(chunks, resolution)
    }

    /// Synchronously query `signal_id` across `range` at exact `resolution` (blocking if any
    /// overlapping partition column is not yet cached in `TileCache`).
    pub fn query_signal_blocking(
        &self,
        signal_id: &str,
        range: ops::RangeInclusive<i64>,
        resolution: Resolution,
    ) -> Result<SignalSeries, RouterError> {
        if range.is_empty() {
            return Ok(SignalSeries::empty_for(resolution));
        }

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
            if let Some(series) = self
                .tile_cache
                .query_signal_blocking(part_id, signal_id, resolution)?
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
                    .query_signal(signal_id, live_start_ms..=end_ms, resolution)
            && !live_series.is_empty()
        {
            chunks.push(live_series);
        }

        Ok(SignalSeries::stitch(chunks, resolution))
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
        let start_ms = *range.start();
        let end_ms = *range.end();

        let mut out = Vec::new();
        for part in &self.partitions {
            if !part.is_local || part.start_utc_ms > end_ms || part.end_utc_ms < start_ms {
                continue;
            }
            if let Some(intervals) = self.tile_cache.query_states(&part.partition_id, signal_id) {
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
                    .query_states(signal_id, live_start_ms..=end_ms),
            );
        }

        out
    }

    /// Find the latest sample `(utc_ms, value)` for `signal_id` at or before `target_utc_ms`
    /// without blocking.
    pub fn latest_signal_value_at(
        &self,
        signal_id: &str,
        target_utc_ms: i64,
    ) -> Option<(i64, f64)> {
        let max_canonical = self.max_canonical_end_ms(target_utc_ms);

        // If target_utc_ms is beyond all canonical partitions (or no partitions exist),
        // check the provisional live buffer first.
        if max_canonical.is_none_or(|max_end| target_utc_ms > max_end)
            && let Some((live_t, live_v)) = self
                .live_buffer
                .latest_signal_value_at(signal_id, target_utc_ms)
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
                signal_id,
                target_utc_ms,
            ) {
                return Some(found);
            }
        }

        self.live_buffer
            .latest_signal_value_at(signal_id, target_utc_ms)
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

    fn make_live_frame(
        db: &Database,
        utc_ms: i64,
        id: u32,
        values: &[(&str, f64)],
    ) -> ParsedFrame {
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
        let tmp_dir = std::env::temp_dir().join(format!(
            "daq_router_m3_verify_{}",
            uuid::Uuid::new_v4()
        ));
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

        let summary =
            write_correlated_epochs_to_store(&tmp_dir, &log_files, epochs, &db).unwrap();
        assert_eq!(summary.partitions_written, 2);

        let t_11_05_ms = t_10_00_ms + 65 * min_ms;
        let t_11_06_ms = t_10_00_ms + 66 * min_ms;

        let mut router = VirtualTimelineRouter::open_store(
            &tmp_dir,
            Time::from_unix_millis(t_11_05_ms),
            30.0,
        )
        .expect("open store");
        assert_eq!(router.partitions().len(), 2);

        // Also push a provisional live frame inside Partition 2's window (10:50) with a bogus speed (99.0 m/s)
        // to verify Canonical SD > Live Buffer precedence, plus real live frames at 11:05 (40.0 m/s, state = 2 FAULT).
        let t_10_50_ms = t_10_00_ms + 50 * min_ms;
        router.ingest_frame(make_live_frame(&db, t_10_50_ms, 100, &[("speed_mps", 99.0)]));
        router.ingest_frame(make_live_frame(&db, t_11_05_ms, 100, &[("speed_mps", 40.0)]));
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
}
