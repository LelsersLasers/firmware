//! Unified Master Timeline state, owned by the UI/main thread.
//!
//! Tracks both the active viewport (`[start, end, setpoint]`) and global Track Day
//! bounds (`[day_min_utc_ms, day_max_utc_ms]`), idle-gap folding state, and active
//! track day selection.

use crate::{router::Resolution, time};
use std::ops;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Track {
    Marching,
    Frozen,
}

#[derive(Debug, Clone)]
pub struct MasterTimeline {
    start: time::Time,
    end: time::Time,
    setpoint: time::Time,
    start_track: Track,
    end_track: Track,
    setpoint_track: Track,
    window_ms: i64,
    latest: time::Time,
    day_bounds_ms: Option<(i64, i64)>,
    active_track_day_id: Option<String>,
    collapse_idle_gaps: bool,
}

/// Type alias preserving compatibility with callers referencing `daqcore::timeline::Timeline`.
pub type Timeline = MasterTimeline;

fn millis(seconds: f64) -> Result<i64, String> {
    if !seconds.is_finite() || seconds < 0.0 {
        return Err("timeline span must be finite and nonnegative".into());
    }

    let milliseconds = (seconds * 1000.0).round();

    // Saturate at the timestamp representation's limit, without a retention cap.
    Ok(milliseconds.min(i64::MAX as f64) as i64)
}

impl MasterTimeline {
    pub fn live(now: time::Time, window_secs: f64) -> Result<Self, String> {
        let window_ms = millis(window_secs)?;

        Ok(Self {
            start: now.offset(-window_ms),
            end: now,
            setpoint: now,
            start_track: Track::Marching,
            end_track: Track::Marching,
            setpoint_track: Track::Marching,
            window_ms,
            latest: now,
            day_bounds_ms: None,
            active_track_day_id: None,
            collapse_idle_gaps: false,
        })
    }

    pub fn reset(&mut self, now: time::Time) {
        self.start = now.offset(-self.window_ms);
        self.end = now;
        self.setpoint = now;
        self.latest = now;
        self.go_live();
    }

    pub fn start(&self) -> time::Time {
        self.start
    }

    pub fn end(&self) -> time::Time {
        self.end
    }

    pub fn setpoint(&self) -> time::Time {
        self.setpoint
    }

    pub fn start_utc_ms(&self) -> i64 {
        self.start.unix_millis()
    }

    pub fn end_utc_ms(&self) -> i64 {
        self.end.unix_millis()
    }

    pub fn setpoint_utc_ms(&self) -> i64 {
        self.setpoint.unix_millis()
    }

    pub fn start_track(&self) -> Track {
        self.start_track
    }

    pub fn end_track(&self) -> Track {
        self.end_track
    }

    pub fn setpoint_track(&self) -> Track {
        self.setpoint_track
    }

    pub fn window_ms(&self) -> i64 {
        self.window_ms
    }

    pub fn window_secs(&self) -> f64 {
        self.window_ms as f64 / 1000.0
    }

    pub fn is_live(&self) -> bool {
        self.setpoint_track == Track::Marching && self.end_track == Track::Marching
    }

    pub fn range(&self) -> ops::RangeInclusive<time::Time> {
        self.start..=self.end
    }

    pub fn range_ms(&self) -> ops::RangeInclusive<i64> {
        self.start.unix_millis()..=self.end.unix_millis()
    }

    pub fn setpoint_range(&self) -> ops::RangeInclusive<time::Time> {
        self.start..=self.setpoint
    }

    pub fn setpoint_range_ms(&self) -> ops::RangeInclusive<i64> {
        self.start.unix_millis()..=self.setpoint.unix_millis()
    }

    pub fn resolution_for_width(&self, plot_width_px: f32) -> Resolution {
        let span_ms = self
            .end
            .unix_millis()
            .saturating_sub(self.start.unix_millis());
        Resolution::for_window_ms(span_ms, plot_width_px)
    }

    pub fn day_bounds_ms(&self) -> Option<(i64, i64)> {
        self.day_bounds_ms
    }

    pub fn day_bounds(&self) -> Option<(time::Time, time::Time)> {
        let (min_ms, max_ms) = self.day_bounds_ms?;
        Some((
            time::Time::from_unix_millis(min_ms),
            time::Time::from_unix_millis(max_ms),
        ))
    }

    pub fn set_day_bounds_ms(&mut self, bounds: Option<(i64, i64)>) {
        self.day_bounds_ms = bounds.map(|(lo, hi)| (lo, hi.max(lo)));
    }

    pub fn observe_day_span_ms(&mut self, start_utc_ms: i64, end_utc_ms: i64) {
        let hi = end_utc_ms.max(start_utc_ms);
        self.day_bounds_ms = Some(match self.day_bounds_ms {
            Some((prev_lo, prev_hi)) => (prev_lo.min(start_utc_ms), prev_hi.max(hi)),
            None => (start_utc_ms, hi),
        });
    }

    pub fn active_track_day_id(&self) -> Option<&str> {
        self.active_track_day_id.as_deref()
    }

    pub fn set_active_track_day_id(&mut self, track_day_id: Option<String>) {
        self.active_track_day_id = track_day_id;
    }

    pub fn collapse_idle_gaps(&self) -> bool {
        self.collapse_idle_gaps
    }

    pub fn set_collapse_idle_gaps(&mut self, collapse: bool) {
        self.collapse_idle_gaps = collapse;
    }

    /// Snap the active viewport `[start, end]` and `setpoint` to a specific historical interval
    /// (e.g. an imported partition or an Event Annotation pill) and freeze live marching.
    pub fn snap_to_range_ms(&mut self, start_utc_ms: i64, end_utc_ms: i64) {
        let lo = start_utc_ms;
        let hi = end_utc_ms.max(lo);
        self.start = time::Time::from_unix_millis(lo);
        self.end = time::Time::from_unix_millis(hi);
        self.setpoint = self.end;
        self.start_track = Track::Frozen;
        self.end_track = Track::Frozen;
        self.setpoint_track = Track::Frozen;
        self.window_ms = hi.saturating_sub(lo);
        self.observe_day_span_ms(lo, hi);
    }

    pub fn observe(&mut self, latest: time::Time) -> bool {
        self.latest = self.latest.max(latest);
        self.observe_day_span_ms(latest.unix_millis(), latest.unix_millis());
        self.recompute()
    }

    fn recompute(&mut self) -> bool {
        let before = (self.start, self.end, self.setpoint);

        if self.end_track == Track::Marching {
            self.end = self.end.max(self.latest);
            if self.start_track == Track::Frozen {
                self.end = self.end.max(self.start);
            }

            if self.setpoint_track == Track::Frozen {
                self.end = self.end.max(self.setpoint);
            }
        }

        if self.start_track == Track::Marching {
            self.start = self.end.offset(-self.window_ms);
            if self.setpoint_track == Track::Frozen {
                self.start = self.start.min(self.setpoint);
            }
        }

        if self.setpoint_track == Track::Marching {
            self.setpoint = self.end;
        }

        before != (self.start, self.end, self.setpoint)
    }

    pub fn set_start(&mut self, time: time::Time) {
        let upper_bound = if self.setpoint_track == Track::Frozen {
            self.setpoint
        } else {
            self.end
        };

        self.start = time.min(upper_bound);
        self.start_track = Track::Frozen;
        self.recompute();
    }

    pub fn set_end(&mut self, time: time::Time) {
        let mut end = time;

        if self.start_track == Track::Frozen {
            end = end.max(self.start);
        }

        if self.setpoint_track == Track::Frozen {
            end = end.max(self.setpoint);
        }

        self.end = end;
        self.end_track = Track::Frozen;
        self.recompute();
    }

    pub fn set_setpoint(&mut self, time: time::Time) {
        self.setpoint = time.clamp(self.start, self.end);
        self.setpoint_track = Track::Frozen;
        self.recompute();
    }

    pub fn release_start(&mut self) {
        self.start_track = Track::Marching;
        self.recompute();
    }

    pub fn release_end(&mut self) {
        self.end_track = Track::Marching;
        self.recompute();
    }

    pub fn release_setpoint(&mut self) {
        self.setpoint_track = Track::Marching;
        self.recompute();
    }

    pub fn go_live(&mut self) {
        self.start_track = Track::Marching;
        self.end_track = Track::Marching;
        self.setpoint_track = Track::Marching;
        self.recompute();
    }

    pub fn set_window_secs(&mut self, seconds: f64) -> Result<(), String> {
        self.window_ms = millis(seconds)?;
        self.recompute();

        Ok(())
    }
}
