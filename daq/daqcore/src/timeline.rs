//! Unified Master Timeline state, owned by the UI/main thread.
//!
//! Tracks the active viewport (`[start, end, setpoint]`), global Track Day
//! bounds (`[day_min_utc_ms, day_max_utc_ms]`), idle-gap folding projection,
//! active track day selection, playback state, and event comparison selections.

use crate::{router::Resolution, time};
use std::ops;

/// Minimum window span (100 ms) when zooming in on the timeline.
pub const MIN_WINDOW_MS: i64 = 100;

/// Minimum unlogged gap duration (60 seconds) that can be folded into an `// idle //` divider.
pub const MIN_FOLDABLE_GAP_MS: i64 = 60_000;

/// Standard playback speed multipliers exposed in the Master Timeline transport bar.
pub const PLAYBACK_SPEED_PRESETS: [f64; 4] = [0.1, 0.5, 1.0, 2.0];

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Track {
    Marching,
    Frozen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SegmentKind {
    Active,
    IdleGap { folded: bool },
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProjectionSegment {
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    pub frac_start: f32,
    pub frac_end: f32,
    pub kind: SegmentKind,
}

impl ProjectionSegment {
    pub fn duration_ms(&self) -> i64 {
        self.end_utc_ms.saturating_sub(self.start_utc_ms)
    }

    pub fn frac_width(&self) -> f32 {
        (self.frac_end - self.frac_start).max(0.0)
    }

    pub fn is_idle_gap(&self) -> bool {
        matches!(self.kind, SegmentKind::IdleGap { .. })
    }

    pub fn is_folded_gap(&self) -> bool {
        matches!(self.kind, SegmentKind::IdleGap { folded: true })
    }
}

/// Piecewise-linear coordinate projection between `[day_start_ms, day_end_ms]` and
/// normalized horizontal coordinates `[0.0, 1.0]`, supporting Dead-Time Gap Folding.
#[derive(Debug, Clone, PartialEq)]
pub struct GapFoldedProjection {
    day_start_ms: i64,
    day_end_ms: i64,
    folded: bool,
    segments: Vec<ProjectionSegment>,
}

impl GapFoldedProjection {
    /// Construct a projection over `[day_start_ms, day_end_ms]` given active coverage intervals.
    pub fn new(
        day_start_ms: i64,
        day_end_ms: i64,
        active_spans: &[(i64, i64)],
        collapse_idle_gaps: bool,
    ) -> Self {
        let day_end_ms = day_end_ms.max(day_start_ms.saturating_add(1));
        let total_day_ms = (day_end_ms - day_start_ms).max(1) as f64;

        // Clamp active spans to [day_start_ms, day_end_ms], sort, and merge spans separated by < MIN_FOLDABLE_GAP_MS
        let mut clamped: Vec<(i64, i64)> = active_spans
            .iter()
            .filter_map(|&(s, e)| {
                if e < day_start_ms || s > day_end_ms {
                    return None;
                }
                let cs = s.clamp(day_start_ms, day_end_ms);
                let ce = e.clamp(day_start_ms, day_end_ms).max(cs);
                Some((cs, ce))
            })
            .collect();
        clamped.sort_by_key(|&(s, e)| (s, e));

        let mut merged_active: Vec<(i64, i64)> = Vec::new();
        for (s, e) in clamped {
            if let Some(last) = merged_active.last_mut() {
                if s.saturating_sub(last.1) < MIN_FOLDABLE_GAP_MS {
                    last.1 = last.1.max(e);
                } else {
                    merged_active.push((s, e));
                }
            } else {
                merged_active.push((s, e));
            }
        }

        if merged_active.is_empty() {
            return Self {
                day_start_ms,
                day_end_ms,
                folded: false,
                segments: vec![ProjectionSegment {
                    start_utc_ms: day_start_ms,
                    end_utc_ms: day_end_ms,
                    frac_start: 0.0,
                    frac_end: 1.0,
                    kind: SegmentKind::Active,
                }],
            };
        }

        // Absorb leading/trailing gaps smaller than MIN_FOLDABLE_GAP_MS into adjacent active spans
        if merged_active[0].0.saturating_sub(day_start_ms) < MIN_FOLDABLE_GAP_MS {
            merged_active[0].0 = day_start_ms;
        }
        if let Some(last) = merged_active.last_mut()
            && day_end_ms.saturating_sub(last.1) < MIN_FOLDABLE_GAP_MS
        {
            last.1 = day_end_ms;
        }

        // Build alternating raw intervals
        let mut raw_intervals: Vec<(i64, i64, bool)> = Vec::new(); // (start, end, is_gap)
        let mut cursor = day_start_ms;
        for (act_start, act_end) in merged_active {
            if act_start > cursor {
                raw_intervals.push((cursor, act_start, true));
            }
            let effective_end = act_end.max(act_start);
            raw_intervals.push((act_start, effective_end, false));
            cursor = effective_end;
        }
        if cursor < day_end_ms {
            raw_intervals.push((cursor, day_end_ms, true));
        }

        let gap_count = raw_intervals
            .iter()
            .filter(|&&(_, _, is_gap)| is_gap)
            .count();
        let active_count = raw_intervals
            .iter()
            .filter(|&&(_, _, is_gap)| !is_gap)
            .count();
        let total_active_ms: i64 = raw_intervals
            .iter()
            .filter(|&&(_, _, is_gap)| !is_gap)
            .map(|&(s, e, _)| (e - s).max(1))
            .sum();

        let do_fold = collapse_idle_gaps && gap_count > 0 && active_count > 0;
        let mut segments = Vec::with_capacity(raw_intervals.len());

        if !do_fold {
            for (s, e, is_gap) in raw_intervals {
                let frac_start = ((s - day_start_ms) as f64 / total_day_ms).clamp(0.0, 1.0) as f32;
                let frac_end = ((e - day_start_ms) as f64 / total_day_ms).clamp(0.0, 1.0) as f32;
                segments.push(ProjectionSegment {
                    start_utc_ms: s,
                    end_utc_ms: e,
                    frac_start,
                    frac_end: frac_end.max(frac_start),
                    kind: if is_gap {
                        SegmentKind::IdleGap { folded: false }
                    } else {
                        SegmentKind::Active
                    },
                });
            }
        } else {
            let gap_frac_each = (0.04_f64).min(0.18 / (gap_count as f64));
            let total_gap_frac = gap_frac_each * (gap_count as f64);
            let remaining_active_frac = (1.0 - total_gap_frac).max(0.1);
            let denom_active = (total_active_ms.max(1)) as f64;

            let mut acc_frac = 0.0_f64;
            let total_len = raw_intervals.len();
            for (idx, (s, e, is_gap)) in raw_intervals.into_iter().enumerate() {
                let width = if is_gap {
                    gap_frac_each
                } else {
                    let dur = (e - s).max(1) as f64;
                    remaining_active_frac * (dur / denom_active)
                };
                let frac_start = acc_frac as f32;
                acc_frac = (acc_frac + width).min(1.0);
                let frac_end = if idx + 1 == total_len {
                    1.0
                } else {
                    (acc_frac as f32).max(frac_start)
                };
                segments.push(ProjectionSegment {
                    start_utc_ms: s,
                    end_utc_ms: e,
                    frac_start,
                    frac_end,
                    kind: if is_gap {
                        SegmentKind::IdleGap { folded: true }
                    } else {
                        SegmentKind::Active
                    },
                });
            }
        }

        Self {
            day_start_ms,
            day_end_ms,
            folded: do_fold,
            segments,
        }
    }

    pub fn day_start_ms(&self) -> i64 {
        self.day_start_ms
    }

    pub fn day_end_ms(&self) -> i64 {
        self.day_end_ms
    }

    pub fn is_folded(&self) -> bool {
        self.folded
    }

    pub fn segments(&self) -> &[ProjectionSegment] {
        &self.segments
    }

    pub fn active_screen_fraction(&self) -> f32 {
        self.segments
            .iter()
            .filter(|s| matches!(s.kind, SegmentKind::Active))
            .map(ProjectionSegment::frac_width)
            .sum()
    }

    /// Map a UTC timestamp (`utc_ms`) into a normalized `[0.0, 1.0]` coordinate.
    pub fn time_to_fraction(&self, utc_ms: i64) -> f32 {
        if utc_ms <= self.day_start_ms {
            return 0.0;
        }
        if utc_ms >= self.day_end_ms {
            return 1.0;
        }
        for seg in &self.segments {
            if utc_ms >= seg.start_utc_ms && utc_ms <= seg.end_utc_ms {
                let dur = (seg.end_utc_ms - seg.start_utc_ms).max(1) as f64;
                let t = (utc_ms - seg.start_utc_ms) as f64 / dur;
                let frac = f64::from(seg.frac_start) + t * f64::from(seg.frac_end - seg.frac_start);
                return (frac as f32).clamp(0.0, 1.0);
            }
        }
        1.0
    }

    /// Map a normalized `[0.0, 1.0]` coordinate back to a UTC timestamp (`utc_ms`).
    pub fn fraction_to_time(&self, fraction: f32) -> i64 {
        let f = fraction.clamp(0.0, 1.0);
        if f <= 0.0 {
            return self.day_start_ms;
        }
        if f >= 1.0 {
            return self.day_end_ms;
        }
        for seg in &self.segments {
            if f >= seg.frac_start && f <= seg.frac_end {
                let w = f64::from((seg.frac_end - seg.frac_start).max(1e-6));
                let local = f64::from(f - seg.frac_start) / w;
                let dur = (seg.end_utc_ms - seg.start_utc_ms) as f64;
                return seg.start_utc_ms + (local * dur).round() as i64;
            }
        }
        self.day_end_ms
    }
}

/// Format an idle gap duration in milliseconds into a compact human label (e.g. `"1h 45m idle"`).
pub fn format_idle_duration(duration_ms: i64) -> String {
    let total_secs = (duration_ms.max(0) / 1000) as u64;
    let hours = total_secs / 3600;
    let mins = (total_secs % 3600) / 60;
    let secs = total_secs % 60;

    if hours > 0 && mins > 0 {
        format!("{hours}h {mins}m idle")
    } else if hours > 0 {
        format!("{hours}h idle")
    } else if mins > 0 {
        format!("{mins}m idle")
    } else {
        format!("{secs}s idle")
    }
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
    is_playing: bool,
    playback_speed: f64,
    selected_annotation_ids: Vec<String>,
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
            is_playing: false,
            playback_speed: 1.0,
            selected_annotation_ids: Vec::new(),
        })
    }

    pub fn reset(&mut self, now: time::Time) {
        self.start = now.offset(-self.window_ms);
        self.end = now;
        self.setpoint = now;
        self.latest = now;
        self.is_playing = false;
        self.selected_annotation_ids.clear();
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

    pub fn is_playing(&self) -> bool {
        self.is_playing
    }

    pub fn set_playing(&mut self, playing: bool) {
        if playing {
            if self.is_live() {
                self.start_track = Track::Frozen;
                self.end_track = Track::Frozen;
                self.setpoint_track = Track::Frozen;
            }
            if self.setpoint >= self.end && self.end > self.start {
                self.setpoint = self.start;
            }
            self.is_playing = true;
        } else {
            self.is_playing = false;
        }
    }

    pub fn toggle_playing(&mut self) {
        self.set_playing(!self.is_playing);
    }

    pub fn playback_speed(&self) -> f64 {
        self.playback_speed
    }

    pub fn set_playback_speed(&mut self, speed: f64) {
        if speed.is_finite() && speed > 0.0 {
            self.playback_speed = speed.clamp(0.01, 100.0);
        }
    }

    /// Advance the playhead during active playback by `real_dt_secs * playback_speed`.
    /// Returns `true` if the timeline state changed.
    pub fn advance_playback(&mut self, real_dt_secs: f64) -> bool {
        if !self.is_playing || !real_dt_secs.is_finite() || real_dt_secs <= 0.0 {
            return false;
        }
        if self.is_live() {
            self.is_playing = false;
            return false;
        }

        let delta_ms = ((real_dt_secs * self.playback_speed * 1000.0).round() as i64).max(1);
        let cur_setpoint_ms = self.setpoint.unix_millis();
        let cur_end_ms = self.end.unix_millis();
        let next_ms = cur_setpoint_ms.saturating_add(delta_ms);

        if next_ms <= cur_end_ms {
            self.setpoint = time::Time::from_unix_millis(next_ms);
            self.setpoint_track = Track::Frozen;
            return true;
        }

        // Playhead reached the end of the active viewport.
        // If no single annotation is pinned and day_bounds extends further, pan the window forward.
        if self.selected_annotation_ids.is_empty()
            && let Some((_, day_max_ms)) = self.day_bounds_ms
            && cur_end_ms < day_max_ms
        {
            let shift_ms = (next_ms - cur_end_ms).min(day_max_ms - cur_end_ms);
            let span_ms = cur_end_ms.saturating_sub(self.start.unix_millis());
            let new_end_ms = cur_end_ms.saturating_add(shift_ms);
            let new_start_ms = new_end_ms.saturating_sub(span_ms);
            self.start = time::Time::from_unix_millis(new_start_ms);
            self.end = time::Time::from_unix_millis(new_end_ms);
            self.setpoint = self.end;
            if new_end_ms >= day_max_ms {
                self.is_playing = false;
            }
            return true;
        }

        self.setpoint = self.end;
        self.is_playing = false;
        cur_setpoint_ms != cur_end_ms
    }

    /// Step the playhead (`setpoint`) by `delta_ms` (positive or negative), shifting
    /// `[start, end]` if `setpoint` steps outside the active window.
    pub fn step_by_ms(&mut self, delta_ms: i64) {
        self.is_playing = false;
        self.start_track = Track::Frozen;
        self.end_track = Track::Frozen;
        self.setpoint_track = Track::Frozen;

        let span_ms = self
            .end
            .unix_millis()
            .saturating_sub(self.start.unix_millis())
            .max(MIN_WINDOW_MS);
        let mut target_ms = self.setpoint.unix_millis().saturating_add(delta_ms);
        if let Some((day_min, day_max)) = self.day_bounds_ms {
            target_ms = target_ms.clamp(day_min, day_max);
        }

        if target_ms < self.start.unix_millis() {
            self.start = time::Time::from_unix_millis(target_ms);
            self.end = time::Time::from_unix_millis(target_ms.saturating_add(span_ms));
        } else if target_ms > self.end.unix_millis() {
            self.end = time::Time::from_unix_millis(target_ms);
            self.start = time::Time::from_unix_millis(target_ms.saturating_sub(span_ms));
        }
        self.setpoint = time::Time::from_unix_millis(target_ms);
    }

    /// Pan the active `[start, end]` window to start at `new_start_ms`, preserving its duration
    /// and the relative offset of `setpoint` inside the window.
    pub fn pan_to_start_ms(&mut self, mut new_start_ms: i64) {
        let span_ms = self
            .end
            .unix_millis()
            .saturating_sub(self.start.unix_millis())
            .max(MIN_WINDOW_MS);
        let setpoint_offset_ms = self
            .setpoint
            .unix_millis()
            .saturating_sub(self.start.unix_millis())
            .clamp(0, span_ms);

        if let Some((day_min, day_max)) = self.day_bounds_ms {
            let max_start = day_max.saturating_sub(span_ms).max(day_min);
            new_start_ms = new_start_ms.clamp(day_min, max_start);
        }
        let new_end_ms = new_start_ms.saturating_add(span_ms);
        self.start = time::Time::from_unix_millis(new_start_ms);
        self.end = time::Time::from_unix_millis(new_end_ms);
        self.setpoint =
            time::Time::from_unix_millis(new_start_ms.saturating_add(setpoint_offset_ms));
        self.start_track = Track::Frozen;
        self.end_track = Track::Frozen;
        self.setpoint_track = Track::Frozen;
        self.window_ms = span_ms;
    }

    /// Smoothly zoom `[start, end]` around `anchor_utc_ms` by `zoom_factor`
    /// (`< 1.0` zooms in, `> 1.0` zooms out), clamping down to [`MIN_WINDOW_MS`] (`100 ms`).
    pub fn zoom_around_ms(&mut self, anchor_utc_ms: i64, zoom_factor: f64) {
        if !zoom_factor.is_finite() || zoom_factor <= 0.0 {
            return;
        }
        let cur_start = self.start.unix_millis();
        let cur_end = self.end.unix_millis();
        let cur_span = cur_end.saturating_sub(cur_start).max(MIN_WINDOW_MS);
        let max_span = self
            .day_bounds_ms
            .map(|(lo, hi)| (hi - lo).max(MIN_WINDOW_MS))
            .unwrap_or(86_400_000);

        let new_span =
            ((cur_span as f64 * zoom_factor).round() as i64).clamp(MIN_WINDOW_MS, max_span);
        let clamped_anchor = anchor_utc_ms.clamp(cur_start, cur_end);
        let anchor_ratio = (clamped_anchor - cur_start) as f64 / (cur_span as f64);

        let mut new_start = clamped_anchor - (anchor_ratio * (new_span as f64)).round() as i64;
        if let Some((day_min, day_max)) = self.day_bounds_ms {
            let max_start = day_max.saturating_sub(new_span).max(day_min);
            new_start = new_start.clamp(day_min, max_start);
        }
        let new_end = new_start.saturating_add(new_span);

        self.start = time::Time::from_unix_millis(new_start);
        self.end = time::Time::from_unix_millis(new_end);
        self.setpoint =
            time::Time::from_unix_millis(self.setpoint.unix_millis().clamp(new_start, new_end));
        self.start_track = Track::Frozen;
        self.end_track = Track::Frozen;
        self.setpoint_track = Track::Frozen;
        self.window_ms = new_span;
    }

    pub fn selected_annotation_ids(&self) -> &[String] {
        &self.selected_annotation_ids
    }

    pub fn is_annotation_selected(&self, annotation_id: &str) -> bool {
        self.selected_annotation_ids
            .iter()
            .any(|id| id == annotation_id)
    }

    pub fn is_comparison_mode(&self) -> bool {
        self.selected_annotation_ids.len() >= 2
    }

    pub fn clear_selected_annotations(&mut self) {
        self.selected_annotation_ids.clear();
    }

    /// Single-click an annotation pill: select it exclusively and snap `[start, end]` to its range
    /// (or center a window around a point bookmark).
    pub fn select_single_annotation(
        &mut self,
        annotation_id: String,
        start_utc_ms: i64,
        end_utc_ms: Option<i64>,
    ) {
        self.is_playing = false;
        self.selected_annotation_ids.clear();
        self.selected_annotation_ids.push(annotation_id);

        match end_utc_ms {
            Some(end_ms) if end_ms > start_utc_ms => {
                self.snap_to_range_ms(start_utc_ms, end_ms);
            }
            _ => {
                let half_win = (self.window_ms / 2).clamp(5_000, 30_000);
                let lo = start_utc_ms.saturating_sub(half_win);
                let hi = start_utc_ms.saturating_add(half_win);
                self.snap_to_range_ms(lo, hi);
                self.setpoint = time::Time::from_unix_millis(start_utc_ms);
            }
        }
    }

    /// Shift-click an annotation pill: toggle its membership in `selected_annotation_ids`
    /// to activate or adjust multi-event Comparison / Overlay Mode.
    pub fn toggle_comparison_annotation(
        &mut self,
        annotation_id: String,
        start_utc_ms: i64,
        end_utc_ms: Option<i64>,
    ) {
        if let Some(pos) = self
            .selected_annotation_ids
            .iter()
            .position(|id| id == &annotation_id)
        {
            self.selected_annotation_ids.remove(pos);
        } else {
            let was_empty = self.selected_annotation_ids.is_empty();
            self.selected_annotation_ids.push(annotation_id);
            if was_empty {
                match end_utc_ms {
                    Some(end_ms) if end_ms > start_utc_ms => {
                        self.snap_to_range_ms(start_utc_ms, end_ms);
                    }
                    _ => {
                        let half_win = (self.window_ms / 2).clamp(5_000, 30_000);
                        self.snap_to_range_ms(
                            start_utc_ms.saturating_sub(half_win),
                            start_utc_ms.saturating_add(half_win),
                        );
                        self.setpoint = time::Time::from_unix_millis(start_utc_ms);
                    }
                }
            }
        }
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
        self.is_playing = false;
        self.selected_annotation_ids.clear();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gap_folded_projection_compresses_multi_hour_idle_gap_and_round_trips() {
        // Morning stint: 10:00..10:15 (900_000 ms)
        // Paddock idle gap: 10:15..12:00 (6_300_000 ms = 1h 45m)
        // Afternoon stint: 12:00..12:15 (900_000 ms)
        let day_start = 1_000_000_000;
        let stint1_end = day_start + 900_000;
        let stint2_start = stint1_end + 6_300_000;
        let day_end = stint2_start + 900_000;
        let active = vec![(day_start, stint1_end), (stint2_start, day_end)];

        let unfolded = GapFoldedProjection::new(day_start, day_end, &active, false);
        assert!(!unfolded.is_folded());
        assert_eq!(unfolded.segments().len(), 3);
        assert!(unfolded.segments()[1].is_idle_gap());
        assert!(!unfolded.segments()[1].is_folded_gap());
        // Without folding, 1800s of driving out of 8100s total is only ~22.2% of screen width
        assert!((unfolded.active_screen_fraction() - (1800.0 / 8100.0)).abs() < 0.01);

        let folded = GapFoldedProjection::new(day_start, day_end, &active, true);
        assert!(folded.is_folded());
        assert_eq!(folded.segments().len(), 3);
        assert!(folded.segments()[1].is_folded_gap());
        // With folding, active driving occupies >= 95% of horizontal width!
        assert!(folded.active_screen_fraction() >= 0.95);

        // Verify monotonic round-trip accuracy inside both stints and the gap
        for sample_ms in [
            day_start,
            day_start + 450_000,
            stint1_end,
            stint1_end + 3_150_000,
            stint2_start,
            stint2_start + 450_000,
            day_end,
        ] {
            let frac = folded.time_to_fraction(sample_ms);
            let recovered = folded.fraction_to_time(frac);
            assert!(
                (recovered - sample_ms).abs() <= 5,
                "expected {sample_ms}, got {recovered} (frac={frac})"
            );
        }

        assert_eq!(format_idle_duration(6_300_000), "1h 45m idle");
        assert_eq!(format_idle_duration(720_000), "12m idle");
    }

    #[test]
    fn master_timeline_playback_zoom_and_annotation_comparison() {
        let t0 = time::Time::from_unix_millis(1_700_000_000_000);
        let mut tl = MasterTimeline::live(t0, 30.0).unwrap();
        tl.snap_to_range_ms(1_000_000, 1_060_000);
        tl.set_day_bounds_ms(Some((1_000_000, 1_120_000)));

        // Starting playback when setpoint == end rewinds setpoint to start
        tl.set_playing(true);
        assert!(tl.is_playing());
        assert_eq!(tl.setpoint_utc_ms(), 1_000_000);

        // Advance 2.0s at 2x speed -> +4,000 ms
        tl.set_playback_speed(2.0);
        assert!(tl.advance_playback(2.0));
        assert_eq!(tl.setpoint_utc_ms(), 1_004_000);

        // Step frame +100ms and -50ms
        tl.step_by_ms(100);
        assert!(!tl.is_playing());
        assert_eq!(tl.setpoint_utc_ms(), 1_004_100);
        tl.step_by_ms(-50);
        assert_eq!(tl.setpoint_utc_ms(), 1_004_050);

        // Zoom in down to 100ms minimum clamp
        tl.zoom_around_ms(1_004_050, 0.0001);
        assert_eq!(tl.end_utc_ms() - tl.start_utc_ms(), MIN_WINDOW_MS);

        // Single-click annotation snaps to range; shift-click second annotation enables comparison mode
        tl.select_single_annotation("stint_1".to_owned(), 1_010_000, Some(1_025_000));
        assert!(!tl.is_comparison_mode());
        assert_eq!(tl.start_utc_ms(), 1_010_000);
        assert_eq!(tl.end_utc_ms(), 1_025_000);

        tl.toggle_comparison_annotation("stint_2".to_owned(), 1_040_000, Some(1_055_000));
        assert!(tl.is_comparison_mode());
        assert_eq!(tl.selected_annotation_ids(), &["stint_1", "stint_2"]);

        tl.toggle_comparison_annotation("stint_2".to_owned(), 1_040_000, Some(1_055_000));
        assert!(!tl.is_comparison_mode());
    }
}
