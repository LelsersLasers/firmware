//! Borrowed per-render projections backed by [`daqcore::router::VirtualTimelineRouter`].

use std::collections::BTreeMap;

pub struct TelemetryView<'a> {
    pub router: &'a daqcore::router::VirtualTimelineRouter,
    // Retained inspector frames in the current timeline range at or before the setpoint
    pub setpoint_frames: &'a [daqcore::frame::ParsedFrame],
    // Latest frame for each message before the setpoint
    pub latest_setpoint_frames:
        BTreeMap<daqcore::superdbc::message::MessageKey, &'a daqcore::frame::ParsedFrame>,
    pub timeline: &'a daqcore::timeline::MasterTimeline,
    view_time: daqcore::time::Time,
}

impl<'a> TelemetryView<'a> {
    pub fn new(router: &'a daqcore::router::VirtualTimelineRouter) -> Self {
        let live_buffer = router.live_buffer();
        let timeline = router.timeline();
        let setpoint_frames = live_buffer.frames_in_range(timeline.setpoint_range());

        let mut latest_setpoint_frames = BTreeMap::new();
        for frame in setpoint_frames.iter().rev() {
            latest_setpoint_frames.entry(frame.key()).or_insert(frame);
            if latest_setpoint_frames.len() == live_buffer.latest_map().len() {
                break;
            }
        }

        let view_time = if timeline.is_live() {
            daqcore::time::Time::now()
        } else {
            timeline.setpoint()
        };

        Self {
            router,
            setpoint_frames,
            timeline,
            latest_setpoint_frames,
            view_time,
        }
    }

    pub fn view_time(&self) -> daqcore::time::Time {
        self.view_time
    }

    /// Query a signal across the active `[start..=end]` viewport at `resolution`.
    pub fn query_signal(
        &self,
        signal_id: &str,
        resolution: daqcore::router::Resolution,
    ) -> daqcore::router::SignalSeries {
        self.router
            .query_signal(signal_id, self.timeline.range_ms(), resolution)
    }

    /// Query a signal across `[start..=setpoint]` at `resolution`.
    pub fn query_signal_to_setpoint(
        &self,
        signal_id: &str,
        resolution: daqcore::router::Resolution,
    ) -> daqcore::router::SignalSeries {
        self.router
            .query_signal(signal_id, self.timeline.setpoint_range_ms(), resolution)
    }

    /// Return the latest `(utc_ms, value)` for `signal_id` at or before `setpoint`.
    pub fn latest_signal_value_at_setpoint(&self, signal_id: &str) -> Option<(i64, f64)> {
        self.router
            .latest_signal_value_at(signal_id, self.timeline.setpoint_utc_ms())
    }
}

/// Rendering projection that preserves extrema, endpoints, `NaN` gap markers, and ordering in each pixel bucket.
pub fn decimate(points: &[[f64; 2]], pixels: usize) -> Vec<[f64; 2]> {
    let bucket = points.len().div_ceil(pixels.max(1)).max(1);
    if bucket <= 4 {
        return points.to_vec();
    }

    let mut out = Vec::new();

    for chunk in points.chunks(bucket) {
        let mut indexes = vec![0, chunk.len() - 1];
        let minimum = chunk
            .iter()
            .enumerate()
            .filter(|(_, p)| p[1].is_finite())
            .min_by(|(_, left), (_, right)| left[1].total_cmp(&right[1]));
        let maximum = chunk
            .iter()
            .enumerate()
            .filter(|(_, p)| p[1].is_finite())
            .max_by(|(_, left), (_, right)| left[1].total_cmp(&right[1]));

        if let Some((index, _)) = minimum {
            indexes.push(index);
        }

        if let Some((index, _)) = maximum {
            indexes.push(index);
        }

        if let Some((nan_idx, _)) = chunk.iter().enumerate().find(|(_, p)| !p[1].is_finite()) {
            indexes.push(nan_idx);
        }

        indexes.sort();
        indexes.dedup();
        out.extend(indexes.into_iter().map(|i| chunk[i]));
    }

    out
}

pub struct BusLoadSample {
    pub timestamp: daqcore::time::Time,
    pub buses: Vec<daqcore::can::bus_load::BusLoadReading>,
}
