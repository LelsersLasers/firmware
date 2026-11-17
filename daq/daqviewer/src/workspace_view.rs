//! Columnar workspace data view and cross-pane cursor synchronization backed by
//! [`daqcore::router::VirtualTimelineRouter`].

use std::cell::Cell;
use std::collections::BTreeMap;
use std::ops::RangeInclusive;

pub struct WorkspaceDataView<'a> {
    pub router: &'a daqcore::router::VirtualTimelineRouter,
    pub timeline: &'a daqcore::timeline::MasterTimeline,
    view_time: daqcore::time::Time,
    hover_utc_ms: Cell<Option<i64>>,
    hover_distance_m: Cell<Option<f64>>,
    requested_setpoint_utc_ms: Cell<Option<i64>>,
    requested_zoom: Cell<Option<(i64, f64)>>,
    requested_zoom_range_ms: Cell<Option<(i64, i64)>>,
    requested_pan_delta_ms: Cell<Option<i64>>,
    requested_open_export_modal: Cell<bool>,
    hover_touched: Cell<bool>,
}

impl<'a> WorkspaceDataView<'a> {
    pub fn new(router: &'a daqcore::router::VirtualTimelineRouter) -> Self {
        Self::with_hover(router, None, None)
    }

    pub fn with_hover(
        router: &'a daqcore::router::VirtualTimelineRouter,
        hover_utc_ms: Option<i64>,
        hover_distance_m: Option<f64>,
    ) -> Self {
        let timeline = router.timeline();
        let view_time = if timeline.is_live() {
            daqcore::time::Time::now()
        } else {
            timeline.setpoint()
        };

        Self {
            router,
            timeline,
            view_time,
            hover_utc_ms: Cell::new(hover_utc_ms),
            hover_distance_m: Cell::new(hover_distance_m),
            requested_setpoint_utc_ms: Cell::new(None),
            requested_zoom: Cell::new(None),
            requested_zoom_range_ms: Cell::new(None),
            requested_pan_delta_ms: Cell::new(None),
            requested_open_export_modal: Cell::new(false),
            hover_touched: Cell::new(false),
        }
    }

    pub fn view_time(&self) -> daqcore::time::Time {
        self.view_time
    }

    /// Monotonically increasing revision counter from the underlying router and tile cache.
    pub fn data_revision(&self) -> u64 {
        self.router.data_revision()
    }

    /// Synchronized hover timestamp (`utc_ms`) across all open analysis panes.
    pub fn hover_utc_ms(&self) -> Option<i64> {
        self.hover_utc_ms.get()
    }

    /// Update the synchronized hover timestamp from an interactive plot pane.
    pub fn set_hover_utc_ms(&self, utc_ms: Option<i64>) {
        self.hover_utc_ms.set(utc_ms);
        if utc_ms.is_some() {
            self.hover_touched.set(true);
        }
    }

    /// Synchronized hover track distance (`meters`) when hovering in distance-mode plots.
    pub fn hover_distance_m(&self) -> Option<f64> {
        self.hover_distance_m.get()
    }

    /// Update the synchronized hover track distance (`meters`).
    pub fn set_hover_distance_m(&self, distance_m: Option<f64>) {
        self.hover_distance_m.set(distance_m);
        if distance_m.is_some() {
            self.hover_touched.set(true);
        }
    }

    /// Returns `true` if any pane actively updated the hover cursor during this render pass.
    pub fn hover_touched(&self) -> bool {
        self.hover_touched.get()
    }

    /// Request moving the global playhead (`setpoint`) to `utc_ms` (e.g. when clicking a plot).
    pub fn request_setpoint_utc_ms(&self, utc_ms: i64) {
        self.requested_setpoint_utc_ms.set(Some(utc_ms));
    }

    /// Pending playhead (`setpoint`) update requested during this render pass.
    pub fn requested_setpoint_utc_ms(&self) -> Option<i64> {
        self.requested_setpoint_utc_ms.get()
    }

    /// Request zooming the global Master Timeline `[start, end]` around `anchor_utc_ms` by `zoom_factor`
    /// (`< 1.0` zooms in, `> 1.0` zooms out).
    pub fn request_zoom_around_ms(&self, anchor_utc_ms: i64, zoom_factor: f64) {
        self.requested_zoom.set(Some((anchor_utc_ms, zoom_factor)));
    }

    /// Pending timeline zoom `(anchor_utc_ms, zoom_factor)` requested during this render pass.
    pub fn requested_zoom(&self) -> Option<(i64, f64)> {
        self.requested_zoom.get()
    }

    /// Request zooming the global Master Timeline `[start, end]` to `[start_utc_ms, end_utc_ms]`.
    pub fn request_zoom_range_ms(&self, start_utc_ms: i64, end_utc_ms: i64) {
        self.requested_zoom_range_ms.set(Some((
            start_utc_ms.min(end_utc_ms),
            start_utc_ms.max(end_utc_ms),
        )));
    }

    /// Pending timeline range zoom `(start_utc_ms, end_utc_ms)` requested during this render pass.
    pub fn requested_zoom_range_ms(&self) -> Option<(i64, i64)> {
        self.requested_zoom_range_ms.get()
    }

    /// Request panning the global Master Timeline `[start, end]` by `delta_ms`.
    pub fn request_pan_delta_ms(&self, delta_ms: i64) {
        if delta_ms != 0 {
            self.requested_pan_delta_ms.set(Some(delta_ms));
        }
    }

    /// Pending timeline pan delta (`ms`) requested during this render pass.
    pub fn requested_pan_delta_ms(&self) -> Option<i64> {
        self.requested_pan_delta_ms.get()
    }

    /// Request opening the dataset export modal.
    pub fn request_open_export_modal(&self) {
        self.requested_open_export_modal.set(true);
    }

    pub fn requested_open_export_modal(&self) -> bool {
        self.requested_open_export_modal.get()
    }

    /// Returns the active inspection cursor (`hover_utc_ms` if hovering, else `setpoint_utc_ms`).
    pub fn active_cursor_utc_ms(&self) -> i64 {
        self.hover_utc_ms
            .get()
            .unwrap_or_else(|| self.timeline.setpoint_utc_ms())
    }

    /// Query a signal or derived formula across the active `[start..=end]` viewport at `resolution`.
    pub fn query_signal(
        &self,
        signal_id: &str,
        resolution: daqcore::router::Resolution,
    ) -> daqcore::router::SignalSeries {
        self.query_signal_range(signal_id, self.timeline.range_ms(), resolution)
    }

    /// Query a signal or derived formula across an explicit `range_ms` at `resolution`.
    /// Supports a leading `-` sign prefix (e.g. `-IMU_acceleration.Y_axis`) to negate values.
    pub fn query_signal_range(
        &self,
        signal_id: &str,
        range_ms: RangeInclusive<i64>,
        resolution: daqcore::router::Resolution,
    ) -> daqcore::router::SignalSeries {
        let trimmed = signal_id.trim();
        if let Some(inner) = trimmed.strip_prefix('-') {
            negate_series(self.router.query_signal(inner.trim(), range_ms, resolution))
        } else {
            self.router.query_signal(trimmed, range_ms, resolution)
        }
    }

    /// Query a signal or derived formula across `[start..=setpoint]` at `resolution`.
    pub fn query_signal_to_setpoint(
        &self,
        signal_id: &str,
        resolution: daqcore::router::Resolution,
    ) -> daqcore::router::SignalSeries {
        self.query_signal_range(signal_id, self.timeline.setpoint_range_ms(), resolution)
    }

    /// Query a signal or derived formula projected onto cumulative distance (`meters`).
    pub fn query_signal_vs_distance(
        &self,
        signal_id: &str,
        speed_signal_id: &str,
        range_ms: RangeInclusive<i64>,
        resolution: daqcore::router::Resolution,
    ) -> daqcore::analysis::DistanceSeries {
        let trimmed = signal_id.trim();
        if let Some(inner) = trimmed.strip_prefix('-') {
            let mut series = self.router.query_signal_vs_distance(
                inner.trim(),
                speed_signal_id,
                range_ms,
                resolution,
            );
            for v in &mut series.values {
                *v = -*v;
            }
            series
        } else {
            self.router
                .query_signal_vs_distance(trimmed, speed_signal_id, range_ms, resolution)
        }
    }

    /// Query discrete state intervals across `range_ms`.
    pub fn query_states(
        &self,
        signal_id: &str,
        range_ms: RangeInclusive<i64>,
    ) -> Vec<daqcore::router::StateInterval> {
        self.router.query_states(signal_id, range_ms)
    }

    /// Return the latest `(utc_ms, value)` for `signal_id` at or before `at_utc_ms`.
    pub fn latest_signal_value_at(&self, signal_id: &str, at_utc_ms: i64) -> Option<(i64, f64)> {
        let trimmed = signal_id.trim();
        if let Some(inner) = trimmed.strip_prefix('-') {
            self.router
                .latest_signal_value_at(inner.trim(), at_utc_ms)
                .map(|(t, v)| (t, -v))
        } else {
            self.router.latest_signal_value_at(trimmed, at_utc_ms)
        }
    }

    /// Return the latest `(utc_ms, value)` for `signal_id` within `[start_utc_ms..=setpoint_utc_ms]`.
    pub fn latest_signal_value_in_setpoint_range(&self, signal_id: &str) -> Option<(i64, f64)> {
        let trimmed = signal_id.trim();
        let range = self.timeline.setpoint_range_ms();
        if let Some(inner) = trimmed.strip_prefix('-') {
            self.router
                .latest_signal_value_in_range(inner.trim(), range)
                .map(|(t, v)| (t, -v))
        } else {
            self.router.latest_signal_value_in_range(trimmed, range)
        }
    }

    /// Return metadata for all raw signals that have samples in the active viewport.
    pub fn active_signals_in_viewport(&self) -> Vec<daqcore::router::ActiveSignalMeta> {
        self.router.active_signals_in_viewport()
    }

    /// Return metadata for all raw signals that have samples in `[start_utc_ms..=setpoint_utc_ms]`.
    pub fn active_signals_to_setpoint(&self) -> Vec<daqcore::router::ActiveSignalMeta> {
        self.router.active_signals_to_setpoint()
    }

    /// Return the latest `(utc_ms, value)` for `signal_id` at or before the active cursor
    /// (`hover_utc_ms` if hovering, else `setpoint_utc_ms`).
    pub fn latest_signal_value_at_cursor(&self, signal_id: &str) -> Option<(i64, f64)> {
        self.latest_signal_value_at(signal_id, self.active_cursor_utc_ms())
    }

    /// On-demand access to retained raw CAN inspector frames in `[start..=setpoint]`.
    /// Used only by operational CAN message inspector widgets (`ViewerTable`, `ViewerList`, `Jitter`).
    pub fn live_frames_to_setpoint(&self) -> &'a [daqcore::frame::ParsedFrame] {
        self.router
            .live_buffer()
            .frames_in_range(self.timeline.setpoint_range())
    }

    /// On-demand map of the latest raw CAN frame per `MessageKey` at or before `setpoint`.
    pub fn latest_live_frames_to_setpoint(
        &self,
    ) -> BTreeMap<daqcore::superdbc::message::MessageKey, &'a daqcore::frame::ParsedFrame> {
        let live_buffer = self.router.live_buffer();
        let frames = live_buffer.frames_in_range(self.timeline.setpoint_range());
        let mut latest = BTreeMap::new();
        for frame in frames.iter().rev() {
            latest.entry(frame.key()).or_insert(frame);
            if latest.len() == live_buffer.latest_map().len() {
                break;
            }
        }
        latest
    }

    /// Resolve engineering unit string for a canonical or derived `signal_id`.
    pub fn resolve_signal_unit(&self, signal_id: &str) -> String {
        let raw_id = signal_id
            .trim()
            .strip_prefix('-')
            .unwrap_or(signal_id)
            .trim();
        if let Some(compiled) = self.router.formula_engine().get(raw_id)
            && !compiled.def.unit.is_empty()
        {
            return compiled.def.unit.clone();
        }
        if let Some(meta) = self.router.signal_meta(raw_id)
            && !meta.unit.is_empty()
        {
            return meta.unit;
        }
        String::new()
    }

    /// Choose the best default speed signal (`m/s` or `km/h`) available for distance integration.
    pub fn resolve_default_speed_signal(&self) -> String {
        let candidates = [
            "gps_speed_heading.gps_speed",
            "wheel_speeds.front_left",
            "wheel_speeds.front_right",
        ];
        let active = self.active_signals_in_viewport();
        for cand in candidates {
            if active.iter().any(|s| s.signal_id == cand) {
                return cand.to_owned();
            }
        }
        for cand in candidates {
            if self.router.signal_meta(cand).is_some() {
                return cand.to_owned();
            }
        }
        "gps_speed_heading.gps_speed".to_owned()
    }
}

fn negate_series(series: daqcore::router::SignalSeries) -> daqcore::router::SignalSeries {
    match series {
        daqcore::router::SignalSeries::Exact {
            timestamps_ms,
            mut values,
        } => {
            for v in &mut values {
                *v = -*v;
            }
            daqcore::router::SignalSeries::Exact {
                timestamps_ms,
                values,
            }
        }
        daqcore::router::SignalSeries::Envelope(mut buckets) => {
            for b in &mut buckets {
                let new_min = -b.max;
                let new_max = -b.min;
                b.min = new_min;
                b.max = new_max;
                b.mean = -b.mean;
            }
            daqcore::router::SignalSeries::Envelope(buckets)
        }
    }
}

/// Convert `(utc_ms, value)` samples into relative `[seconds_from_start, value]` plot coordinates,
/// inserting `[gap_mid_s, f64::NAN]` whenever two consecutive finite samples are separated by more
/// than `max_gap_ms` so `egui_plot` breaks the line across unlogged intervals.
pub fn points_with_gap_breaks<I>(samples: I, start_ms: i64, max_gap_ms: i64) -> Vec<[f64; 2]>
where
    I: IntoIterator<Item = (i64, f64)>,
{
    let mut out = Vec::new();
    let mut prev_finite_t_ms: Option<i64> = None;

    for (t_ms, val) in samples {
        let t_s = ((t_ms - start_ms) as f64) / 1000.0;
        if val.is_finite() {
            if let Some(prev_ms) = prev_finite_t_ms
                && t_ms - prev_ms > max_gap_ms
            {
                let gap_t_s = (((prev_ms + t_ms) / 2 - start_ms) as f64) / 1000.0;
                out.push([gap_t_s, f64::NAN]);
            }
            prev_finite_t_ms = Some(t_ms);
            out.push([t_s, val]);
        } else {
            if out.last().is_none_or(|last| last[1].is_finite()) {
                out.push([t_s, f64::NAN]);
            }
            prev_finite_t_ms = None;
        }
    }

    out
}

fn decimate_finite_segment(segment: &[[f64; 2]], bucket: usize, out: &mut Vec<[f64; 2]>) {
    if segment.is_empty() {
        return;
    }
    if bucket <= 4 || segment.len() <= 4 {
        out.extend_from_slice(segment);
        return;
    }

    for chunk in segment.chunks(bucket) {
        let mut indexes = vec![0, chunk.len() - 1];
        let minimum = chunk
            .iter()
            .enumerate()
            .min_by(|(_, left), (_, right)| left[1].total_cmp(&right[1]));
        let maximum = chunk
            .iter()
            .enumerate()
            .max_by(|(_, left), (_, right)| left[1].total_cmp(&right[1]));

        if let Some((index, _)) = minimum {
            indexes.push(index);
        }

        if let Some((index, _)) = maximum {
            indexes.push(index);
        }

        indexes.sort();
        indexes.dedup();
        out.extend(indexes.into_iter().map(|i| chunk[i]));
    }
}

/// Rendering projection that preserves extrema, endpoints, `NaN` gap markers, and ordering in each pixel bucket.
pub fn decimate(points: &[[f64; 2]], pixels: usize) -> Vec<[f64; 2]> {
    let bucket = points.len().div_ceil(pixels.max(1)).max(1);
    if bucket <= 4 {
        return points.to_vec();
    }

    let mut out = Vec::new();
    let mut seg_start = 0;

    for (idx, pt) in points.iter().enumerate() {
        if !pt[1].is_finite() {
            decimate_finite_segment(&points[seg_start..idx], bucket, &mut out);
            out.push(*pt);
            seg_start = idx + 1;
        }
    }
    decimate_finite_segment(&points[seg_start..], bucket, &mut out);

    out
}

pub struct BusLoadSample {
    pub timestamp: daqcore::time::Time,
    pub buses: Vec<daqcore::can::bus_load::BusLoadReading>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_data_view_tracks_linked_hover_and_setpoint_requests() {
        let t0 = daqcore::time::Time::from_unix_millis(1_794_500_000_000);
        let router = daqcore::router::VirtualTimelineRouter::live(t0, 30.0).expect("live router");
        let view = WorkspaceDataView::new(&router);

        assert_eq!(view.hover_utc_ms(), None);
        assert_eq!(view.hover_distance_m(), None);
        assert!(!view.hover_touched());
        assert_eq!(view.active_cursor_utc_ms(), t0.unix_millis());

        view.set_hover_utc_ms(Some(t0.unix_millis() - 5_000));
        view.set_hover_distance_m(Some(142.5));
        assert!(view.hover_touched());
        assert_eq!(view.hover_utc_ms(), Some(t0.unix_millis() - 5_000));
        assert_eq!(view.hover_distance_m(), Some(142.5));
        assert_eq!(view.active_cursor_utc_ms(), t0.unix_millis() - 5_000);

        view.request_setpoint_utc_ms(t0.unix_millis() - 2_500);
        assert_eq!(
            view.requested_setpoint_utc_ms(),
            Some(t0.unix_millis() - 2_500)
        );

        assert_eq!(view.resolve_signal_unit("battery.power_kw"), "kW");
        assert_eq!(
            view.resolve_default_speed_signal(),
            "gps_speed_heading.gps_speed"
        );
        assert!(
            view.latest_signal_value_at_cursor("battery.power_kw")
                .is_none()
        );
        let dist = view.query_signal_vs_distance(
            "-IMU_acceleration.Y_axis",
            "gps_speed_heading.gps_speed",
            (t0.unix_millis() - 10_000)..=t0.unix_millis(),
            daqcore::router::Resolution::FullRes,
        );
        assert!(dist.is_empty());
        assert!(
            view.query_states(
                "main_hb.car_state",
                (t0.unix_millis() - 10_000)..=t0.unix_millis()
            )
            .is_empty()
        );
    }
}
