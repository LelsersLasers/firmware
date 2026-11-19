use crate::{action, app, ui, widget_constructor, workspace_view};

/// Draft state for creating or editing a Track Day [`daqcore::storage::Annotation`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnotationDraft {
    pub id: String,
    pub is_new: bool,
    pub is_point_bookmark: bool,
    pub track_day_id: String,
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    pub category: String,
    pub name: String,
    pub color: String,
    pub notes: String,
    pub author: String,
    pub source: String,
    pub min_updated_at_ms: i64,
}

impl AnnotationDraft {
    pub fn new_at_playhead(
        router: &daqcore::router::VirtualTimelineRouter,
        is_point_bookmark: bool,
    ) -> Self {
        let timeline = router.timeline();
        let setpoint_ms = timeline.setpoint_utc_ms();
        let (start_utc_ms, end_utc_ms) = if is_point_bookmark {
            (setpoint_ms, setpoint_ms)
        } else {
            (timeline.start_utc_ms(), timeline.end_utc_ms())
        };
        let track_day_id = timeline
            .active_track_day_id()
            .map(str::to_owned)
            .or_else(|| {
                router
                    .list_track_days()
                    .ok()
                    .and_then(|days| days.last().map(|td| td.id.clone()))
            })
            .unwrap_or_else(|| format_utc_date(setpoint_ms));

        Self {
            id: format!("user_ann_{}", uuid::Uuid::new_v4()),
            is_new: true,
            is_point_bookmark,
            track_day_id,
            start_utc_ms,
            end_utc_ms,
            category: "event".to_owned(),
            name: if is_point_bookmark {
                "Bookmark".to_owned()
            } else {
                "Run 1".to_owned()
            },
            color: "#38BDF8".to_owned(),
            notes: String::new(),
            author: "user".to_owned(),
            source: "manual".to_owned(),
            min_updated_at_ms: 0,
        }
    }

    pub fn from_existing(ann: &daqcore::storage::Annotation) -> Self {
        let is_point = ann.end_utc_ms.is_none_or(|e| e <= ann.start_utc_ms);
        let author =
            if ann.author.trim().is_empty() || ann.author == "daqcore" || ann.author == "auto" {
                "user".to_owned()
            } else {
                ann.author.clone()
            };
        Self {
            id: ann.id.clone(),
            is_new: false,
            is_point_bookmark: is_point,
            track_day_id: ann.track_day_id.clone(),
            start_utc_ms: ann.start_utc_ms,
            end_utc_ms: ann.end_utc_ms.unwrap_or(ann.start_utc_ms),
            category: ann.category.clone(),
            name: ann.name.clone(),
            color: ann.color.clone(),
            notes: ann.notes.clone(),
            author,
            source: "manual".to_owned(),
            min_updated_at_ms: ann.updated_at_ms,
        }
    }

    pub fn into_annotation(self, updated_at_ms: i64) -> daqcore::storage::Annotation {
        let start = self.start_utc_ms;
        let end_utc_ms = if self.is_point_bookmark {
            None
        } else {
            Some(self.end_utc_ms.max(start))
        };
        let author =
            if self.author.trim().is_empty() || self.author == "daqcore" || self.author == "auto" {
                "user".to_owned()
            } else {
                self.author
            };
        daqcore::storage::Annotation {
            id: self.id,
            track_day_id: self.track_day_id,
            name: if self.name.trim().is_empty() {
                "Untitled Event".to_owned()
            } else {
                self.name.trim().to_owned()
            },
            category: self.category,
            color: self.color,
            start_utc_ms: start,
            end_utc_ms,
            author,
            source: "manual".to_owned(),
            notes: self.notes,
            updated_at_ms: updated_at_ms.max(self.min_updated_at_ms),
            deleted: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkFilter {
    All,
    StintsAndEvents,
    Partitions,
    Faults,
}

#[derive(Clone)]
struct TimelinePanDrag {
    origin: eframe::egui::Pos2,
    origin_time_ms: i64,
    start_ms: i64,
    projection: daqcore::timeline::GapFoldedProjection,
    dragging: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TimelineAnnotationCacheKey {
    active_day_id: Option<String>,
    day_min_ms: i64,
    day_max_ms: i64,
    data_revision: u64,
}

#[derive(Debug, Clone)]
struct CachedFaultGroup {
    signal_id: String,
    short_name: String,
    severity: daqcore::storage::FaultSeverity,
    lcd_message: String,
    occurrences: Vec<(i64, Option<i64>)>,
}

#[derive(Debug, Clone)]
struct TimelineAnnotationCache {
    key: TimelineAnnotationCacheKey,
    stints: Vec<daqcore::storage::Annotation>,
    faults: Vec<daqcore::storage::Annotation>,
    fault_starts_and_sevs: Vec<(i64, daqcore::storage::FaultSeverity)>,
    grouped_faults: Vec<CachedFaultGroup>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TimelineLaneCacheKey {
    active_day_id: Option<String>,
    day_min_ms: i64,
    day_max_ms: i64,
    collapse_idle_gaps: bool,
    live_bounds: Option<(i64, i64)>,
    data_revision: u64,
    pixel_cols: usize,
}

#[derive(Debug, Clone)]
struct TimelineLaneCache {
    key: TimelineLaneCacheKey,
    projection: daqcore::timeline::GapFoldedProjection,
    activity_col_peaks: Vec<f32>,
    /// Pre-binned fault onset ticks as `(normalized_x_fraction, max_severity)` per 2px column.
    fault_ticks: Vec<(f32, daqcore::storage::FaultSeverity)>,
}

/// Persistent UI state for the Master Timeline Bar, Sessions/Faults Navigator, and Annotation Editor modal.
#[derive(Debug, Clone)]
pub struct TimelineUiState {
    pub editor: Option<AnnotationDraft>,
    pub show_stints: bool,
    pub show_faults: bool,
    pub show_chunk_navigator: bool,
    pub chunk_search_query: String,
    pub chunk_filter: ChunkFilter,
    pub chunk_days_open_override: Option<bool>,
    pub editing_track_day: Option<(String, String)>,
    pub fault_severity_filter: Option<daqcore::storage::FaultSeverity>,
    pub expanded_fault_ids: std::collections::BTreeSet<String>,
    pub pending_actions: Vec<action::AppAction>,
    span_hint_at: Option<f64>,
    pub(super) requested_sidebar_filter: Option<ChunkFilter>,
    annotation_cache: Option<std::sync::Arc<TimelineAnnotationCache>>,
    lane_cache: Option<std::sync::Arc<TimelineLaneCache>>,
}

impl Default for TimelineUiState {
    fn default() -> Self {
        Self {
            editor: None,
            show_stints: true,
            show_faults: true,
            show_chunk_navigator: false,
            chunk_search_query: String::new(),
            chunk_filter: ChunkFilter::StintsAndEvents,
            chunk_days_open_override: None,
            editing_track_day: None,
            fault_severity_filter: None,
            expanded_fault_ids: std::collections::BTreeSet::new(),
            pending_actions: Vec::new(),
            span_hint_at: None,
            requested_sidebar_filter: None,
            annotation_cache: None,
            lane_cache: None,
        }
    }
}

impl TimelineUiState {
    fn get_or_build_annotation_cache(
        &mut self,
        router: &daqcore::router::VirtualTimelineRouter,
        active_day_id: Option<&str>,
        day_min_ms: i64,
        day_max_ms: i64,
    ) -> std::sync::Arc<TimelineAnnotationCache> {
        let key = TimelineAnnotationCacheKey {
            active_day_id: active_day_id.map(str::to_owned),
            day_min_ms,
            day_max_ms,
            data_revision: router.data_revision(),
        };
        if let Some(cached) = &self.annotation_cache
            && cached.key == key
        {
            return std::sync::Arc::clone(cached);
        }

        let all_cached = router.all_annotations_cached().unwrap_or_default();
        let mut stints: Vec<daqcore::storage::Annotation> = Vec::new();
        let mut faults: Vec<daqcore::storage::Annotation> = Vec::new();
        for ann in all_cached.iter() {
            let in_range = match ann.end_utc_ms {
                None => ann.start_utc_ms >= day_min_ms && ann.start_utc_ms <= day_max_ms,
                Some(end_ms) => ann.start_utc_ms <= day_max_ms && end_ms >= day_min_ms,
            };
            if !in_range || !active_day_id.is_none_or(|day_id| ann.track_day_id == day_id) {
                continue;
            }
            if ann.category == "fault" {
                faults.push(ann.clone());
            } else {
                stints.push(ann.clone());
            }
        }
        stints.sort_by_key(|a| (a.start_utc_ms, a.end_utc_ms.unwrap_or(a.start_utc_ms)));
        faults.sort_by_key(|a| a.start_utc_ms);

        let fault_starts_and_sevs: Vec<(i64, daqcore::storage::FaultSeverity)> = faults
            .iter()
            .map(|a| {
                (
                    a.start_utc_ms,
                    a.fault_severity()
                        .unwrap_or(daqcore::storage::FaultSeverity::Error),
                )
            })
            .collect();

        let grouped_faults: Vec<CachedFaultGroup> = group_fault_annotations(&faults)
            .into_iter()
            .map(|g| CachedFaultGroup {
                signal_id: g.signal_id,
                short_name: g.short_name,
                severity: g.severity,
                lcd_message: g.lcd_message,
                occurrences: g
                    .occurrences
                    .into_iter()
                    .map(|o| (o.start_utc_ms, o.end_utc_ms))
                    .collect(),
            })
            .collect();

        let built = std::sync::Arc::new(TimelineAnnotationCache {
            key,
            stints,
            faults,
            fault_starts_and_sevs,
            grouped_faults,
        });
        self.annotation_cache = Some(std::sync::Arc::clone(&built));
        built
    }

    #[allow(clippy::too_many_arguments)]
    fn get_or_build_lane_cache(
        &mut self,
        router: &daqcore::router::VirtualTimelineRouter,
        active_day_id: Option<&str>,
        day_min_ms: i64,
        day_max_ms: i64,
        collapse_idle_gaps: bool,
        live_bounds: Option<(i64, i64)>,
        active_spans_for_folding: &[(i64, i64)],
        pixel_cols: usize,
        ann_cache: &TimelineAnnotationCache,
    ) -> std::sync::Arc<TimelineLaneCache> {
        let key = TimelineLaneCacheKey {
            active_day_id: active_day_id.map(str::to_owned),
            day_min_ms,
            day_max_ms,
            collapse_idle_gaps,
            live_bounds,
            data_revision: router.data_revision(),
            pixel_cols,
        };
        if let Some(cached) = &self.lane_cache
            && cached.key == key
        {
            return std::sync::Arc::clone(cached);
        }

        let projection = daqcore::timeline::GapFoldedProjection::new(
            day_min_ms,
            day_max_ms,
            active_spans_for_folding,
            collapse_idle_gaps,
        );

        let activity_1hz = router.activity_envelope_1hz(day_min_ms..=day_max_ms);
        let mut activity_col_peaks = if activity_1hz.is_empty() {
            Vec::new()
        } else {
            vec![0.0f32; pixel_cols]
        };
        if !activity_1hz.is_empty() {
            let max_col = (pixel_cols - 1) as f32;
            for &(bucket_ms, intensity) in &activity_1hz {
                let frac = projection.time_to_fraction(bucket_ms);
                let col = ((frac * max_col).round() as usize).min(pixel_cols - 1);
                activity_col_peaks[col] = activity_col_peaks[col].max(intensity);
            }
        }

        let tick_cols = (pixel_cols / 2).max(1);
        let mut col_ticks: Vec<Option<(f32, daqcore::storage::FaultSeverity)>> =
            vec![None; tick_cols];
        for &(start_utc_ms, sev) in &ann_cache.fault_starts_and_sevs {
            let frac = projection.time_to_fraction(start_utc_ms).clamp(0.0, 1.0);
            let idx = ((frac * (tick_cols - 1) as f32).round() as usize).min(tick_cols - 1);
            match &mut col_ticks[idx] {
                Some((_, prev_sev)) => {
                    if sev > *prev_sev {
                        *prev_sev = sev;
                    }
                }
                None => {
                    col_ticks[idx] = Some((frac, sev));
                }
            }
        }
        let fault_ticks: Vec<(f32, daqcore::storage::FaultSeverity)> =
            col_ticks.into_iter().flatten().collect();

        let built = std::sync::Arc::new(TimelineLaneCache {
            key,
            projection,
            activity_col_peaks,
            fault_ticks,
        });
        self.lane_cache = Some(std::sync::Arc::clone(&built));
        built
    }
    pub fn reset_ruler(&mut self) {
        self.span_hint_at = None;
    }

    fn span_hint_opacity(&mut self, now: f64, resizing: bool) -> Option<f32> {
        if resizing {
            self.span_hint_at = Some(now);
        }
        let elapsed = (now - self.span_hint_at?).max(0.0);
        if elapsed >= 3.0 {
            self.span_hint_at = None;
            None
        } else {
            Some(((3.0 - elapsed) / 0.5).min(1.0) as f32)
        }
    }

    pub fn open_new_at_playhead(
        &mut self,
        router: &daqcore::router::VirtualTimelineRouter,
        is_point_bookmark: bool,
    ) {
        self.editor = Some(AnnotationDraft::new_at_playhead(router, is_point_bookmark));
    }

    pub fn open_new_at_timestamp(
        &mut self,
        router: &daqcore::router::VirtualTimelineRouter,
        utc_ms: i64,
    ) {
        let mut draft = AnnotationDraft::new_at_playhead(router, true);
        draft.start_utc_ms = utc_ms;
        draft.end_utc_ms = utc_ms;
        self.editor = Some(draft);
    }

    pub fn open_edit(&mut self, ann: &daqcore::storage::Annotation) {
        self.editor = Some(AnnotationDraft::from_existing(ann));
    }
}

fn format_span_ms(span_ms: i64) -> String {
    let span_ms = span_ms.max(0);
    let seconds = span_ms / 1_000;
    let millis = span_ms % 1_000;
    if seconds == 0 {
        format!("Δt ({millis} ms)")
    } else if millis == 0 {
        format!("Δt ({seconds} s)")
    } else {
        format!("Δt ({seconds} s {millis} ms)")
    }
}

fn format_window_label(window_ms: i64) -> String {
    let ms = window_ms.max(0);
    let secs = ms as f64 / 1_000.0;
    for &(preset_secs, label) in &daqcore::timeline::WINDOW_DURATION_PRESETS {
        if (secs - preset_secs).abs() < 0.05 {
            return label.to_owned();
        }
    }
    if ms < 1_000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        if ms % 1_000 == 0 {
            format!("{}s", ms / 1_000)
        } else {
            format!("{secs:.1}s")
        }
    } else if ms < 3_600_000 {
        let total_secs = (ms + 500) / 1_000;
        let mins = total_secs / 60;
        let rem_secs = total_secs % 60;
        if rem_secs == 0 {
            format!("{mins}min")
        } else {
            format!("{mins}m{rem_secs}s")
        }
    } else {
        let total_mins = (ms + 30_000) / 60_000;
        let hours = total_mins / 60;
        let rem_mins = total_mins % 60;
        if rem_mins == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h{rem_mins}m")
        }
    }
}

fn format_utc_date(utc_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(utc_ms)
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "1970-01-01".to_owned())
}

fn parse_hex_color(hex: &str, fallback: eframe::egui::Color32) -> eframe::egui::Color32 {
    let trimmed = hex.trim().strip_prefix('#').unwrap_or(hex.trim());
    if trimmed.len() == 6
        && let Ok(r) = u8::from_str_radix(&trimmed[0..2], 16)
        && let Ok(g) = u8::from_str_radix(&trimmed[2..4], 16)
        && let Ok(b) = u8::from_str_radix(&trimmed[4..6], 16)
    {
        return eframe::egui::Color32::from_rgb(r, g, b);
    }
    fallback
}

fn provenance_badge_style(
    provenance: Option<&daqcore::router::ClockProvenance>,
    accent_color: eframe::egui::Color32,
) -> (String, eframe::egui::Color32, &'static str) {
    match provenance {
        Some(p @ daqcore::router::ClockProvenance::CanonicalPartition(mapping)) => {
            let color = match mapping.confidence {
                daqcore::log_parse::correlate::ClockConfidence::GpsSynced { .. } => {
                    eframe::egui::Color32::from_rgb(34, 197, 94)
                }
                daqcore::log_parse::correlate::ClockConfidence::RtcEstimated
                | daqcore::log_parse::correlate::ClockConfidence::ManualOffset => {
                    eframe::egui::Color32::from_rgb(56, 189, 248)
                }
                daqcore::log_parse::correlate::ClockConfidence::FileMtimeEstimated => {
                    eframe::egui::Color32::from_rgb(245, 158, 11)
                }
            };
            (
                p.badge_label(),
                color,
                "Canonical SD Partition clock correlation provenance at Playhead",
            )
        }
        Some(p @ daqcore::router::ClockProvenance::LiveProvisional) => (
            p.badge_label(),
            accent_color,
            "Provisional Live UDP / CAN telemetry stream",
        ),
        None => (
            "Unlogged".to_owned(),
            eframe::egui::Color32::from_gray(130),
            "Playhead is currently over an unlogged interval",
        ),
    }
}

pub fn jump_to_fault_occurrence(
    router: &mut daqcore::router::VirtualTimelineRouter,
    fault_ms: i64,
) {
    let tl = router.timeline_mut();
    tl.set_playing(false);
    let half_win = (tl.window_ms() / 2).clamp(2_500, 10_000);
    tl.snap_to_range_ms(
        fault_ms.saturating_sub(half_win),
        fault_ms.saturating_add(half_win),
    );
    tl.set_setpoint(daqcore::time::Time::from_unix_millis(fault_ms));
}

pub fn show(
    app: &mut app::ViewerState,
    platform: &mut dyn app::ViewerPlatform,
    ctx: &eframe::egui::Context,
) {
    let dt_secs = ctx.input(|i| i.stable_dt as f64).clamp(0.0, 0.25);
    if app.router.timeline().is_playing() {
        app.router.timeline_mut().advance_playback(dt_secs);
        ctx.request_repaint();
    }

    let theme = ui::theme::get_theme(ctx);
    let mut span_changed = false;
    let bottom_frame = eframe::egui::Frame::side_top_panel(&ctx.style())
        .fill(theme.panel_color())
        .stroke(eframe::egui::Stroke::new(1.0_f32, theme.border_color()))
        .inner_margin(eframe::egui::Margin::symmetric(8, 4));
    eframe::egui::TopBottomPanel::bottom("shared_timeline")
        .frame(bottom_frame)
        .show(ctx, |ui| {
            span_changed = show_timeline_contents(
                ui,
                &mut app.router,
                &mut app.timeline_ui,
                &mut app.bus_load_samples,
                &mut app.diagnostic,
            );
        });

    if let Some(filter) = app.timeline_ui.requested_sidebar_filter.take() {
        app.timeline_ui.chunk_filter = filter;
        app.timeline_ui.chunk_search_query.clear();
        app.is_sidebar_open = true;
        app.sidebar_tab = ui::sidebar::SidebarTab::Sessions;
    }

    show_chunk_navigator_window(
        ctx,
        &mut app.router,
        &mut app.timeline_ui,
        &mut app.diagnostic,
    );

    show_annotation_editor_modal(
        ctx,
        &mut app.router,
        &mut app.timeline_ui,
        &mut app.diagnostic,
    );

    if !app.timeline_ui.pending_actions.is_empty() {
        app.action_queue
            .append(&mut app.timeline_ui.pending_actions);
    }

    if span_changed {
        platform.on_workspace_changed(app);
    }
}

/// Render the bottom-docked Master Timeline & Single-Row Transport Bar inside `ui`. Returns `true` if `window_secs` changed.
pub fn show_timeline_contents(
    ui: &mut eframe::egui::Ui,
    router: &mut daqcore::router::VirtualTimelineRouter,
    timeline_ui: &mut TimelineUiState,
    bus_load_samples: &mut Vec<workspace_view::BusLoadSample>,
    diagnostic: &mut Option<String>,
) -> bool {
    let theme = ui::theme::get_theme(ui.ctx());
    let accent_color = theme.accent_color();
    let previous_span = router.timeline().window_secs();
    ui.spacing_mut().item_spacing = eframe::egui::vec2(4.0, 2.0);

    // 1. Unified Single Timeline Ribbon (Track Day / Session overview + Playhead handle + Window Drag Rail + Annotations)
    show_day_overview_lane(ui, router, timeline_ui, diagnostic, accent_color);

    ui.add_space(2.0);

    // 2. Clean Single-Row Transport & Action Bar
    show_transport_bar(ui, router, timeline_ui, bus_load_samples, diagnostic);

    if let Some(error) = diagnostic {
        ui.colored_label(ui.visuals().error_fg_color, error);
    }

    previous_span != router.timeline().window_secs()
}

fn paint_play_icon(
    ui: &eframe::egui::Ui,
    rect: eframe::egui::Rect,
    running: bool,
    color: eframe::egui::Color32,
) {
    let center = rect.center();
    if running {
        for dx in [-2.5_f32, 2.5_f32] {
            ui.painter().rect_filled(
                eframe::egui::Rect::from_center_size(
                    center + eframe::egui::vec2(dx, 0.0),
                    eframe::egui::vec2(2.0, 9.0),
                ),
                0.0,
                color,
            );
        }
    } else {
        ui.painter().add(eframe::egui::Shape::convex_polygon(
            vec![
                center + eframe::egui::vec2(-3.0, -4.5),
                center + eframe::egui::vec2(4.0, 0.0),
                center + eframe::egui::vec2(-3.0, 4.5),
            ],
            color,
            eframe::egui::Stroke::NONE,
        ));
    }
}

/// Group fault annotations by canonical signal ID (`msg.sig`), sorted by severity descending then occurrence count.
pub(crate) struct GroupedFault<'a> {
    pub signal_id: String,
    pub short_name: String,
    pub severity: daqcore::storage::FaultSeverity,
    pub lcd_message: String,
    pub occurrences: Vec<&'a daqcore::storage::Annotation>,
}

pub(crate) fn group_fault_annotations(
    faults: &[daqcore::storage::Annotation],
) -> Vec<GroupedFault<'_>> {
    let mut groups: Vec<GroupedFault<'_>> = Vec::new();
    let mut index_by_sig: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for ann in faults {
        let sig_id_ref = ann.fault_signal_id().unwrap_or(ann.name.as_str());
        let sev = ann
            .fault_severity()
            .unwrap_or(daqcore::storage::FaultSeverity::Error);
        let lcd = ann.fault_lcd_message().unwrap_or("");
        if let Some(&idx) = index_by_sig.get(sig_id_ref) {
            let existing = &mut groups[idx];
            if sev > existing.severity {
                existing.severity = sev;
            }
            if existing.lcd_message.is_empty() && !lcd.is_empty() {
                existing.lcd_message = lcd.to_owned();
            }
            existing.occurrences.push(ann);
        } else {
            let sig_id = sig_id_ref.to_owned();
            let short_name = sig_id
                .split_once('.')
                .map(|(_, s)| s.to_owned())
                .unwrap_or_else(|| sig_id.clone());
            let idx = groups.len();
            index_by_sig.insert(sig_id.clone(), idx);
            groups.push(GroupedFault {
                signal_id: sig_id,
                short_name,
                severity: sev,
                lcd_message: lcd.to_owned(),
                occurrences: vec![ann],
            });
        }
    }
    for g in &mut groups {
        g.occurrences.sort_by_key(|a| a.start_utc_ms);
    }
    groups.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then_with(|| b.occurrences.len().cmp(&a.occurrences.len()))
            .then_with(|| a.signal_id.cmp(&b.signal_id))
    });
    groups
}

fn show_transport_bar(
    ui: &mut eframe::egui::Ui,
    router: &mut daqcore::router::VirtualTimelineRouter,
    timeline_ui: &mut TimelineUiState,
    bus_load_samples: &mut Vec<workspace_view::BusLoadSample>,
    diagnostic: &mut Option<String>,
) {
    let theme = ui::theme::get_theme(ui.ctx());
    let accent_color = theme.accent_color();
    let border_color = theme.border_color();
    let bg_color = ui.visuals().extreme_bg_color;
    let active_day_id = router.timeline().active_track_day_id().map(str::to_owned);

    let captured = router.captured_time_span();
    let (day_min_ms, day_max_ms) = router
        .timeline()
        .day_bounds_ms()
        .or_else(|| captured.map(|(s, e)| (s.unix_millis(), e.unix_millis())))
        .unwrap_or_else(|| {
            (
                router.timeline().start_utc_ms(),
                router.timeline().end_utc_ms(),
            )
        });
    let day_min_ms = day_min_ms.min(router.timeline().start_utc_ms());
    let day_max_ms = day_max_ms
        .max(router.timeline().end_utc_ms())
        .max(day_min_ms + 1_000);

    let ann_cache = timeline_ui.get_or_build_annotation_cache(
        router,
        active_day_id.as_deref(),
        day_min_ms,
        day_max_ms,
    );
    let stint_annotations = &ann_cache.stints;
    let grouped_faults = &ann_cache.grouped_faults;
    let fault_count = ann_cache.faults.len();
    let highest_fault_sev = grouped_faults.first().map(|g| g.severity);

    let setpoint_ms = router.timeline().setpoint_utc_ms();
    let provenance = router.clock_provenance_at(setpoint_ms);
    let (prov_text, prov_color, prov_hover) =
        provenance_badge_style(provenance.as_ref(), accent_color);
    let active_res = match router
        .timeline()
        .resolution_for_width(ui.available_width().max(1.0))
    {
        daqcore::router::Resolution::Overview1Hz => "1Hz",
        daqcore::router::Resolution::Overview10Hz => "10Hz",
        daqcore::router::Resolution::FullRes => "Full-Res",
    };
    let tile_cache_mib = router.tile_cache().lru_bytes() as f64 / (1024.0 * 1024.0);
    let partition_count = router.partitions().len();
    let live_frame_count = router.live_buffer().len();
    let viewport_start_lbl = router.timeline().start().label();
    let viewport_end_lbl = router.timeline().end().label();
    let playhead_lbl = router.timeline().setpoint().label();
    let active_date_lbl = active_day_id
        .clone()
        .unwrap_or_else(|| format_utc_date(setpoint_ms));

    let mut menu_single_click: Option<(String, i64, Option<i64>)> = None;
    let mut menu_jump_fault_ms: Option<i64> = None;
    let mut clear_live_clicked = false;
    let mut open_new_bookmark = false;
    let mut open_new_interval = false;
    let mut clear_comparison = false;

    let row_height = ui.spacing().interact_size.y.max(22.0);
    let (row_rect, _) = ui.allocate_exact_size(
        eframe::egui::vec2(ui.available_width().max(60.0), row_height),
        eframe::egui::Sense::hover(),
    );

    let mut row_ui = ui.new_child(eframe::egui::UiBuilder::new().max_rect(row_rect).layout(
        eframe::egui::Layout::left_to_right(eframe::egui::Align::Center),
    ));
    row_ui.set_clip_rect(ui.clip_rect().intersect(row_rect));
    row_ui.spacing_mut().item_spacing = eframe::egui::vec2(4.0, 2.0);

    // -------------------------------------------------------------------------
    // 1. Left: Playhead Timestamp & Compact Provenance Badge
    // -------------------------------------------------------------------------
    let ts_btn = eframe::egui::Button::new(
        eframe::egui::RichText::new(format!("{active_date_lbl}  {playhead_lbl}"))
            .monospace()
            .size(11.0)
            .strong()
            .color(row_ui.visuals().strong_text_color()),
    )
    .fill(bg_color)
    .stroke(eframe::egui::Stroke::new(1.0_f32, border_color))
    .corner_radius(3.0);
    let ts_resp = row_ui.add(ts_btn).on_hover_text(format!(
        "Playhead: {active_date_lbl} {playhead_lbl} UTC\nViewport: {viewport_start_lbl} .. {viewport_end_lbl} ({:.2}s)",
        router.timeline().window_secs()
    ));
    eframe::egui::Popup::menu(&ts_resp).show(|ui| {
        ui.label(
            eframe::egui::RichText::new(format!("{active_date_lbl} {playhead_lbl} UTC"))
                .monospace()
                .strong(),
        );
        ui.label(
            eframe::egui::RichText::new(format!(
                "Window: {viewport_start_lbl} .. {viewport_end_lbl} ({:.2}s)",
                router.timeline().window_secs()
            ))
            .small()
            .weak(),
        );
    });

    if row_rect.width() >= 680.0 {
        let prov_pill = eframe::egui::Button::new(
            eframe::egui::RichText::new(format!("{prov_text} · {active_res}"))
                .color(prov_color)
                .size(10.0),
        )
        .fill(prov_color.gamma_multiply(0.12))
        .stroke(eframe::egui::Stroke::new(
            1.0_f32,
            prov_color.gamma_multiply(0.45),
        ))
        .corner_radius(3.0)
        .sense(eframe::egui::Sense::hover());
        row_ui.add(prov_pill).on_hover_text(format!(
            "{prov_hover}\nLOD Tier: {active_res}\nTile Cache: {tile_cache_mib:.1} MiB\nSD Partitions: {partition_count} · Live Frames: {live_frame_count}"
        ));
    }

    if ui::cloud_stats::show_loading_activity_pill(&mut row_ui, router, false, 0, false) {
        timeline_ui
            .pending_actions
            .push(action::AppAction::SpawnWidget(
                widget_constructor::WidgetConstructor::CloudStats,
            ));
    }

    let left_end_x = row_ui.cursor().left().max(row_rect.left());

    // -------------------------------------------------------------------------
    // 2. Center: Centered Play / Time Control Deck (`[⏮] [‹] [▶/⏸] [›] [⏭] [1x] [●] [30s]`)
    // -------------------------------------------------------------------------
    let has_stints = !stint_annotations.is_empty();
    let show_speed = row_rect.width() >= 460.0;
    let show_window_dropdown = row_rect.width() >= 520.0;
    let estimated_deck_width = 18.0
        + 24.0
        + 18.0
        + 22.0
        + if has_stints { 48.0 } else { 0.0 }
        + if show_speed { 50.0 } else { 0.0 }
        + if show_window_dropdown { 66.0 } else { 0.0 }
        + 16.0;
    let ideal_deck_left = row_rect.center().x - estimated_deck_width * 0.5;
    let deck_left = ideal_deck_left.max(left_end_x + 6.0).min(row_rect.right());
    if deck_left > left_end_x {
        row_ui.add_space(deck_left - left_end_x);
    }

    let cur_start = router.timeline().start_utc_ms();
    if has_stints
        && row_ui
            .add(
                eframe::egui::Button::new(eframe::egui::RichText::new("⏮").size(11.0))
                    .frame(false)
                    .min_size(eframe::egui::vec2(20.0, row_height)),
            )
            .on_hover_text("Previous Driving Stint / Event ([)")
            .clicked()
    {
        let prev = stint_annotations
            .iter()
            .rev()
            .find(|a| a.start_utc_ms < cur_start - 500)
            .or_else(|| stint_annotations.last());
        if let Some(ann) = prev {
            menu_single_click = Some((ann.id.clone(), ann.start_utc_ms, ann.end_utc_ms));
        }
    }

    let fine = row_ui.input(|i| i.modifiers.alt);
    if row_ui
        .add(
            eframe::egui::Button::new(eframe::egui::RichText::new("‹").size(16.0))
                .frame(false)
                .min_size(eframe::egui::vec2(18.0, row_height)),
        )
        .on_hover_text("Step playhead backward 100 ms (Hold Alt for 10 ms)")
        .clicked()
    {
        router
            .timeline_mut()
            .step_by_ms(if fine { -10 } else { -100 });
    }

    let is_playing = router.timeline().is_playing();
    let play_resp = row_ui
        .add(
            eframe::egui::Button::new("")
                .fill(accent_color.gamma_multiply(0.18))
                .stroke(eframe::egui::Stroke::new(
                    1.0_f32,
                    accent_color.gamma_multiply(0.75),
                ))
                .corner_radius(3.0)
                .min_size(eframe::egui::vec2(24.0, row_height - 2.0)),
        )
        .on_hover_text("Play / pause playhead scrub (Space)");
    paint_play_icon(&row_ui, play_resp.rect, is_playing, theme.text_color());
    if play_resp.clicked() {
        router.timeline_mut().toggle_playing();
    }

    if row_ui
        .add(
            eframe::egui::Button::new(eframe::egui::RichText::new("›").size(16.0))
                .frame(false)
                .min_size(eframe::egui::vec2(18.0, row_height)),
        )
        .on_hover_text("Step playhead forward 100 ms (Hold Alt for 10 ms)")
        .clicked()
    {
        router
            .timeline_mut()
            .step_by_ms(if fine { 10 } else { 100 });
    }

    if has_stints
        && row_ui
            .add(
                eframe::egui::Button::new(eframe::egui::RichText::new("⏭").size(11.0))
                    .frame(false)
                    .min_size(eframe::egui::vec2(20.0, row_height)),
            )
            .on_hover_text("Next Driving Stint / Event (])")
            .clicked()
    {
        let next = stint_annotations
            .iter()
            .find(|a| a.start_utc_ms > cur_start + 500)
            .or_else(|| stint_annotations.first());
        if let Some(ann) = next {
            menu_single_click = Some((ann.id.clone(), ann.start_utc_ms, ann.end_utc_ms));
        }
    }

    if show_speed {
        let current_speed = router.timeline().playback_speed();
        eframe::egui::ComboBox::from_id_salt("timeline_playback_speed")
            .width(46.0)
            .selected_text(format!("{current_speed}x"))
            .show_ui(&mut row_ui, |ui| {
                for &preset in &daqcore::timeline::PLAYBACK_SPEED_PRESETS {
                    if ui
                        .selectable_label(
                            (current_speed - preset).abs() < 1e-4,
                            format!("{preset}x"),
                        )
                        .clicked()
                    {
                        router.timeline_mut().set_playback_speed(preset);
                    }
                }
            });
    }

    let is_live = router.timeline().is_live();
    let live_resp = row_ui
        .add(
            eframe::egui::Button::new("")
                .fill(bg_color)
                .stroke(eframe::egui::Stroke::new(1.0_f32, border_color))
                .corner_radius(3.0)
                .min_size(eframe::egui::vec2(22.0, row_height - 2.0)),
        )
        .on_hover_text("Follow incoming live telemetry (click to toggle)");
    row_ui.painter().circle_filled(
        live_resp.rect.center(),
        4.0,
        if is_live {
            eframe::egui::Color32::from_rgb(239, 68, 68)
        } else {
            theme.muted_text_color().gamma_multiply(0.45)
        },
    );
    if live_resp.clicked() {
        let tl = router.timeline_mut();
        if is_live {
            tl.set_start(tl.start());
            tl.set_end(tl.end());
            tl.set_setpoint(tl.setpoint());
        } else {
            tl.go_live();
        }
    }

    if show_window_dropdown {
        let current_window_ms = router.timeline().window_ms();
        let current_window_secs = current_window_ms as f64 / 1_000.0;
        let window_label = format_window_label(current_window_ms);
        let combo_resp = eframe::egui::ComboBox::from_id_salt("timeline_window_size")
            .width(60.0)
            .selected_text(window_label)
            .show_ui(&mut row_ui, |ui| {
                for &(preset_secs, label) in &daqcore::timeline::WINDOW_DURATION_PRESETS {
                    if ui
                        .selectable_label(
                            (current_window_secs - preset_secs).abs() < 0.05,
                            label,
                        )
                        .clicked()
                    {
                        if let Err(err) = router
                            .timeline_mut()
                            .apply_window_duration_secs(preset_secs)
                        {
                            *diagnostic = Some(err);
                        } else {
                            timeline_ui.reset_ruler();
                        }
                    }
                }
            });
        combo_resp
            .response
            .on_hover_text("Viewport window duration (click to choose preset)");
    }

    // -------------------------------------------------------------------------
    // 3. Right: Explicit Mark Timestamp & Mark Interval Buttons + Quick Actions
    // -------------------------------------------------------------------------
    let right_left = row_ui.cursor().left().max(row_rect.left());
    let right_rect = eframe::egui::Rect::from_min_max(
        eframe::egui::pos2(right_left, row_rect.top()),
        row_rect.right_bottom(),
    );
    if right_rect.width() >= 24.0 {
        row_ui.scope_builder(
            eframe::egui::UiBuilder::new()
                .max_rect(right_rect)
                .layout(eframe::egui::Layout::right_to_left(
                    eframe::egui::Align::Center,
                )),
            |ui| {
                ui.spacing_mut().item_spacing.x = 5.0;

                // 1. Overflow Menu `[⋯]`
                let more_resp = ui
                    .add(
                        eframe::egui::Button::new("")
                            .frame(false)
                            .min_size(eframe::egui::vec2(22.0, row_height)),
                    )
                    .on_hover_text("Timeline options, window duration & keyboard shortcuts");
                for dx in [-4.0_f32, 0.0_f32, 4.0_f32] {
                    ui.painter().circle_filled(
                        more_resp.rect.center() + eframe::egui::vec2(dx, 0.0),
                        1.3,
                        theme.muted_text_color(),
                    );
                }
                eframe::egui::Popup::menu(&more_resp).show(|ui| {
                    ui.set_min_width(210.0);
                    ui.label(eframe::egui::RichText::new("Viewport Window").weak().small());
                    ui.horizontal(|ui| {
                        ui.label("Duration:");
                        let tl = router.timeline_mut();
                        let mut span = tl.window_secs();
                        if ui
                            .add(
                                eframe::egui::DragValue::new(&mut span)
                                    .range(0.1..=86_400.0)
                                    .speed(0.5)
                                    .suffix(" s"),
                            )
                            .changed()
                        {
                            if let Err(err) = tl.apply_window_duration_secs(span) {
                                *diagnostic = Some(err);
                            }
                        }
                        if ui.small_button("−").on_hover_text("Zoom out (J)").clicked() {
                            let sp_ms = router.timeline().setpoint_utc_ms();
                            router.timeline_mut().zoom_around_ms(sp_ms, 2.0);
                        }
                        if ui.small_button("+").on_hover_text("Zoom in (K)").clicked() {
                            let sp_ms = router.timeline().setpoint_utc_ms();
                            router.timeline_mut().zoom_around_ms(sp_ms, 0.5);
                        }
                    });
                    if ui.button("↔ Fit Entire Track Day (F)").clicked() {
                        if let Some((d_min, d_max)) = router.timeline().day_bounds_ms() {
                            router.timeline_mut().snap_to_range_ms(d_min, d_max);
                            timeline_ui.reset_ruler();
                        }
                        ui.close();
                    }
                    let mut fold = router.timeline().collapse_idle_gaps();
                    if ui.checkbox(&mut fold, "Fold idle gaps >= 60s (G)").changed() {
                        router.timeline_mut().set_collapse_idle_gaps(fold);
                    }
                    ui.separator();
                    ui.label(eframe::egui::RichText::new("Timeline Overlays").weak().small());
                    ui.checkbox(&mut timeline_ui.show_stints, "Show runs & event pills");
                    ui.checkbox(&mut timeline_ui.show_faults, "Show severity fault ticks");
                    ui.separator();
                    if ui.button("📍 Mark Timestamp at Playhead (M)").clicked() {
                        open_new_bookmark = true;
                        ui.close();
                    }
                    if ui.button("🏷 Mark Interval [start..end]").clicked() {
                        open_new_interval = true;
                        ui.close();
                    }
                    if ui.button("📅 Open Runs & Faults in Sidebar").clicked() {
                        timeline_ui.requested_sidebar_filter = Some(ChunkFilter::StintsAndEvents);
                        ui.close();
                    }
                    if live_frame_count > 0 {
                        ui.separator();
                        if ui.button("Clear Live Buffer").clicked() {
                            clear_live_clicked = true;
                            ui.close();
                        }
                    }
                    ui.separator();
                    ui.menu_button("⌨ Keyboard Shortcuts", |ui| {
                        eframe::egui::Grid::new("timeline_shortcuts_grid")
                            .num_columns(2)
                            .spacing([12.0, 3.0])
                            .show(ui, |ui| {
                                for (act, key) in [
                                    ("Play / Pause", "Space"),
                                    ("Step Playhead (1%)", "H / L or Left / Right"),
                                    ("Fine Step (±10 ms)", "Alt + Left / Right"),
                                    ("Pan Window (10%)", "Shift + Left / Right"),
                                    ("Zoom In / Out", "K / J or Up / Down"),
                                    ("Prev / Next Stint", "[ / ]"),
                                    ("Fit Track Day", "F"),
                                    ("Fold Idle Gaps", "G"),
                                    ("Mark Timestamp", "M"),
                                    ("Focus Search", "/"),
                                ] {
                                    ui.label(act);
                                    ui.label(eframe::egui::RichText::new(key).monospace().weak());
                                    ui.end_row();
                                }
                            });
                    });
                });

                // 2. Explicit `🏷 Mark Interval` button (marks active [start..end] viewport)
                if ui.available_width() >= 96.0
                    && ui
                        .small_button("🏷 Mark Interval")
                        .on_hover_text(
                            "Create an interval event annotation spanning the active viewport [start..end]",
                        )
                        .clicked()
                {
                    open_new_interval = true;
                }

                // 3. Explicit `📍 Mark Time` button (marks single playhead timestamp)
                if ui.available_width() >= 84.0
                    && ui
                        .small_button("📍 Mark Time")
                        .on_hover_text("Drop a point bookmark at Playhead (M)")
                        .clicked()
                {
                    open_new_bookmark = true;
                }

                // 4. Fit Day Button
                if ui.available_width() >= 64.0
                    && let Some((day_min, day_max)) = router.timeline().day_bounds_ms()
                    && ui
                        .small_button("Fit Day")
                        .on_hover_text("Zoom out to the full Track Day overview (F)")
                        .clicked()
                {
                    router.timeline_mut().snap_to_range_ms(day_min, day_max);
                    timeline_ui.reset_ruler();
                }

                // 5. Fold Gaps Toggle Pill
                if ui.available_width() >= 76.0 {
                    let mut fold_gaps = router.timeline().collapse_idle_gaps();
                    let fold_btn = eframe::egui::Button::new(
                        eframe::egui::RichText::new("Fold Gaps")
                            .size(10.5)
                            .color(if fold_gaps {
                                accent_color
                            } else {
                                theme.muted_text_color()
                            }),
                    )
                    .fill(if fold_gaps {
                        accent_color.gamma_multiply(0.16)
                    } else {
                        bg_color
                    })
                    .stroke(eframe::egui::Stroke::new(
                        1.0_f32,
                        if fold_gaps {
                            accent_color.gamma_multiply(0.6)
                        } else {
                            border_color
                        },
                    ))
                    .corner_radius(3.0);
                    if ui
                        .add(fold_btn)
                        .on_hover_text(
                            "Collapse unlogged intervals (>= 60s) into thin dividers (G)",
                        )
                        .clicked()
                    {
                        fold_gaps = !fold_gaps;
                        router.timeline_mut().set_collapse_idle_gaps(fold_gaps);
                    }
                }

                // 5. Severity-Colored `[⚠ Faults (N)]` Dropdown Menu
                if !grouped_faults.is_empty() && ui.available_width() >= 95.0 {
                    let sev_col = highest_fault_sev
                        .map(|s| theme.fault_severity_color(s))
                        .unwrap_or_else(|| theme.warning_color());
                    ui.scope(|ui| {
                        ui.visuals_mut().widgets.inactive.bg_fill = sev_col.gamma_multiply(0.14);
                        ui.visuals_mut().widgets.inactive.bg_stroke =
                            eframe::egui::Stroke::new(1.0_f32, sev_col.gamma_multiply(0.55));
                        ui.menu_button(
                            eframe::egui::RichText::new(format!(
                                "⚠ Faults ({})",
                                fault_count
                            ))
                            .size(10.5)
                            .strong()
                            .color(sev_col),
                            |ui| {
                                ui.set_min_width(340.0);
                                ui.horizontal(|ui| {
                                    ui.checkbox(
                                        &mut timeline_ui.show_faults,
                                        "Show fault ticks on timeline",
                                    );
                                    if ui
                                        .small_button("Open in Sidebar ▶")
                                        .on_hover_text(
                                            "Open the Faults tab in the left Sessions sidebar",
                                        )
                                        .clicked()
                                    {
                                        timeline_ui.requested_sidebar_filter =
                                            Some(ChunkFilter::Faults);
                                        ui.close();
                                    }
                                });
                                ui.separator();
                                eframe::egui::ScrollArea::vertical()
                                    .max_height(300.0)
                                    .show(ui, |ui| {
                                        for group in grouped_faults {
                                            let g_col = theme.fault_severity_color(group.severity);
                                            let first_ms = group.occurrences[0].0;
                                            let first_lbl =
                                                daqcore::time::Time::from_unix_millis(first_ms)
                                                    .label();
                                            let header_txt = if group.lcd_message.is_empty() {
                                                format!(
                                                    "[{}] {}  ({}× · 1st @ {})",
                                                    group.severity.badge_label(),
                                                    group.short_name,
                                                    group.occurrences.len(),
                                                    first_lbl
                                                )
                                            } else {
                                                format!(
                                                    "[{}] {} — {} ({}×)",
                                                    group.severity.badge_label(),
                                                    group.short_name,
                                                    group.lcd_message,
                                                    group.occurrences.len()
                                                )
                                            };
                                            let collapsing = ui.collapsing(
                                                eframe::egui::RichText::new(header_txt)
                                                    .color(g_col)
                                                    .size(11.0),
                                                |ui| {
                                                    ui.horizontal(|ui| {
                                                        if ui
                                                            .small_button("Open in State Lane")
                                                            .clicked()
                                                        {
                                                            timeline_ui.pending_actions.push(
                                                                action::AppAction::SpawnWidget(
                                                                    widget_constructor::WidgetConstructor::StateLaneSingle {
                                                                        signal_id: group.signal_id.clone(),
                                                                        label: group.short_name.clone(),
                                                                    },
                                                                ),
                                                            );
                                                            ui.close();
                                                        }
                                                        if ui
                                                            .small_button("Plot in YT Plot")
                                                            .clicked()
                                                        {
                                                            timeline_ui.pending_actions.push(
                                                                action::AppAction::SpawnWidget(
                                                                    widget_constructor::WidgetConstructor::ScopeChannel {
                                                                        signal_id: group.signal_id.clone(),
                                                                        label: group.short_name.clone(),
                                                                        unit: String::new(),
                                                                    },
                                                                ),
                                                            );
                                                            ui.close();
                                                        }
                                                    });
                                                    for (idx, &(occ_start_ms, occ_end_ms)) in group
                                                        .occurrences
                                                        .iter()
                                                        .take(50)
                                                        .enumerate()
                                                    {
                                                        let t_lbl =
                                                            daqcore::time::Time::from_unix_millis(
                                                                occ_start_ms,
                                                            )
                                                            .label();
                                                        let dur_str = occ_end_ms
                                                            .filter(|&e| e > occ_start_ms)
                                                            .map(|e| {
                                                                format!(
                                                                    " ({:.2}s)",
                                                                    (e - occ_start_ms) as f64
                                                                        / 1000.0
                                                                )
                                                            })
                                                            .unwrap_or_default();
                                                        if ui
                                                            .small_button(format!(
                                                                "#{} @ {}{}",
                                                                idx + 1,
                                                                t_lbl,
                                                                dur_str
                                                            ))
                                                            .on_hover_text(
                                                                "Jump playhead and window to this fault occurrence",
                                                            )
                                                            .clicked()
                                                        {
                                                            menu_jump_fault_ms = Some(occ_start_ms);
                                                            ui.close();
                                                        }
                                                    }
                                                },
                                            );
                                            collapsing.header_response.on_hover_text(format!(
                                                "{}\nSeverity: {}\nOccurrences: {}\nClick arrow to view & jump to any occurrence",
                                                group.signal_id,
                                                group.severity.display_name(),
                                                group.occurrences.len()
                                            ));
                                        }
                                    });
                            },
                        );
                    });
                }

                // 6. Active Comparison Mode Pill
                if router.timeline().is_comparison_mode() && ui.available_width() >= 90.0 {
                    let count = router.timeline().selected_annotation_ids().len();
                    let event_color = eframe::egui::Color32::from_rgb(168, 85, 247);
                    let cmp_btn = eframe::egui::Button::new(
                        eframe::egui::RichText::new(format!("Comparing ({count}) ×"))
                            .size(10.5)
                            .strong()
                            .color(event_color),
                    )
                    .fill(event_color.gamma_multiply(0.18))
                    .stroke(eframe::egui::Stroke::new(1.0_f32, event_color))
                    .corner_radius(3.0);
                    if ui
                        .add(cmp_btn)
                        .on_hover_text("Click to exit multi-run Comparison Mode")
                        .clicked()
                    {
                        clear_comparison = true;
                    }
                }
            },
        );
    }

    if let Some((id, s, e)) = menu_single_click {
        router.timeline_mut().select_single_annotation(id, s, e);
    }
    if let Some(fault_ms) = menu_jump_fault_ms {
        jump_to_fault_occurrence(router, fault_ms);
    }
    if clear_live_clicked {
        router.reset_live(daqcore::time::Time::now());
        bus_load_samples.clear();
        timeline_ui.reset_ruler();
    }
    if open_new_bookmark {
        timeline_ui.open_new_at_playhead(router, true);
    }
    if open_new_interval {
        timeline_ui.open_new_at_playhead(router, false);
    }
    if clear_comparison {
        router.timeline_mut().clear_selected_annotations();
    }
}

fn show_day_overview_lane(
    ui: &mut eframe::egui::Ui,
    router: &mut daqcore::router::VirtualTimelineRouter,
    timeline_ui: &mut TimelineUiState,
    diagnostic: &mut Option<String>,
    accent_color: eframe::egui::Color32,
) {
    let all_days_unselected = router.timeline().active_track_day_id().is_none()
        && !router.timeline().is_comparison_mode()
        && !router.timeline().is_live()
        && !router.partitions().is_empty();
    if all_days_unselected {
        let lane_height = 28.0;
        let width = ui.available_width().max(16.0);
        let (outer_rect, _) = ui.allocate_exact_size(
            eframe::egui::vec2(width, lane_height + 15.0),
            eframe::egui::Sense::hover(),
        );
        let rect = eframe::egui::Rect::from_min_max(
            outer_rect.left_top() + eframe::egui::vec2(0.0, 8.0),
            outer_rect.right_bottom() - eframe::egui::vec2(0.0, 7.0),
        );
        let theme = ui::theme::get_theme(ui.ctx());
        let painter = ui.painter_at(rect.intersect(ui.clip_rect()));
        painter.rect_filled(rect, 3.0, theme.canvas_color());
        painter.rect_stroke(
            rect,
            3.0,
            eframe::egui::Stroke::new(1.0_f32, theme.border_color()),
            eframe::egui::StrokeKind::Inside,
        );
        painter.text(
            rect.center(),
            eframe::egui::Align2::CENTER_CENTER,
            "All Track Days — Select a Track Day, Run, Fault, or Chunk from the sidebar (or Shift+Click runs to compare)",
            eframe::egui::FontId::proportional(11.0),
            theme.muted_text_color(),
        );
        return;
    }

    let captured = router.captured_time_span();
    let (day_min_ms, day_max_ms) = router
        .timeline()
        .day_bounds_ms()
        .or_else(|| captured.map(|(s, e)| (s.unix_millis(), e.unix_millis())))
        .unwrap_or_else(|| {
            (
                router.timeline().start_utc_ms(),
                router.timeline().end_utc_ms(),
            )
        });
    let day_min_ms = day_min_ms.min(router.timeline().start_utc_ms());
    let day_max_ms = day_max_ms
        .max(router.timeline().end_utc_ms())
        .max(day_min_ms + 1_000);

    // Collect active spans (canonical partitions + live buffer) for gap folding
    let active_day_filter = router.timeline().active_track_day_id().map(str::to_owned);
    let canonical_spans: Vec<(i64, i64, String)> = router
        .partitions()
        .iter()
        .filter(|p| {
            active_day_filter
                .as_deref()
                .is_none_or(|day_id| p.track_day_id == day_id)
        })
        .map(|p| (p.start_utc_ms, p.end_utc_ms, p.partition_id.clone()))
        .collect();
    let live_bounds = router.live_buffer().time_bounds_ms();

    let mut active_spans_for_folding: Vec<(i64, i64)> =
        canonical_spans.iter().map(|&(s, e, _)| (s, e)).collect();
    if let Some((ls, le)) = live_bounds {
        active_spans_for_folding.push((ls, le));
    }

    // Keep the timeline overview anchored to the full Track Day / Session bounds so stints,
    // bookmarks, and severity-colored fault ticks never vanish when zooming in!
    let collapse_idle_gaps = router.timeline().collapse_idle_gaps() && !router.timeline().is_live();
    let lane_height = 28.0;
    let width = ui.available_width().max(16.0);
    let pixel_cols = (width.ceil() as usize).clamp(1, 2048);

    let ann_cache = timeline_ui.get_or_build_annotation_cache(
        router,
        active_day_filter.as_deref(),
        day_min_ms,
        day_max_ms,
    );
    let lane_cache = timeline_ui.get_or_build_lane_cache(
        router,
        active_day_filter.as_deref(),
        day_min_ms,
        day_max_ms,
        collapse_idle_gaps,
        live_bounds,
        &active_spans_for_folding,
        pixel_cols,
        &ann_cache,
    );
    let projection = &lane_cache.projection;
    let selected_ann_ids: Vec<String> = router.timeline().selected_annotation_ids().to_vec();

    // Keep the protruding playhead handle above the lane and the window drag rail below it.
    let (outer_rect, lane_response) = ui.allocate_exact_size(
        eframe::egui::vec2(width, lane_height + 15.0),
        eframe::egui::Sense::click_and_drag(),
    );
    let rect = eframe::egui::Rect::from_min_max(
        outer_rect.left_top() + eframe::egui::vec2(0.0, 8.0),
        outer_rect.right_bottom() - eframe::egui::vec2(0.0, 7.0),
    );
    let clip_rect = rect.intersect(ui.clip_rect());
    let painter = ui.painter_at(clip_rect);

    // 1. Background (theme-aware canvas color)
    let theme = ui::theme::get_theme(ui.ctx());
    painter.rect_filled(rect, 3.0, theme.canvas_color());

    let time_to_x =
        |utc_ms: i64| -> f32 { rect.left() + projection.time_to_fraction(utc_ms) * rect.width() };
    let x_to_time = |px: f32| -> i64 {
        let frac = ((px - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
        projection.fraction_to_time(frac)
    };

    // Secondary-button panning works over the whole lane, including its grips.
    let pan_id = ui.id().with("day_secondary_pan");
    let (secondary_pressed, secondary_down, secondary_released, pointer) = ui.input(|input| {
        (
            input
                .pointer
                .button_pressed(eframe::egui::PointerButton::Secondary),
            input
                .pointer
                .button_down(eframe::egui::PointerButton::Secondary),
            input
                .pointer
                .button_released(eframe::egui::PointerButton::Secondary),
            input.pointer.interact_pos(),
        )
    });
    if secondary_pressed
        && lane_response.contains_pointer()
        && let Some(pointer) = pointer
    {
        ui.data_mut(|data| {
            data.insert_temp(
                pan_id,
                TimelinePanDrag {
                    origin: pointer,
                    origin_time_ms: x_to_time(pointer.x),
                    start_ms: router.timeline().start_utc_ms(),
                    projection: projection.clone(),
                    dragging: false,
                },
            )
        });
    }
    let mut right_panning = false;
    if let Some(mut drag) = ui.data(|data| data.get_temp::<TimelinePanDrag>(pan_id)) {
        if (secondary_down || secondary_released)
            && let Some(pointer) = pointer
        {
            drag.dragging |= pointer.distance(drag.origin) >= 4.0;
            if drag.dragging {
                right_panning = true;
                let fraction = ((pointer.x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
                let delta_ms = drag.projection.fraction_to_time(fraction) - drag.origin_time_ms;
                let timeline = router.timeline_mut();
                timeline.set_playing(false);
                timeline.pan_to_start_ms(drag.start_ms.saturating_add(delta_ms));
            }
        }
        ui.data_mut(|data| {
            if secondary_down {
                data.insert_temp(pan_id, drag);
            } else {
                data.remove::<TimelinePanDrag>(pan_id);
            }
        });
    }

    // 2. Shade Canonical SD Partitions (solid accent tint)
    let canonical_fill = eframe::egui::Color32::from_rgba_unmultiplied(14, 165, 233, 48);
    let canonical_border = eframe::egui::Color32::from_rgba_unmultiplied(56, 189, 248, 120);
    for (p_start, p_end, _) in &canonical_spans {
        if *p_end < day_min_ms || *p_start > day_max_ms {
            continue;
        }
        let x0 = time_to_x(*p_start).max(rect.left());
        let x1 = time_to_x(*p_end).min(rect.right()).max(x0 + 2.0);
        let part_rect = eframe::egui::Rect::from_min_max(
            eframe::egui::pos2(x0, rect.top() + 1.0),
            eframe::egui::pos2(x1, rect.bottom() - 1.0),
        );
        painter.rect_filled(part_rect, 2.0, canonical_fill);
        painter.rect_stroke(
            part_rect,
            2.0,
            eframe::egui::Stroke::new(1.0_f32, canonical_border),
            eframe::egui::StrokeKind::Inside,
        );
    }

    // 3. Shade Provisional Live UDP region (hatched / dashed accent)
    if let Some((live_start, live_end)) = live_bounds {
        let max_canon = canonical_spans.iter().map(|(_, e, _)| *e).max();
        let eff_start = max_canon.map_or(live_start, |mc| live_start.max(mc));
        if eff_start <= live_end && live_end >= day_min_ms && eff_start <= day_max_ms {
            let x0 = time_to_x(eff_start).max(rect.left());
            let x1 = time_to_x(live_end).min(rect.right()).max(x0 + 3.0);
            let live_rect = eframe::egui::Rect::from_min_max(
                eframe::egui::pos2(x0, rect.top() + 1.0),
                eframe::egui::pos2(x1, rect.bottom() - 1.0),
            );
            let live_color = eframe::egui::Color32::from_rgb(249, 115, 22);
            painter.rect_filled(live_rect, 2.0, live_color.gamma_multiply(0.16));
            let step = 8.0;
            let mut hx = live_rect.left();
            while hx < live_rect.right() {
                let x_end = (hx + 10.0).min(live_rect.right());
                painter.line_segment(
                    [
                        eframe::egui::pos2(hx, live_rect.bottom()),
                        eframe::egui::pos2(x_end, live_rect.top()),
                    ],
                    eframe::egui::Stroke::new(1.0_f32, live_color.gamma_multiply(0.35)),
                );
                hx += step;
            }
            painter.rect_stroke(
                live_rect,
                2.0,
                eframe::egui::Stroke::new(1.0_f32, live_color),
                eframe::egui::StrokeKind::Inside,
            );
        }
    }

    // 4. Render 1Hz Activity Envelope along the bottom half of the Timeline lane as a single Mesh
    if !lane_cache.activity_col_peaks.is_empty() {
        let env_bottom = rect.bottom() - 1.5;
        let env_max_h = 12.0;
        let col_count = lane_cache.activity_col_peaks.len();
        let bar_color = eframe::egui::Color32::from_rgba_unmultiplied(56, 189, 248, 95);
        let half_w = ((rect.width() / col_count as f32) * 0.5).max(0.75);
        let mut mesh = eframe::egui::Mesh::default();
        for (col, &peak) in lane_cache.activity_col_peaks.iter().enumerate() {
            if peak <= 0.0 {
                continue;
            }
            let x = rect.left() + (col as f32 + 0.5) / (col_count as f32) * rect.width();
            let h = (peak * env_max_h).max(1.5);
            let y_top = env_bottom - h;
            let base = mesh.vertices.len() as u32;
            mesh.vertices.push(eframe::egui::epaint::Vertex {
                pos: eframe::egui::pos2(x - half_w, y_top),
                uv: eframe::egui::epaint::WHITE_UV,
                color: bar_color,
            });
            mesh.vertices.push(eframe::egui::epaint::Vertex {
                pos: eframe::egui::pos2(x + half_w, y_top),
                uv: eframe::egui::epaint::WHITE_UV,
                color: bar_color,
            });
            mesh.vertices.push(eframe::egui::epaint::Vertex {
                pos: eframe::egui::pos2(x + half_w, env_bottom),
                uv: eframe::egui::epaint::WHITE_UV,
                color: bar_color,
            });
            mesh.vertices.push(eframe::egui::epaint::Vertex {
                pos: eframe::egui::pos2(x - half_w, env_bottom),
                uv: eframe::egui::epaint::WHITE_UV,
                color: bar_color,
            });
            mesh.indices
                .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
        if !mesh.is_empty() {
            painter.add(eframe::egui::Shape::mesh(mesh));
        }
    }

    // 5. Render Folded Idle Gap dividers
    let gap_fill = if theme.is_light {
        eframe::egui::Color32::from_rgb(228, 234, 242)
    } else {
        eframe::egui::Color32::from_rgb(16, 20, 28)
    };
    let gap_border = theme.border_color();
    let gap_text_color = theme.muted_text_color();

    for (seg_idx, seg) in projection.segments().iter().enumerate() {
        if seg.is_folded_gap() {
            let x0 = rect.left() + seg.frac_start * rect.width();
            let x1 = rect.left() + seg.frac_end * rect.width();
            let gap_rect = eframe::egui::Rect::from_min_max(
                eframe::egui::pos2(x0, rect.top() + 1.0),
                eframe::egui::pos2(x1, rect.bottom() - 1.0),
            );
            let gap_clip = gap_rect.intersect(clip_rect);
            let gap_painter = ui.painter_at(gap_clip);
            gap_painter.rect_filled(gap_rect, 1.0, gap_fill);

            let mut hx = gap_rect.left() - gap_rect.height();
            while hx < gap_rect.right() {
                gap_painter.line_segment(
                    [
                        eframe::egui::pos2(hx, gap_rect.bottom()),
                        eframe::egui::pos2(hx + gap_rect.height(), gap_rect.top()),
                    ],
                    eframe::egui::Stroke::new(1.0_f32, gap_border.gamma_multiply(0.55)),
                );
                hx += 7.0;
            }

            painter.line_segment(
                [gap_rect.left_top(), gap_rect.left_bottom()],
                eframe::egui::Stroke::new(1.0_f32, gap_border),
            );
            painter.line_segment(
                [gap_rect.right_top(), gap_rect.right_bottom()],
                eframe::egui::Stroke::new(1.0_f32, gap_border),
            );

            let full_dur = daqcore::timeline::format_idle_duration(seg.duration_ms());
            let short_dur = full_dur.strip_suffix(" idle").unwrap_or(&full_dur);
            let label = if gap_rect.width() >= 96.0 {
                format!("// {full_dur} //")
            } else if gap_rect.width() >= 62.0 {
                full_dur.clone()
            } else {
                short_dur.to_owned()
            };

            let text_clip = gap_rect
                .shrink2(eframe::egui::vec2(2.0, 0.0))
                .intersect(clip_rect);
            if text_clip.width() >= 18.0 {
                ui.painter_at(text_clip).text(
                    gap_rect.center(),
                    eframe::egui::Align2::CENTER_CENTER,
                    &label,
                    eframe::egui::FontId::monospace(9.0),
                    gap_text_color,
                );
            }

            if ui.rect_contains_pointer(gap_rect) {
                ui.interact(
                    gap_rect,
                    ui.id().with(("folded_idle_gap", seg_idx)),
                    eframe::egui::Sense::hover(),
                )
                .on_hover_text(format!(
                    "Folded idle gap: {full_dur}\nPress G or click 'Fold Gaps' to expand"
                ));
            }
        }
    }

    // 6. Render Stints/Events (top band) and Severity-Colored Fault Onset Ticks (bottom strip)
    let mut interacted_with_overlay = right_panning;
    let mut pending_single_click: Option<(String, i64, Option<i64>)> = None;
    let mut pending_shift_click: Option<(String, i64, Option<i64>)> = None;
    let mut pending_edit: Option<daqcore::storage::Annotation> = None;
    let mut pending_delete_id: Option<String> = None;
    let mut pending_jump_fault_ms: Option<i64> = None;

    let opaque_pill_fill = |c: eframe::egui::Color32, selected: bool| -> eframe::egui::Color32 {
        let mix = if selected { 0.72_f32 } else { 0.55_f32 };
        let bg = if theme.is_light {
            [30.0_f32, 41.0_f32, 59.0_f32]
        } else {
            [22.0_f32, 26.0_f32, 34.0_f32]
        };
        let r = (bg[0] * (1.0 - mix) + (c.r() as f32) * mix).round() as u8;
        let g = (bg[1] * (1.0 - mix) + (c.g() as f32) * mix).round() as u8;
        let b = (bg[2] * (1.0 - mix) + (c.b() as f32) * mix).round() as u8;
        eframe::egui::Color32::from_rgb(r, g, b)
    };

    let r_left = rect.left();
    let r_right = rect.right().max(r_left);

    if timeline_ui.show_stints {
        let pointer_pos = ui.input(|i| i.pointer.hover_pos().or_else(|| i.pointer.interact_pos()));
        let active_ctx_stint_id = ui.id().with("day_stint_ctx_idx");
        let saved_ctx_idx: Option<usize> = ui.data(|d| d.get_temp(active_ctx_stint_id));
        let mut hit_pill: Option<(usize, eframe::egui::Rect)> = None;
        let mut saved_ctx_rect: Option<eframe::egui::Rect> = None;

        for (stint_idx, ann) in ann_cache.stints.iter().enumerate() {
            let ann_end_ms = ann.end_utc_ms.unwrap_or(ann.start_utc_ms);
            let x0 = time_to_x(ann.start_utc_ms).clamp(r_left, r_right);
            let x1 = time_to_x(ann_end_ms).clamp(r_left, r_right);
            let is_point = ann.end_utc_ms.is_none() || ann_end_ms <= ann.start_utc_ms;
            let (pill_left, pill_right) = if is_point {
                ((x0 - 4.0).max(rect.left()), (x0 + 4.0).min(rect.right()))
            } else {
                (x0, x1.max(x0 + 5.0).min(rect.right()))
            };
            let pill_rect = eframe::egui::Rect::from_min_max(
                eframe::egui::pos2(pill_left, rect.top() + 2.0),
                eframe::egui::pos2(pill_right, rect.top() + 18.0),
            );

            let base_color = parse_hex_color(&ann.color, accent_color);
            let is_selected = selected_ann_ids.contains(&ann.id);
            let fill_color = opaque_pill_fill(base_color, is_selected);
            let stroke = if is_selected {
                eframe::egui::Stroke::new(2.0_f32, eframe::egui::Color32::WHITE)
            } else {
                eframe::egui::Stroke::new(1.0_f32, base_color)
            };

            painter.rect_filled(pill_rect, 3.0, fill_color);
            painter.rect_stroke(pill_rect, 3.0, stroke, eframe::egui::StrokeKind::Inside);

            if pill_rect.width() >= 36.0 {
                let text_clip = pill_rect
                    .shrink2(eframe::egui::vec2(3.0, 0.0))
                    .intersect(clip_rect);
                if text_clip.width() > 8.0 {
                    ui.painter_at(text_clip).text(
                        eframe::egui::pos2(pill_rect.left() + 4.0, pill_rect.center().y),
                        eframe::egui::Align2::LEFT_CENTER,
                        &ann.name,
                        eframe::egui::FontId::proportional(10.0),
                        eframe::egui::Color32::WHITE,
                    );
                }
            }

            if pointer_pos.is_some_and(|p| pill_rect.contains(p)) {
                hit_pill = Some((stint_idx, pill_rect));
            }
            if saved_ctx_idx == Some(stint_idx) {
                saved_ctx_rect = Some(pill_rect);
            }
        }

        // Only allocate egui interaction / tooltip / context menu for the single hovered pill
        // (and any pill with an open context menu) instead of all ~200+ pills every frame.
        let mut active_indices: [Option<(usize, eframe::egui::Rect)>; 2] = [hit_pill, None];
        if let (Some(ctx_idx), Some(ctx_rect)) = (saved_ctx_idx, saved_ctx_rect)
            && hit_pill.is_none_or(|(idx, _)| idx != ctx_idx)
        {
            active_indices[1] = Some((ctx_idx, ctx_rect));
        }

        let mut any_ctx_open = false;
        for (stint_idx, pill_rect) in active_indices.into_iter().flatten() {
            let Some(ann) = ann_cache.stints.get(stint_idx) else {
                continue;
            };
            let ann_end_ms = ann.end_utc_ms.unwrap_or(ann.start_utc_ms);
            let dur_s = (ann_end_ms - ann.start_utc_ms).max(0) as f64 / 1000.0;
            let hover_summary = format!(
                "{} [{}]\n{} .. {} ({dur_s:.1}s)\n{}\nClick: Snap viewport · Shift+Click: Compare · Double-Click / Right-Click: Edit",
                ann.name,
                ann.category,
                daqcore::time::Time::from_unix_millis(ann.start_utc_ms).label(),
                daqcore::time::Time::from_unix_millis(ann_end_ms).label(),
                if ann.notes.is_empty() {
                    "(no notes)"
                } else {
                    ann.notes.as_str()
                }
            );

            let pill_resp = ui
                .interact(
                    pill_rect,
                    ui.id().with(("day_stint_pill", stint_idx)),
                    eframe::egui::Sense::click(),
                )
                .on_hover_text(hover_summary);

            if pill_resp.double_clicked() {
                interacted_with_overlay = true;
                pending_edit = Some(ann.clone());
            } else if pill_resp.clicked() {
                interacted_with_overlay = true;
                let shift = ui.input(|i| i.modifiers.shift);
                if shift {
                    pending_shift_click = Some((ann.id.clone(), ann.start_utc_ms, ann.end_utc_ms));
                } else {
                    pending_single_click = Some((ann.id.clone(), ann.start_utc_ms, ann.end_utc_ms));
                }
            }

            if !right_panning {
                if pill_resp.secondary_clicked() {
                    ui.data_mut(|d| d.insert_temp(active_ctx_stint_id, stint_idx));
                }
                let menu_resp = pill_resp.context_menu(|ui| {
                    ui.label(eframe::egui::RichText::new(&ann.name).strong());
                    ui.separator();
                    if ui.button("Snap Viewport to Stint/Event").clicked() {
                        pending_single_click =
                            Some((ann.id.clone(), ann.start_utc_ms, ann.end_utc_ms));
                        ui.close();
                    }
                    if ui.button("Toggle Comparison Mode (Shift+Click)").clicked() {
                        pending_shift_click =
                            Some((ann.id.clone(), ann.start_utc_ms, ann.end_utc_ms));
                        ui.close();
                    }
                    if ui.button("Edit Event…").clicked() {
                        pending_edit = Some(ann.clone());
                        ui.close();
                    }
                    if ui.button("Delete Event").clicked() {
                        pending_delete_id = Some(ann.id.clone());
                        ui.close();
                    }
                });
                if menu_resp.is_some() {
                    any_ctx_open = true;
                    ui.data_mut(|d| d.insert_temp(active_ctx_stint_id, stint_idx));
                }
            }
        }
        if !any_ctx_open && saved_ctx_idx.is_some() {
            ui.data_mut(|d| d.remove::<usize>(active_ctx_stint_id));
        }
    }

    // Fault Onset Ticks along the bottom strip (`rect.bottom() - 10.0 .. rect.bottom() - 2.0`),
    // colored by FaultSeverity (`Warning` amber, `Error` orange, `Fatal` crimson) with highest severity drawn per 2px column!
    if timeline_ui.show_faults {
        let fault_y_top = rect.bottom() - 10.0;
        let fault_y_bot = rect.bottom() - 2.0;
        let fx_min = (r_left + 1.0).min(r_right);
        let fx_max = (r_right - 1.0).max(fx_min);
        for &(frac, sev) in &lane_cache.fault_ticks {
            let fx = (rect.left() + frac * rect.width()).clamp(fx_min, fx_max);
            let tick_color = theme.fault_severity_color(sev);
            painter.line_segment(
                [
                    eframe::egui::pos2(fx, fault_y_top),
                    eframe::egui::pos2(fx, fault_y_bot),
                ],
                eframe::egui::Stroke::new(2.0_f32, tick_color),
            );
        }
    }

    // 7. Draw Active Viewport `[start .. end]` with draggable Left/Right edge grips
    let vp_left = time_to_x(router.timeline().start_utc_ms()).clamp(r_left, r_right);
    let vp_right = time_to_x(router.timeline().end_utc_ms())
        .clamp(r_left, r_right)
        .max((vp_left + 4.0).min(r_right));
    let vp_rect = eframe::egui::Rect::from_min_max(
        eframe::egui::pos2(vp_left, rect.top() + 1.0),
        eframe::egui::pos2(vp_right, rect.bottom() - 1.0),
    );
    let vp_stroke_color = if theme.is_light {
        eframe::egui::Color32::from_rgb(15, 23, 42)
    } else {
        eframe::egui::Color32::WHITE
    };
    let vp_fill_color = if theme.is_light {
        eframe::egui::Color32::from_rgba_unmultiplied(2, 132, 199, 22)
    } else {
        eframe::egui::Color32::from_rgba_unmultiplied(255, 255, 255, 24)
    };
    painter.rect_filled(vp_rect, 2.0, vp_fill_color);
    painter.rect_stroke(
        vp_rect,
        2.0,
        eframe::egui::Stroke::new(1.5_f32, vp_stroke_color),
        eframe::egui::StrokeKind::Inside,
    );
    for edge_x in [vp_left, vp_right] {
        let grip_rect = eframe::egui::Rect::from_center_size(
            eframe::egui::pos2(edge_x, rect.center().y),
            eframe::egui::vec2(3.0, 14.0),
        );
        painter.rect_filled(grip_rect, 1.5, accent_color);
    }

    let left_grip_hit = eframe::egui::Rect::from_center_size(
        eframe::egui::pos2(vp_left, rect.center().y),
        eframe::egui::vec2(10.0, lane_height),
    );
    let right_grip_hit = eframe::egui::Rect::from_center_size(
        eframe::egui::pos2(vp_right, rect.center().y),
        eframe::egui::vec2(10.0, lane_height),
    );
    let left_grip_resp = ui
        .interact(
            left_grip_hit,
            ui.id().with("day_vp_left_grip"),
            eframe::egui::Sense::drag(),
        )
        .on_hover_cursor(eframe::egui::CursorIcon::ResizeHorizontal)
        .on_hover_text("Resize window start");
    let right_grip_resp = ui
        .interact(
            right_grip_hit,
            ui.id().with("day_vp_right_grip"),
            eframe::egui::Sense::drag(),
        )
        .on_hover_cursor(eframe::egui::CursorIcon::ResizeHorizontal)
        .on_hover_text("Resize window end");

    if left_grip_resp.dragged_by(eframe::egui::PointerButton::Primary)
        && let Some(pointer) = left_grip_resp.interact_pointer_pos()
    {
        interacted_with_overlay = true;
        let tl = router.timeline_mut();
        let new_start =
            x_to_time(pointer.x).min(tl.end_utc_ms() - daqcore::timeline::MIN_WINDOW_MS);
        tl.set_start(daqcore::time::Time::from_unix_millis(new_start));
        let _ = tl.set_window_secs(tl.end().secs(tl.start()));
    } else if right_grip_resp.dragged_by(eframe::egui::PointerButton::Primary)
        && let Some(pointer) = right_grip_resp.interact_pointer_pos()
    {
        interacted_with_overlay = true;
        let tl = router.timeline_mut();
        let new_end =
            x_to_time(pointer.x).max(tl.start_utc_ms() + daqcore::timeline::MIN_WINDOW_MS);
        tl.set_end(daqcore::time::Time::from_unix_millis(new_end));
        let _ = tl.set_window_secs(tl.end().secs(tl.start()));
    }

    let playhead_color = theme.playhead_color();
    let ph_x = time_to_x(router.timeline().setpoint_utc_ms()).clamp(r_left, r_right);
    painter.line_segment(
        [
            eframe::egui::pos2(ph_x, rect.top()),
            eframe::egui::pos2(ph_x, rect.bottom()),
        ],
        eframe::egui::Stroke::new(2.0_f32, playhead_color),
    );

    // Protruding top playhead handle stays draggable even at a window resize grip.
    let handle_painter = ui.painter_at(outer_rect.intersect(ui.clip_rect()));
    let handle_y = outer_rect.top() + 3.0;
    handle_painter.add(eframe::egui::Shape::convex_polygon(
        vec![
            eframe::egui::pos2(ph_x - 5.0, handle_y - 3.0),
            eframe::egui::pos2(ph_x + 5.0, handle_y - 3.0),
            eframe::egui::pos2(ph_x + 5.0, handle_y + 1.0),
            eframe::egui::pos2(ph_x, rect.top()),
            eframe::egui::pos2(ph_x - 5.0, handle_y + 1.0),
        ],
        playhead_color,
        eframe::egui::Stroke::NONE,
    ));
    let playhead_hit = eframe::egui::Rect::from_center_size(
        eframe::egui::pos2(ph_x, outer_rect.top() + 4.0),
        eframe::egui::vec2(14.0, 8.0),
    )
    .intersect(outer_rect);
    let playhead_response = ui
        .interact(
            playhead_hit,
            ui.id().with("day_playhead_handle"),
            eframe::egui::Sense::click_and_drag(),
        )
        .on_hover_cursor(eframe::egui::CursorIcon::ResizeHorizontal)
        .on_hover_text("Drag to scrub playhead");
    if (playhead_response.clicked()
        || playhead_response.dragged_by(eframe::egui::PointerButton::Primary))
        && let Some(pointer) = playhead_response.interact_pointer_pos()
    {
        interacted_with_overlay = true;
        let tl = router.timeline_mut();
        tl.set_playing(false);
        tl.set_setpoint(daqcore::time::Time::from_unix_millis(x_to_time(pointer.x)));
    }

    // Bottom window drag rail
    let window_rail = eframe::egui::Rect::from_min_max(
        eframe::egui::pos2(vp_left, rect.bottom() + 1.0),
        eframe::egui::pos2(vp_right, outer_rect.bottom()),
    );
    handle_painter.rect_filled(window_rail, 2.0, accent_color.gamma_multiply(0.6));
    let window_response = ui
        .interact(
            window_rail,
            ui.id().with("day_window_drag"),
            eframe::egui::Sense::drag(),
        )
        .on_hover_cursor(eframe::egui::CursorIcon::Grab)
        .on_hover_text("Drag to pan viewport window");
    let drag_offset_id = ui.id().with("day_window_drag_offset");
    if window_response.drag_started_by(eframe::egui::PointerButton::Primary)
        && let Some(pointer) = window_response.interact_pointer_pos()
    {
        let offset = x_to_time(pointer.x) - router.timeline().start_utc_ms();
        ui.data_mut(|data| data.insert_temp(drag_offset_id, offset));
    }
    if window_response.dragged_by(eframe::egui::PointerButton::Primary)
        && let Some(pointer) = window_response.interact_pointer_pos()
    {
        interacted_with_overlay = true;
        let offset = ui
            .data(|data| data.get_temp::<i64>(drag_offset_id))
            .unwrap_or(0);
        router
            .timeline_mut()
            .pan_to_start_ms(x_to_time(pointer.x) - offset);
    }
    if window_response.drag_stopped() {
        ui.data_mut(|data| data.remove::<i64>(drag_offset_id));
    }

    // Subtle lane badge at bottom-left
    painter.text(
        eframe::egui::pos2(rect.left() + 4.0, rect.bottom() - 2.0),
        eframe::egui::Align2::LEFT_BOTTOM,
        "TIMELINE",
        eframe::egui::FontId::monospace(8.5),
        theme.muted_text_color(),
    );

    if let Some((id, s, e)) = pending_single_click {
        router.timeline_mut().select_single_annotation(id, s, e);
    }
    if let Some((id, s, e)) = pending_shift_click {
        router.timeline_mut().toggle_comparison_annotation(id, s, e);
    }
    if let Some(ann) = pending_edit {
        timeline_ui.open_edit(&ann);
    }
    if let Some(del_id) = pending_delete_id {
        let now_ms = daqcore::time::Time::now().unix_millis();
        if let Err(err) = router.soft_delete_annotation(&del_id, now_ms) {
            *diagnostic = Some(format!("Failed to delete annotation: {err}"));
        }
    }

    painter.rect_stroke(
        rect,
        4.0,
        ui.visuals().widgets.noninteractive.bg_stroke,
        eframe::egui::StrokeKind::Inside,
    );

    // 8. Interactions on the Timeline background (fault hover tooltip, scrub inside window, pan outside window, scroll zoom)
    let hover_or_interact_pos = lane_response
        .hover_pos()
        .or_else(|| lane_response.interact_pointer_pos());
    let mut nearby_faults: Vec<&daqcore::storage::Annotation> = Vec::new();
    if let Some(pos) = hover_or_interact_pos
        && timeline_ui.show_faults
        && pos.y >= rect.bottom() - 12.0
    {
        nearby_faults = ann_cache
            .faults
            .iter()
            .filter(|a| (time_to_x(a.start_utc_ms) - pos.x).abs() <= 5.0)
            .collect();
    }

    if !nearby_faults.is_empty() {
        let first_ms = nearby_faults[0].start_utc_ms;
        let mut lines: Vec<String> = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for f in &nearby_faults {
            let short = f.fault_signal_id().unwrap_or(&f.name);
            if seen.insert(short) {
                let sev = f
                    .fault_severity()
                    .unwrap_or(daqcore::storage::FaultSeverity::Error);
                let lcd = f.fault_lcd_message().unwrap_or("");
                if lcd.is_empty() {
                    lines.push(format!("[{}] {short}", sev.badge_label()));
                } else {
                    lines.push(format!("[{}] {short} — {lcd}", sev.badge_label()));
                }
            }
        }
        let tip = format!(
            "⚠ {} fault transition(s) near {}:\n• {}\n(Click tick to jump · Right-click for all occurrences)",
            nearby_faults.len(),
            daqcore::time::Time::from_unix_millis(first_ms).label(),
            lines.into_iter().take(6).collect::<Vec<_>>().join("\n• "),
        );
        lane_response.clone().on_hover_text(tip);
        if !interacted_with_overlay && lane_response.clicked() {
            interacted_with_overlay = true;
            jump_to_fault_occurrence(router, first_ms);
        }
    }

    let scrub_origin_id = ui.id().with("day_scrub_origin");
    if lane_response.drag_started_by(eframe::egui::PointerButton::Primary) {
        let began_in_window = ui
            .input(|input| input.pointer.press_origin())
            .is_some_and(|pos| vp_rect.contains(pos));
        ui.data_mut(|data| data.insert_temp(scrub_origin_id, began_in_window));
    }
    if !interacted_with_overlay
        && (lane_response.dragged_by(eframe::egui::PointerButton::Primary)
            || lane_response.clicked())
        && let Some(pointer) = lane_response.interact_pointer_pos()
    {
        let target_ms = x_to_time(pointer.x);
        let scrubbing = if lane_response.dragged_by(eframe::egui::PointerButton::Primary) {
            ui.data(|data| data.get_temp::<bool>(scrub_origin_id))
                .unwrap_or(false)
        } else {
            vp_rect.contains(pointer)
        };
        if scrubbing {
            let tl = router.timeline_mut();
            tl.set_playing(false);
            tl.set_setpoint(daqcore::time::Time::from_unix_millis(target_ms));
        } else {
            let span_ms = router.timeline().window_ms().max(100);
            router
                .timeline_mut()
                .pan_to_start_ms(target_ms - span_ms / 2);
        }
    }
    if lane_response.drag_stopped() {
        ui.data_mut(|data| data.remove::<bool>(scrub_origin_id));
    }

    if lane_response.hovered() {
        let scroll_y = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll_y != 0.0
            && let Some(pointer) = ui.input(|i| i.pointer.hover_pos())
        {
            let anchor_ms = x_to_time(pointer.x);
            let factor = ui::scroll_zoom_factor(scroll_y);
            router.timeline_mut().zoom_around_ms(anchor_ms, factor);
        }
    }

    let hover_time_ms = lane_response
        .hover_pos()
        .map(|p| x_to_time(p.x))
        .unwrap_or_else(|| router.timeline().setpoint_utc_ms());

    let resizing = left_grip_resp.dragged_by(eframe::egui::PointerButton::Primary)
        || right_grip_resp.dragged_by(eframe::egui::PointerButton::Primary)
        || left_grip_resp.drag_stopped_by(eframe::egui::PointerButton::Primary)
        || right_grip_resp.drag_stopped_by(eframe::egui::PointerButton::Primary);
    if let Some(opacity) = timeline_ui.span_hint_opacity(ui.input(|input| input.time), resizing) {
        let timeline = router.timeline();
        let galley = painter.layout_no_wrap(
            format_span_ms(
                timeline
                    .end_utc_ms()
                    .saturating_sub(timeline.start_utc_ms()),
            ),
            eframe::egui::FontId::monospace(10.0),
            theme.text_color().gamma_multiply(opacity),
        );
        let size = eframe::egui::vec2(
            (galley.size().x + 12.0).min(rect.width()),
            galley.size().y + 4.0,
        );
        let min_center = rect.left() + size.x * 0.5;
        let max_center = (rect.right() - size.x * 0.5).max(min_center);
        let center_x = ((time_to_x(timeline.start_utc_ms()) + time_to_x(timeline.end_utc_ms()))
            * 0.5)
            .clamp(min_center, max_center);
        let badge = eframe::egui::Rect::from_center_size(
            eframe::egui::pos2(center_x, rect.center().y),
            size,
        );
        painter.rect_filled(badge, 3.0, theme.panel_color().gamma_multiply(opacity));
        painter.rect_stroke(
            badge,
            3.0,
            eframe::egui::Stroke::new(1.0_f32, accent_color.gamma_multiply(opacity)),
            eframe::egui::StrokeKind::Inside,
        );
        painter.galley(
            badge.center() - galley.size() * 0.5,
            galley,
            theme.text_color().gamma_multiply(opacity),
        );
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(33));
    }

    if right_panning {
        ui.ctx().set_cursor_icon(eframe::egui::CursorIcon::Grabbing);
    }
    if nearby_faults.is_empty() {
        lane_response.clone().on_hover_text(
            "Click inside window to scrub playhead · Click outside to move window · Right-drag to pan · Scroll to zoom",
        );
    }

    if !right_panning {
        let nearby_fault_sigs: Vec<String> = {
            let mut v = Vec::new();
            for f in &nearby_faults {
                let sig = f.fault_signal_id().unwrap_or(&f.name).to_owned();
                if !v.contains(&sig) {
                    v.push(sig);
                }
            }
            v
        };
        lane_response.context_menu(|ui| {
            if !nearby_fault_sigs.is_empty() {
                for sig_id in &nearby_fault_sigs {
                    if let Some(group) = ann_cache
                        .grouped_faults
                        .iter()
                        .find(|g| &g.signal_id == sig_id)
                    {
                        let sev_col = theme.fault_severity_color(group.severity);
                        ui.label(
                            eframe::egui::RichText::new(format!(
                                "[{}] {} ({}×)",
                                group.severity.badge_label(),
                                group.short_name,
                                group.occurrences.len()
                            ))
                            .strong()
                            .color(sev_col),
                        );
                        if !group.lcd_message.is_empty() {
                            ui.label(
                                eframe::egui::RichText::new(&group.lcd_message)
                                    .small()
                                    .weak(),
                            );
                        }
                        let first_ms = group.occurrences[0].0;
                        if ui
                            .button(format!(
                                "Jump to First Occurrence ({})",
                                daqcore::time::Time::from_unix_millis(first_ms).label()
                            ))
                            .clicked()
                        {
                            pending_jump_fault_ms = Some(first_ms);
                            ui.close();
                        }
                        ui.menu_button(
                            format!("All Occurrences ({})", group.occurrences.len()),
                            |ui| {
                                eframe::egui::ScrollArea::vertical().max_height(240.0).show(
                                    ui,
                                    |ui| {
                                        for (idx, &(occ_start_ms, occ_end_ms)) in
                                            group.occurrences.iter().enumerate()
                                        {
                                            let t_lbl =
                                                daqcore::time::Time::from_unix_millis(occ_start_ms)
                                                    .label();
                                            let dur_str = occ_end_ms
                                                .filter(|&e| e > occ_start_ms)
                                                .map(|e| {
                                                    format!(
                                                        " ({:.2}s)",
                                                        (e - occ_start_ms) as f64 / 1000.0
                                                    )
                                                })
                                                .unwrap_or_default();
                                            if ui
                                                .button(format!(
                                                    "#{} @ {}{}",
                                                    idx + 1,
                                                    t_lbl,
                                                    dur_str
                                                ))
                                                .clicked()
                                            {
                                                pending_jump_fault_ms = Some(occ_start_ms);
                                                ui.close();
                                            }
                                        }
                                    },
                                );
                            },
                        );
                        ui.horizontal(|ui| {
                            if ui.small_button("Open in State Lane").clicked() {
                                timeline_ui
                                    .pending_actions
                                    .push(action::AppAction::SpawnWidget(
                                        widget_constructor::WidgetConstructor::StateLaneSingle {
                                            signal_id: group.signal_id.clone(),
                                            label: group.short_name.clone(),
                                        },
                                    ));
                                ui.close();
                            }
                            if ui.small_button("Plot in YT Plot").clicked() {
                                timeline_ui
                                    .pending_actions
                                    .push(action::AppAction::SpawnWidget(
                                        widget_constructor::WidgetConstructor::ScopeChannel {
                                            signal_id: group.signal_id.clone(),
                                            label: group.short_name.clone(),
                                            unit: String::new(),
                                        },
                                    ));
                                ui.close();
                            }
                        });
                        ui.separator();
                    }
                }
            }
            if ui
                .button(format!(
                    "📍 Drop Bookmark at {}",
                    daqcore::time::Time::from_unix_millis(hover_time_ms).label()
                ))
                .clicked()
            {
                timeline_ui.open_new_at_timestamp(router, hover_time_ms);
                ui.close();
            }
            if ui
                .button("🏷 Create Interval Event from Active Viewport [start..end]")
                .clicked()
            {
                timeline_ui.open_new_at_playhead(router, false);
                ui.close();
            }
        });
    }

    if let Some(fault_ms) = pending_jump_fault_ms {
        jump_to_fault_occurrence(router, fault_ms);
    }
}

pub fn show_annotation_editor_modal(
    ctx: &eframe::egui::Context,
    router: &mut daqcore::router::VirtualTimelineRouter,
    timeline_ui: &mut TimelineUiState,
    diagnostic: &mut Option<String>,
) {
    let Some(mut draft) = timeline_ui.editor.clone() else {
        return;
    };

    let mut keep_open = true;
    let mut cancel_clicked = false;
    let mut save_clicked = false;
    let mut delete_clicked = false;

    let title = if draft.is_new {
        "Add Track Day Event Annotation"
    } else {
        "Edit Track Day Event Annotation"
    };

    eframe::egui::Window::new(title)
        .id(eframe::egui::Id::new("annotation_editor_modal"))
        .collapsible(false)
        .resizable(false)
        .open(&mut keep_open)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Event Name:");
                let name_resp = ui.text_edit_singleline(&mut draft.name);
                if name_resp.lost_focus() && ui.input(|i| i.key_pressed(eframe::egui::Key::Enter)) {
                    save_clicked = true;
                }
            });

            ui.horizontal(|ui| {
                ui.label("Category:");
                for (cat, default_color) in [
                    ("event", "#38BDF8"),
                    ("lap", "#22C55E"),
                    ("driving_stint", "#14B8A6"),
                    ("fault", "#EF4444"),
                    ("note", "#F59E0B"),
                ] {
                    if ui.selectable_label(draft.category == cat, cat).clicked() {
                        draft.category = cat.to_owned();
                        draft.color = default_color.to_owned();
                    }
                }
            });

            ui.horizontal(|ui| {
                ui.label("Color:");
                for preset_hex in [
                    "#38BDF8", "#22C55E", "#14B8A6", "#F59E0B", "#EF4444", "#A855F7",
                ] {
                    let c = parse_hex_color(preset_hex, eframe::egui::Color32::WHITE);
                    let btn = eframe::egui::Button::new(if draft.color == preset_hex {
                        "✔"
                    } else {
                        "  "
                    })
                    .fill(c);
                    if ui.add(btn).clicked() {
                        draft.color = preset_hex.to_owned();
                    }
                }
            });

            ui.checkbox(
                &mut draft.is_point_bookmark,
                "Point bookmark (single timestamp at Start)",
            );

            ui.horizontal(|ui| {
                ui.label("Start (UTC ms):");
                ui.add(eframe::egui::DragValue::new(&mut draft.start_utc_ms).speed(10.0));
                if ui.small_button("Use Playhead").clicked() {
                    draft.start_utc_ms = router.timeline().setpoint_utc_ms();
                    if draft.is_point_bookmark {
                        draft.end_utc_ms = draft.start_utc_ms;
                    }
                }
            });

            if !draft.is_point_bookmark {
                ui.horizontal(|ui| {
                    ui.label("End (UTC ms):  ");
                    ui.add(
                        eframe::egui::DragValue::new(&mut draft.end_utc_ms)
                            .range(draft.start_utc_ms..=i64::MAX)
                            .speed(10.0),
                    );
                    if ui.small_button("Use Viewport [start..end]").clicked() {
                        draft.start_utc_ms = router.timeline().start_utc_ms();
                        draft.end_utc_ms = router.timeline().end_utc_ms();
                    }
                });
            }

            ui.label("Notes:");
            ui.text_edit_multiline(&mut draft.notes);

            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Save Annotation").clicked() {
                    save_clicked = true;
                }
                if !draft.is_new && ui.button("Delete").clicked() {
                    delete_clicked = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel_clicked = true;
                }
            });
        });

    let now_ms = daqcore::time::Time::now().unix_millis();
    if save_clicked {
        let ann = draft.into_annotation(now_ms);
        if let Err(err) = router.upsert_annotation(&ann) {
            *diagnostic = Some(format!("Failed to save annotation: {err}"));
        }
        timeline_ui.editor = None;
    } else if delete_clicked {
        if let Err(err) = router.soft_delete_annotation(&draft.id, now_ms) {
            *diagnostic = Some(format!("Failed to delete annotation: {err}"));
        }
        timeline_ui.editor = None;
    } else if !keep_open || cancel_clicked {
        timeline_ui.editor = None;
    } else {
        timeline_ui.editor = Some(draft);
    }
}

/// Optional floating window wrapper for the Sessions & Faults Navigator.
pub fn show_chunk_navigator_window(
    ctx: &eframe::egui::Context,
    router: &mut daqcore::router::VirtualTimelineRouter,
    timeline_ui: &mut TimelineUiState,
    diagnostic: &mut Option<String>,
) {
    if !timeline_ui.show_chunk_navigator {
        return;
    }
    let mut open = timeline_ui.show_chunk_navigator;
    eframe::egui::Window::new("📅 Sessions, Runs & Faults")
        .open(&mut open)
        .default_width(320.0)
        .default_height(460.0)
        .resizable(true)
        .collapsible(true)
        .show(ctx, |ui| {
            show_chunk_navigator_contents(ui, router, timeline_ui, diagnostic, 380.0);
        });
    timeline_ui.show_chunk_navigator = open;
}

/// Focused Track Day Sessions, Runs & Faults navigator (designed to fit cleanly inside the narrow
/// `215px` left sidebar with zero horizontal overflow or button soup).
pub fn show_chunk_navigator_contents(
    ui: &mut eframe::egui::Ui,
    router: &mut daqcore::router::VirtualTimelineRouter,
    timeline_ui: &mut TimelineUiState,
    diagnostic: &mut Option<String>,
    max_scroll_height: f32,
) {
    let nav_max_w = ui.available_width().max(60.0);
    ui.set_max_width(nav_max_w);
    ui.style_mut().wrap_mode = Some(eframe::egui::TextWrapMode::Truncate);
    ui.spacing_mut().item_spacing = eframe::egui::vec2(4.0, 3.0);

    let theme = ui::theme::get_theme(ui.ctx());
    let accent_color = theme.accent_color();
    let muted_color = theme.muted_text_color();
    let border_color = theme.border_color();

    let track_days = router.list_track_days().unwrap_or_default();
    let all_partitions: Vec<(String, String, i64, i64, usize)> = router
        .partitions()
        .iter()
        .map(|p| {
            (
                p.track_day_id.clone(),
                p.partition_id.clone(),
                p.start_utc_ms,
                p.end_utc_ms,
                p.manifest.signals.len(),
            )
        })
        .collect();
    let all_annotations = router.all_annotations_cached().unwrap_or_default();

    let mut day_ids_set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for td in &track_days {
        day_ids_set.insert(td.id.clone());
    }
    for (day_id, _, _, _, _) in &all_partitions {
        day_ids_set.insert(day_id.clone());
    }
    for ann in all_annotations.iter() {
        day_ids_set.insert(ann.track_day_id.clone());
    }
    let day_ids: Vec<String> = day_ids_set.into_iter().collect();

    if day_ids.is_empty() {
        ui.label(
            eframe::egui::RichText::new("No recorded sessions or annotations loaded yet.")
                .italics()
                .weak(),
        );
        return;
    }

    let active_day_id = router.timeline().active_track_day_id().map(str::to_owned);

    let mut req_select_day: Option<Option<String>> = None;
    let mut req_set_day_label: Option<(String, String)> = None;

    // -------------------------------------------------------------------------
    // 1. Compact Active Track Day Selector Bar (`[◀] 2026-09-06 · KMS ▾ [▶] [↔] [✏]`)
    // -------------------------------------------------------------------------
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let cur_idx = active_day_id
            .as_ref()
            .and_then(|id| day_ids.iter().position(|d| d == id));

        let can_prev = cur_idx.map_or(!day_ids.is_empty(), |i| i > 0);
        if ui
            .add_enabled(can_prev, eframe::egui::Button::new("‹").small())
            .on_hover_text("Previous Track Day")
            .clicked()
        {
            if let Some(i) = cur_idx {
                if i > 0 {
                    req_select_day = Some(Some(day_ids[i - 1].clone()));
                }
            } else if let Some(last_day) = day_ids.last() {
                req_select_day = Some(Some(last_day.clone()));
            }
        }

        let active_meta = active_day_id
            .as_ref()
            .and_then(|id| track_days.iter().find(|td| &td.id == id));
        let active_loc = active_meta.map(|td| td.location.as_str()).unwrap_or("");
        let selected_label = match (&active_day_id, active_loc.is_empty()) {
            (Some(id), false) => format!("{id} · {active_loc}"),
            (Some(id), true) => id.clone(),
            (None, _) => "All Track Days".to_owned(),
        };

        let trailing_reserve = if active_day_id.is_some() { 56.0 } else { 36.0 };
        let combo_w = (ui.available_width() - trailing_reserve).max(75.0);
        eframe::egui::ComboBox::from_id_salt("sidebar_sessions_day_combo")
            .width(combo_w)
            .selected_text(
                eframe::egui::RichText::new(selected_label)
                    .size(11.0)
                    .strong(),
            )
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(active_day_id.is_none(), "All Track Days")
                    .clicked()
                {
                    req_select_day = Some(None);
                }
                for d_id in day_ids.iter().rev() {
                    let meta = track_days.iter().find(|td| &td.id == d_id);
                    let loc = meta.map(|td| td.location.as_str()).unwrap_or("");
                    let lbl = if loc.is_empty() {
                        d_id.clone()
                    } else {
                        format!("{d_id} — {loc}")
                    };
                    if ui
                        .selectable_label(active_day_id.as_ref() == Some(d_id), lbl)
                        .clicked()
                    {
                        req_select_day = Some(Some(d_id.clone()));
                    }
                }
            });

        let can_next = cur_idx.map_or(!day_ids.is_empty(), |i| i + 1 < day_ids.len());
        if ui
            .add_enabled(can_next, eframe::egui::Button::new("›").small())
            .on_hover_text("Next Track Day")
            .clicked()
        {
            if let Some(i) = cur_idx {
                if i + 1 < day_ids.len() {
                    req_select_day = Some(Some(day_ids[i + 1].clone()));
                }
            } else if let Some(first_day) = day_ids.first() {
                req_select_day = Some(Some(first_day.clone()));
            }
        }

        if let Some(d_id) = &active_day_id {
            if ui
                .small_button("↔")
                .on_hover_text("Fit entire Track Day (F)")
                .clicked()
            {
                req_select_day = Some(Some(d_id.clone()));
            }
            if ui
                .small_button("✏")
                .on_hover_text("Edit Track Day location / label")
                .clicked()
            {
                if timeline_ui.editing_track_day.is_some() {
                    timeline_ui.editing_track_day = None;
                } else {
                    timeline_ui.editing_track_day = Some((d_id.clone(), active_loc.to_owned()));
                }
            }
        } else if ui
            .small_button("↔")
            .on_hover_text("Fit all Track Days (F)")
            .clicked()
        {
            req_select_day = Some(None);
        }
    });

    if let Some((edit_id, draft)) = &mut timeline_ui.editing_track_day {
        let mut commit_edit = false;
        let mut cancel_edit = false;
        ui.horizontal(|ui| {
            let edit_w = (ui.available_width() - 52.0).max(50.0);
            let resp = ui.add(
                eframe::egui::TextEdit::singleline(draft)
                    .id_salt(("track_day_label_input", edit_id.as_str()))
                    .desired_width(edit_w)
                    .hint_text("Location / event label…"),
            );
            if resp.lost_focus() && ui.input(|i| i.key_pressed(eframe::egui::Key::Enter)) {
                commit_edit = true;
            }
            if resp.has_focus() && ui.input(|i| i.key_pressed(eframe::egui::Key::Escape)) {
                cancel_edit = true;
            }
            if ui.small_button("✔").on_hover_text("Save (Enter)").clicked() {
                commit_edit = true;
            }
            if ui.small_button("×").on_hover_text("Cancel (Esc)").clicked() {
                cancel_edit = true;
            }
        });
        if commit_edit {
            req_set_day_label = Some((edit_id.clone(), draft.trim().to_owned()));
            timeline_ui.editing_track_day = None;
        } else if cancel_edit {
            timeline_ui.editing_track_day = None;
        }
    }

    // -------------------------------------------------------------------------
    // 2. Search Input (`/`)
    // -------------------------------------------------------------------------
    ui.allocate_ui_with_layout(
        eframe::egui::vec2(nav_max_w, ui.spacing().interact_size.y),
        eframe::egui::Layout::right_to_left(eframe::egui::Align::Center),
        |ui| {
            ui.set_max_width(nav_max_w);
            if ui
                .add_enabled(
                    !timeline_ui.chunk_search_query.is_empty(),
                    eframe::egui::Button::new("×").small(),
                )
                .on_hover_text("Clear search query")
                .clicked()
            {
                timeline_ui.chunk_search_query.clear();
            }
            let edit_w = (ui.available_width() - 6.0).max(40.0);
            let resp = ui.add(
                eframe::egui::TextEdit::singleline(&mut timeline_ui.chunk_search_query)
                    .id(eframe::egui::Id::new("chunk_navigator_search_input"))
                    .desired_width(edit_w)
                    .hint_text("Filter runs, faults, chunks… (/)"),
            );
            if resp.has_focus() && ui.input(|i| i.key_pressed(eframe::egui::Key::Escape)) {
                resp.surrender_focus();
            }
        },
    );

    let query = timeline_ui.chunk_search_query.trim().to_lowercase();
    let matches = |text: &str| -> bool { query.is_empty() || text.to_lowercase().contains(&query) };
    let filter_day = router.timeline().active_track_day_id().map(str::to_owned);

    let mut day_stints: Vec<&daqcore::storage::Annotation> = all_annotations
        .iter()
        .filter(|a| {
            filter_day.as_deref().is_none_or(|d| a.track_day_id == d)
                && a.category != "fault"
                && matches(&format!(
                    "{} {} {} {}",
                    a.track_day_id, a.name, a.category, a.notes
                ))
        })
        .collect();
    day_stints.sort_by_key(|a| (a.start_utc_ms, a.end_utc_ms.unwrap_or(a.start_utc_ms)));

    let day_fault_anns: Vec<daqcore::storage::Annotation> = all_annotations
        .iter()
        .filter(|a| {
            filter_day.as_deref().is_none_or(|d| a.track_day_id == d)
                && a.category == "fault"
                && matches(&format!("{} {} {}", a.track_day_id, a.name, a.notes))
        })
        .cloned()
        .collect();
    let grouped_faults = group_fault_annotations(&day_fault_anns);

    let mut day_parts: Vec<&(String, String, i64, i64, usize)> = all_partitions
        .iter()
        .filter(|(d, p_id, s, e, _)| {
            filter_day.as_deref().is_none_or(|day| d == day)
                && matches(&format!(
                    "{d} {p_id} {} {}",
                    daqcore::time::Time::from_unix_millis(*s).label(),
                    daqcore::time::Time::from_unix_millis(*e).label()
                ))
        })
        .collect();
    day_parts.sort_by_key(|(_, _, s, _, _)| *s);

    // -------------------------------------------------------------------------
    // 3. Segmented Mode Selector (`Runs (N)` | `Faults (N)` | `Chunks (N)`)
    // -------------------------------------------------------------------------
    let active_mode = if timeline_ui.chunk_filter == ChunkFilter::All {
        ChunkFilter::StintsAndEvents
    } else {
        timeline_ui.chunk_filter
    };
    let strong_text = ui.visuals().strong_text_color();
    let extreme_bg = ui.visuals().extreme_bg_color;
    ui.columns(3, |cols| {
        for (col_idx, (mode, label)) in [
            (
                ChunkFilter::StintsAndEvents,
                format!("Runs ({})", day_stints.len()),
            ),
            (
                ChunkFilter::Faults,
                format!("Faults ({})", grouped_faults.len()),
            ),
            (
                ChunkFilter::Partitions,
                format!("Chunks ({})", day_parts.len()),
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let is_sel = active_mode == mode;
            let btn = eframe::egui::Button::new(
                eframe::egui::RichText::new(label)
                    .size(10.5)
                    .strong()
                    .color(if is_sel { strong_text } else { muted_color }),
            )
            .fill(if is_sel {
                accent_color.gamma_multiply(0.20)
            } else {
                extreme_bg
            })
            .stroke(eframe::egui::Stroke::new(
                1.0_f32,
                if is_sel { accent_color } else { border_color },
            ))
            .corner_radius(3.0);
            if cols[col_idx]
                .add_sized([cols[col_idx].available_width(), 20.0], btn)
                .clicked()
            {
                timeline_ui.chunk_filter = mode;
            }
        }
    });

    if active_mode == ChunkFilter::Faults {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            for (sev_opt, lbl) in [
                (None, "All"),
                (Some(daqcore::storage::FaultSeverity::Fatal), "Fatal"),
                (Some(daqcore::storage::FaultSeverity::Error), "Error"),
                (Some(daqcore::storage::FaultSeverity::Warning), "Warn"),
            ] {
                let is_sel = timeline_ui.fault_severity_filter == sev_opt;
                let col = sev_opt
                    .map(|s| theme.fault_severity_color(s))
                    .unwrap_or(accent_color);
                let chip = eframe::egui::Button::new(
                    eframe::egui::RichText::new(lbl)
                        .size(10.0)
                        .color(if is_sel { col } else { muted_color }),
                )
                .fill(if is_sel {
                    col.gamma_multiply(0.16)
                } else {
                    eframe::egui::Color32::TRANSPARENT
                })
                .stroke(eframe::egui::Stroke::new(
                    1.0_f32,
                    if is_sel {
                        col.gamma_multiply(0.65)
                    } else {
                        border_color
                    },
                ))
                .corner_radius(3.0);
                if ui.add(chip).clicked() {
                    timeline_ui.fault_severity_filter = sev_opt;
                }
            }
        });
    }

    ui.separator();

    let cur_win_start = router.timeline().start_utc_ms();
    let cur_win_end = router.timeline().end_utc_ms();
    let selected_ann_ids: Vec<String> = router.timeline().selected_annotation_ids().to_vec();

    let mut req_snap_range: Option<(String, i64, i64)> = None;
    let mut req_single_ann: Option<(String, String, i64, Option<i64>)> = None;
    let mut req_compare_ann: Option<(String, i64, Option<i64>)> = None;
    let mut req_edit_ann: Option<daqcore::storage::Annotation> = None;
    let mut req_delete_ann_id: Option<String> = None;
    let mut req_jump_fault: Option<(String, i64)> = None;

    eframe::egui::ScrollArea::vertical()
        .id_salt(("chunk_navigator_scroll", max_scroll_height as i32))
        .max_height(max_scroll_height)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            let inner_w = (nav_max_w - 6.0).max(60.0);
            ui.set_max_width(inner_w);
            ui.style_mut().wrap_mode = Some(eframe::egui::TextWrapMode::Truncate);

            match active_mode {
                ChunkFilter::StintsAndEvents | ChunkFilter::All => {
                    if day_stints.is_empty() {
                        ui.label(
                            eframe::egui::RichText::new("No runs or events match filter.")
                                .italics()
                                .weak()
                                .small(),
                        );
                    }
                    for ann in &day_stints {
                        let ann_end = ann.end_utc_ms.unwrap_or(ann.start_utc_ms);
                        let dur_s = (ann_end - ann.start_utc_ms).max(0) as f64 / 1000.0;
                        let start_short = chrono::DateTime::from_timestamp_millis(ann.start_utc_ms)
                            .map(|dt| dt.format("%H:%M:%S").to_string())
                            .unwrap_or_else(|| {
                                daqcore::time::Time::from_unix_millis(ann.start_utc_ms).label()
                            });
                        let is_selected = selected_ann_ids.contains(&ann.id);
                        let is_active = is_selected
                            || (filter_day.is_some()
                                && (cur_win_start - ann.start_utc_ms).abs() <= 1_000
                                && (cur_win_end - ann_end).abs() <= 1_000);
                        let bar_color = parse_hex_color(&ann.color, accent_color);
                        let time_meta = if dur_s > 0.0 {
                            format!("{start_short} · {dur_s:.0}s")
                        } else {
                            start_short.clone()
                        };
                        let right_meta = if filter_day.is_none() && !ann.track_day_id.is_empty() {
                            format!("{} · {time_meta}", ann.track_day_id)
                        } else {
                            time_meta
                        };

                        let (row_rect, resp) = ui.allocate_exact_size(
                            eframe::egui::vec2(inner_w, 22.0),
                            eframe::egui::Sense::click(),
                        );
                        if ui.is_rect_visible(row_rect) {
                            let painter = ui.painter_at(row_rect);
                            if is_active || resp.hovered() {
                                painter.rect_filled(
                                    row_rect,
                                    3.0,
                                    if is_active {
                                        bar_color.gamma_multiply(0.18)
                                    } else {
                                        theme.border_color().gamma_multiply(0.45)
                                    },
                                );
                            }
                            if is_selected {
                                painter.rect_stroke(
                                    row_rect,
                                    3.0,
                                    eframe::egui::Stroke::new(1.0_f32, bar_color),
                                    eframe::egui::StrokeKind::Inside,
                                );
                            }
                            // Left color indicator bar
                            let indicator = eframe::egui::Rect::from_min_size(
                                eframe::egui::pos2(row_rect.left() + 2.0, row_rect.top() + 3.0),
                                eframe::egui::vec2(3.0, row_rect.height() - 6.0),
                            );
                            painter.rect_filled(indicator, 1.5, bar_color);

                            // Right metadata
                            let meta_galley = painter.layout_no_wrap(
                                right_meta,
                                eframe::egui::FontId::monospace(9.5),
                                muted_color,
                            );
                            let meta_x =
                                (row_rect.right() - meta_galley.size().x - 4.0).max(row_rect.left() + 48.0);
                            painter.galley(
                                eframe::egui::pos2(
                                    meta_x,
                                    row_rect.center().y - meta_galley.size().y * 0.5,
                                ),
                                meta_galley,
                                muted_color,
                            );

                            // Left truncated title
                            let title_clip = eframe::egui::Rect::from_min_max(
                                eframe::egui::pos2(row_rect.left() + 9.0, row_rect.top()),
                                eframe::egui::pos2((meta_x - 4.0).max(row_rect.left() + 12.0), row_rect.bottom()),
                            )
                            .intersect(ui.clip_rect());
                            ui.painter_at(title_clip).text(
                                eframe::egui::pos2(row_rect.left() + 9.0, row_rect.center().y),
                                eframe::egui::Align2::LEFT_CENTER,
                                &ann.name,
                                eframe::egui::FontId::proportional(11.0),
                                ui.visuals().text_color(),
                            );
                        }

                        let hover_txt = format!(
                            "{} [{}]\nStart: {}\nDuration: {dur_s:.1}s\n{}\nClick: Snap · Shift+Click: Compare · Right-Click: Options",
                            ann.name,
                            ann.category,
                            daqcore::time::Time::from_unix_millis(ann.start_utc_ms).label(),
                            if ann.notes.is_empty() {
                                "(no notes)"
                            } else {
                                ann.notes.as_str()
                            }
                        );
                        let resp = resp
                            .on_hover_cursor(eframe::egui::CursorIcon::PointingHand)
                            .on_hover_text(hover_txt);

                        if resp.double_clicked() {
                            req_edit_ann = Some((*ann).clone());
                        } else if resp.clicked() {
                            if ui.input(|i| i.modifiers.shift) {
                                req_compare_ann =
                                    Some((ann.id.clone(), ann.start_utc_ms, ann.end_utc_ms));
                            } else {
                                req_single_ann = Some((
                                    ann.track_day_id.clone(),
                                    ann.id.clone(),
                                    ann.start_utc_ms,
                                    ann.end_utc_ms,
                                ));
                            }
                        }

                        resp.context_menu(|ui| {
                            ui.label(eframe::egui::RichText::new(&ann.name).strong());
                            ui.separator();
                            if ui.button("Snap Viewport to Run/Event").clicked() {
                                req_single_ann = Some((
                                    ann.track_day_id.clone(),
                                    ann.id.clone(),
                                    ann.start_utc_ms,
                                    ann.end_utc_ms,
                                ));
                                ui.close();
                            }
                            if ui
                                .button(if is_selected {
                                    "Remove from Comparison"
                                } else {
                                    "Compare with Active Run (Shift+Click)"
                                })
                                .clicked()
                            {
                                req_compare_ann =
                                    Some((ann.id.clone(), ann.start_utc_ms, ann.end_utc_ms));
                                ui.close();
                            }
                            if ui.button("Edit Run / Event…").clicked() {
                                req_edit_ann = Some((*ann).clone());
                                ui.close();
                            }
                            if ui.button("Delete Run / Event").clicked() {
                                req_delete_ann_id = Some(ann.id.clone());
                                ui.close();
                            }
                        });
                    }
                }
                ChunkFilter::Faults => {
                    let filtered_groups: Vec<&GroupedFault<'_>> = grouped_faults
                        .iter()
                        .filter(|g| {
                            timeline_ui
                                .fault_severity_filter
                                .is_none_or(|sev| g.severity == sev)
                        })
                        .collect();

                    if filtered_groups.is_empty() {
                        ui.label(
                            eframe::egui::RichText::new("No faults match severity filter.")
                                .italics()
                                .weak()
                                .small(),
                        );
                    }

                    let playhead_ms = router.timeline().setpoint_utc_ms();

                    for group in filtered_groups {
                        let sev_col = theme.fault_severity_color(group.severity);
                        let is_expanded = timeline_ui.expanded_fault_ids.contains(&group.signal_id);
                        let (row_rect, resp) = ui.allocate_exact_size(
                            eframe::egui::vec2(inner_w, 24.0),
                            eframe::egui::Sense::click(),
                        );
                        if ui.is_rect_visible(row_rect) {
                            let painter = ui.painter_at(row_rect);
                            if is_expanded || resp.hovered() {
                                painter.rect_filled(
                                    row_rect,
                                    3.0,
                                    sev_col.gamma_multiply(if is_expanded { 0.16 } else { 0.10 }),
                                );
                            }
                            // Left severity badge
                            let badge_txt = match group.severity {
                                daqcore::storage::FaultSeverity::Fatal => "FATAL",
                                daqcore::storage::FaultSeverity::Error => "ERR",
                                daqcore::storage::FaultSeverity::Warning => "WARN",
                            };
                            let badge_rect = eframe::egui::Rect::from_min_size(
                                eframe::egui::pos2(row_rect.left() + 2.0, row_rect.top() + 4.0),
                                eframe::egui::vec2(34.0, 16.0),
                            );
                            painter.rect_filled(badge_rect, 2.5, sev_col.gamma_multiply(0.22));
                            painter.rect_stroke(
                                badge_rect,
                                2.5,
                                eframe::egui::Stroke::new(1.0_f32, sev_col.gamma_multiply(0.65)),
                                eframe::egui::StrokeKind::Inside,
                            );
                            painter.text(
                                badge_rect.center(),
                                eframe::egui::Align2::CENTER_CENTER,
                                badge_txt,
                                eframe::egui::FontId::monospace(8.5),
                                sev_col,
                            );

                            // Right count pill (`N×`)
                            let count_txt = format!(
                                "{}× {}",
                                group.occurrences.len(),
                                if is_expanded { "−" } else { "+" }
                            );
                            let count_galley = painter.layout_no_wrap(
                                count_txt,
                                eframe::egui::FontId::monospace(9.5),
                                muted_color,
                            );
                            let count_x = (row_rect.right() - count_galley.size().x - 4.0)
                                .max(badge_rect.right() + 24.0);
                            painter.galley(
                                eframe::egui::pos2(
                                    count_x,
                                    row_rect.center().y - count_galley.size().y * 0.5,
                                ),
                                count_galley,
                                muted_color,
                            );

                            // Short fault name
                            let name_clip = eframe::egui::Rect::from_min_max(
                                eframe::egui::pos2(badge_rect.right() + 5.0, row_rect.top()),
                                eframe::egui::pos2(
                                    (count_x - 4.0).max(badge_rect.right() + 8.0),
                                    row_rect.bottom(),
                                ),
                            )
                            .intersect(ui.clip_rect());
                            ui.painter_at(name_clip).text(
                                eframe::egui::pos2(badge_rect.right() + 5.0, row_rect.center().y),
                                eframe::egui::Align2::LEFT_CENTER,
                                &group.short_name,
                                eframe::egui::FontId::proportional(11.0),
                                ui.visuals().text_color(),
                            );
                        }

                        let first_occ = group.occurrences[0];
                        let tip = if group.lcd_message.is_empty() {
                            format!(
                                "{} [{}]\nOccurrences: {}\nClick to expand all occurrences · Right-click for actions",
                                group.signal_id,
                                group.severity.display_name(),
                                group.occurrences.len()
                            )
                        } else {
                            format!(
                                "{} [{}]\nLCD: {}\nOccurrences: {}\nClick to expand all occurrences · Right-click for actions",
                                group.signal_id,
                                group.severity.display_name(),
                                group.lcd_message,
                                group.occurrences.len()
                            )
                        };
                        let resp = resp
                            .on_hover_cursor(eframe::egui::CursorIcon::PointingHand)
                            .on_hover_text(tip);

                        if resp.clicked() {
                            if is_expanded {
                                timeline_ui.expanded_fault_ids.remove(&group.signal_id);
                            } else {
                                timeline_ui
                                    .expanded_fault_ids
                                    .insert(group.signal_id.clone());
                                if group.occurrences.len() == 1 {
                                    req_jump_fault = Some((
                                        first_occ.track_day_id.clone(),
                                        first_occ.start_utc_ms,
                                    ));
                                }
                            }
                        }

                        resp.context_menu(|ui| {
                            ui.label(
                                eframe::egui::RichText::new(format!(
                                    "[{}] {}",
                                    group.severity.badge_label(),
                                    group.signal_id
                                ))
                                .strong()
                                .color(sev_col),
                            );
                            if !group.lcd_message.is_empty() {
                                ui.label(
                                    eframe::egui::RichText::new(&group.lcd_message)
                                        .small()
                                        .weak(),
                                );
                            }
                            ui.separator();
                            if ui
                                .button(format!(
                                    "Jump to First Occurrence ({})",
                                    daqcore::time::Time::from_unix_millis(first_occ.start_utc_ms)
                                        .label()
                                ))
                                .clicked()
                            {
                                req_jump_fault =
                                    Some((first_occ.track_day_id.clone(), first_occ.start_utc_ms));
                                ui.close();
                            }
                            let next_occ = group
                                .occurrences
                                .iter()
                                .find(|o| o.start_utc_ms > playhead_ms + 50)
                                .unwrap_or(&first_occ);
                            if ui
                                .button(format!(
                                    "Jump to Next after Playhead ({})",
                                    daqcore::time::Time::from_unix_millis(next_occ.start_utc_ms)
                                        .label()
                                ))
                                .clicked()
                            {
                                req_jump_fault =
                                    Some((next_occ.track_day_id.clone(), next_occ.start_utc_ms));
                                ui.close();
                            }
                            ui.menu_button(
                                format!("All Occurrences ({})", group.occurrences.len()),
                                |ui| {
                                    eframe::egui::ScrollArea::vertical()
                                        .max_height(240.0)
                                        .show(ui, |ui| {
                                            for (idx, occ) in group.occurrences.iter().enumerate() {
                                                let t_lbl = daqcore::time::Time::from_unix_millis(
                                                    occ.start_utc_ms,
                                                )
                                                .label();
                                                let dur_str = occ
                                                    .end_utc_ms
                                                    .filter(|&e| e > occ.start_utc_ms)
                                                    .map(|e| {
                                                        format!(
                                                            " ({:.2}s)",
                                                            (e - occ.start_utc_ms) as f64 / 1000.0
                                                        )
                                                    })
                                                    .unwrap_or_default();
                                                if ui
                                                    .button(format!(
                                                        "#{} @ {}{}",
                                                        idx + 1,
                                                        t_lbl,
                                                        dur_str
                                                    ))
                                                    .clicked()
                                                {
                                                    req_jump_fault = Some((
                                                        occ.track_day_id.clone(),
                                                        occ.start_utc_ms,
                                                    ));
                                                    ui.close();
                                                }
                                            }
                                        });
                                },
                            );
                            ui.separator();
                            if ui.button("Open in State Lane").clicked() {
                                timeline_ui.pending_actions.push(
                                    action::AppAction::SpawnWidget(
                                        widget_constructor::WidgetConstructor::StateLaneSingle {
                                            signal_id: group.signal_id.clone(),
                                            label: group.short_name.clone(),
                                        },
                                    ),
                                );
                                ui.close();
                            }
                            if ui.button("Plot in YT Plot").clicked() {
                                timeline_ui.pending_actions.push(
                                    action::AppAction::SpawnWidget(
                                        widget_constructor::WidgetConstructor::ScopeChannel {
                                            signal_id: group.signal_id.clone(),
                                            label: group.short_name.clone(),
                                            unit: String::new(),
                                        },
                                    ),
                                );
                                ui.close();
                            }
                        });

                        if is_expanded {
                            if !group.lcd_message.is_empty() {
                                ui.indent(("fault_lcd_indent", &group.signal_id), |ui| {
                                    ui.label(
                                        eframe::egui::RichText::new(&group.lcd_message)
                                            .size(10.0)
                                            .italics()
                                            .color(muted_color),
                                    );
                                });
                            }
                            ui.indent(("fault_occ_indent", &group.signal_id), |ui| {
                                for (idx, occ) in group.occurrences.iter().take(50).enumerate() {
                                    let t_lbl =
                                        daqcore::time::Time::from_unix_millis(occ.start_utc_ms)
                                            .label();
                                    let dur_str = occ
                                        .end_utc_ms
                                        .filter(|&e| e > occ.start_utc_ms)
                                        .map(|e| {
                                            format!(
                                                " · {:.2}s",
                                                (e - occ.start_utc_ms) as f64 / 1000.0
                                            )
                                        })
                                        .unwrap_or_default();
                                    let day_prefix = if filter_day.is_none() && !occ.track_day_id.is_empty() {
                                        format!("{} · ", occ.track_day_id)
                                    } else {
                                        String::new()
                                    };
                                    let is_near_playhead = filter_day.is_some()
                                        && (playhead_ms - occ.start_utc_ms).abs() <= 500;
                                    if ui
                                        .selectable_label(
                                            is_near_playhead,
                                            eframe::egui::RichText::new(format!(
                                                "#{}  {}{}{}",
                                                idx + 1,
                                                day_prefix,
                                                t_lbl,
                                                dur_str
                                            ))
                                            .monospace()
                                            .size(10.0),
                                        )
                                        .on_hover_text("Click to jump playhead & window to this occurrence")
                                        .clicked()
                                    {
                                        req_jump_fault = Some((
                                            occ.track_day_id.clone(),
                                            occ.start_utc_ms,
                                        ));
                                    }
                                }
                                if group.occurrences.len() > 50 {
                                    ui.label(
                                        eframe::egui::RichText::new(format!(
                                            "…and {} more (use right-click menu)",
                                            group.occurrences.len() - 50
                                        ))
                                        .size(9.5)
                                        .weak(),
                                    );
                                }
                            });
                        }
                    }
                }
                ChunkFilter::Partitions => {
                    if day_parts.is_empty() {
                        ui.label(
                            eframe::egui::RichText::new("No recorded SD chunks match filter.")
                                .italics()
                                .weak()
                                .small(),
                        );
                    }
                    for (d_id, part_id, p_start, p_end, sig_count) in &day_parts {
                        let dur_s = (*p_end - *p_start).max(0) as f64 / 1000.0;
                        let start_lbl = chrono::DateTime::from_timestamp_millis(*p_start)
                            .map(|dt| dt.format("%H:%M:%S").to_string())
                            .unwrap_or_else(|| {
                                daqcore::time::Time::from_unix_millis(*p_start).label()
                            });
                        let end_lbl = chrono::DateTime::from_timestamp_millis(*p_end)
                            .map(|dt| dt.format("%H:%M:%S").to_string())
                            .unwrap_or_else(|| {
                                daqcore::time::Time::from_unix_millis(*p_end).label()
                            });
                        let is_active = filter_day.is_some()
                            && (cur_win_start - *p_start).abs() <= 1_000
                            && (cur_win_end - *p_end).abs() <= 1_000;
                        let lbl = if filter_day.is_none() {
                            format!("{d_id}  {start_lbl}–{end_lbl}  ({dur_s:.0}s · {sig_count} sigs)")
                        } else {
                            format!("{start_lbl}–{end_lbl}  ({dur_s:.0}s · {sig_count} sigs)")
                        };
                        if ui
                            .selectable_label(
                                is_active,
                                eframe::egui::RichText::new(lbl).monospace().size(10.5),
                            )
                            .on_hover_text(format!(
                                "Partition {part_id} ({d_id})\nClick to snap viewport to this recorded SD chunk"
                            ))
                            .clicked()
                        {
                            req_snap_range = Some(((*d_id).clone(), *p_start, *p_end));
                        }
                    }
                }
            }
        });

    timeline_ui.chunk_days_open_override = None;

    if let Some((day_id, new_label)) = req_set_day_label
        && let Err(err) = router.set_track_day_label(&day_id, &new_label)
    {
        *diagnostic = Some(format!("Failed to update track day label: {err}"));
    }
    if let Some(day_opt) = req_select_day {
        let is_some_day = day_opt.is_some();
        if let Err(err) = router.select_track_day(day_opt) {
            *diagnostic = Some(format!("Failed to select track day: {err}"));
        } else if is_some_day && let Some((lo, hi)) = router.timeline().day_bounds_ms() {
            router.timeline_mut().snap_to_range_ms(lo, hi);
            timeline_ui.reset_ruler();
        }
    }
    if let Some((day_id, lo, hi)) = req_snap_range {
        if router.timeline().active_track_day_id() != Some(day_id.as_str()) {
            let _ = router.select_track_day(Some(day_id));
        }
        router.timeline_mut().snap_to_range_ms(lo, hi);
    }
    if let Some((day_id, ann_id, s, e)) = req_single_ann {
        if router.timeline().active_track_day_id() != Some(day_id.as_str()) {
            let _ = router.select_track_day(Some(day_id));
        }
        router.timeline_mut().select_single_annotation(ann_id, s, e);
    }
    if let Some((ann_id, s, e)) = req_compare_ann {
        router
            .timeline_mut()
            .toggle_comparison_annotation(ann_id, s, e);
    }
    if let Some(ann) = req_edit_ann {
        timeline_ui.open_edit(&ann);
    }
    if let Some(del_id) = req_delete_ann_id {
        let now_ms = daqcore::time::Time::now().unix_millis();
        if let Err(err) = router.soft_delete_annotation(&del_id, now_ms) {
            *diagnostic = Some(format!("Failed to delete annotation: {err}"));
        }
    }
    if let Some((day_id, fault_ms)) = req_jump_fault {
        if router.timeline().active_track_day_id() != Some(day_id.as_str()) {
            let _ = router.select_track_day(Some(day_id));
        }
        jump_to_fault_occurrence(router, fault_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_span_hint_lingers_then_fades_and_expires() {
        let mut state = TimelineUiState::default();
        assert_eq!(state.span_hint_opacity(0.0, false), None);
        assert_eq!(state.span_hint_opacity(10.0, true), Some(1.0));
        assert_eq!(state.span_hint_opacity(100.0, true), Some(1.0));
        assert_eq!(state.span_hint_opacity(100.0, false), Some(1.0));
        assert_eq!(state.span_hint_opacity(102.4, false), Some(1.0));
        assert_eq!(state.span_hint_opacity(102.75, false), Some(0.5));
        assert_eq!(state.span_hint_opacity(103.0, false), None);
        assert_eq!(state.span_hint_at, None);
        assert_eq!(state.span_hint_opacity(104.0, true), Some(1.0));
        state.reset_ruler();
        assert_eq!(state.span_hint_opacity(104.0, false), None);
    }

    #[test]
    fn resize_span_label_is_precise_and_short() {
        for (span, expected) in [
            (0, "Δt (0 ms)"),
            (100, "Δt (100 ms)"),
            (1_000, "Δt (1 s)"),
            (1_234, "Δt (1 s 234 ms)"),
            (90_001, "Δt (90 s 1 ms)"),
        ] {
            let label = format_span_ms(span);
            assert_eq!(label, expected);
        }
    }

    #[test]
    fn unified_lane_separates_playhead_window_drag_and_resize() {
        use eframe::egui::{Event, Modifiers, PointerButton, Pos2, Rect, Vec2};

        fn frame(
            ctx: &eframe::egui::Context,
            router: &mut daqcore::router::VirtualTimelineRouter,
            events: Vec<Event>,
        ) -> Rect {
            let mut bounds = Rect::NOTHING;
            let state_id = eframe::egui::Id::new("test_timeline_state");
            let mut timeline_ui = ctx
                .data_mut(|data| data.get_temp::<TimelineUiState>(state_id))
                .unwrap_or_default();
            let _ = ctx.run(
                eframe::egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 200.0))),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    eframe::egui::CentralPanel::default().show(ctx, |ui| {
                        bounds = Rect::from_min_size(
                            ui.next_widget_position(),
                            Vec2::new(ui.available_width(), 43.0),
                        );
                        show_day_overview_lane(
                            ui,
                            router,
                            &mut timeline_ui,
                            &mut None,
                            eframe::egui::Color32::LIGHT_BLUE,
                        );
                    });
                },
            );
            ctx.data_mut(|data| data.insert_temp(state_id, timeline_ui));
            bounds
        }

        fn drag(
            ctx: &eframe::egui::Context,
            router: &mut daqcore::router::VirtualTimelineRouter,
            from: Pos2,
            to: Pos2,
        ) {
            drag_button(ctx, router, from, to, PointerButton::Primary);
        }

        fn drag_button(
            ctx: &eframe::egui::Context,
            router: &mut daqcore::router::VirtualTimelineRouter,
            from: Pos2,
            to: Pos2,
            button: PointerButton,
        ) {
            frame(ctx, router, vec![Event::PointerMoved(from)]);
            frame(
                ctx,
                router,
                vec![Event::PointerButton {
                    pos: from,
                    button,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                }],
            );
            frame(ctx, router, vec![Event::PointerMoved(to)]);
            frame(
                ctx,
                router,
                vec![Event::PointerButton {
                    pos: to,
                    button,
                    pressed: false,
                    modifiers: Modifiers::NONE,
                }],
            );
        }

        let ctx = eframe::egui::Context::default();
        ui::theme::store_theme(&ctx, ui::theme::ThemeSelection::default().get_colors());
        let mut router = daqcore::router::VirtualTimelineRouter::live(
            daqcore::time::Time::from_unix_millis(100_000),
            40.0,
        )
        .unwrap();
        router.timeline_mut().set_day_bounds_ms(Some((0, 100_000)));
        router.timeline_mut().snap_to_range_ms(20_000, 60_000);
        let bounds = frame(&ctx, &mut router, vec![]);
        let x = |fraction: f32| bounds.left() + bounds.width() * fraction;

        // The top handle scrubs even when the playhead is at a resize edge.
        drag(
            &ctx,
            &mut router,
            Pos2::new(x(0.2), bounds.top() + 3.0),
            Pos2::new(x(0.4), bounds.top() + 3.0),
        );
        assert!((router.timeline().setpoint_utc_ms() - 40_000).abs() < 200);
        assert_eq!(router.timeline().start_utc_ms(), 20_000);
        assert_eq!(router.timeline().end_utc_ms(), 60_000);

        // Scrubbing the window body clamps at the edge instead of moving the window.
        drag(
            &ctx,
            &mut router,
            Pos2::new(x(0.4), bounds.top() + 22.0),
            Pos2::new(x(0.9), bounds.top() + 22.0),
        );
        assert_eq!(router.timeline().setpoint_utc_ms(), 60_000);
        assert_eq!(router.timeline().start_utc_ms(), 20_000);
        assert_eq!(router.timeline().end_utc_ms(), 60_000);

        // The bottom grip translates the window without changing its duration.
        drag(
            &ctx,
            &mut router,
            Pos2::new(x(0.4), bounds.bottom() - 3.0),
            Pos2::new(x(0.5), bounds.bottom() - 3.0),
        );
        assert!((router.timeline().start_utc_ms() - 30_000).abs() < 200);
        assert_eq!(router.timeline().window_ms(), 40_000);

        let state_id = eframe::egui::Id::new("test_timeline_state");
        assert!(
            ctx.data(|data| data.get_temp::<TimelineUiState>(state_id))
                .unwrap()
                .span_hint_at
                .is_none(),
            "Scrubbing and panning must not show a resize hint"
        );

        // The side grip resizes independently of the playhead handle.
        let right_fraction = router.timeline().end_utc_ms() as f32 / 100_000.0;
        drag(
            &ctx,
            &mut router,
            Pos2::new(x(right_fraction), bounds.top() + 22.0),
            Pos2::new(x(0.8), bounds.top() + 22.0),
        );
        assert!((router.timeline().end_utc_ms() - 80_000).abs() < 200);
        assert!((router.timeline().start_utc_ms() - 30_000).abs() < 200);

        assert!(
            ctx.data(|data| data.get_temp::<TimelineUiState>(state_id))
                .unwrap()
                .span_hint_at
                .is_some(),
            "Resizing shows the temporary duration badge"
        );

        // Right dragging pans from the background or over either kind of handle.
        let before = (
            router.timeline().start_utc_ms(),
            router.timeline().end_utc_ms(),
        );
        drag_button(
            &ctx,
            &mut router,
            Pos2::new(x(0.5), bounds.top() + 22.0),
            Pos2::new(x(0.4), bounds.top() + 22.0),
            PointerButton::Secondary,
        );
        assert!(
            (router.timeline().start_utc_ms() - (before.0 - 10_000)).abs() < 200,
            "Right drag moved {:?} to {:?}",
            before,
            (
                router.timeline().start_utc_ms(),
                router.timeline().end_utc_ms()
            )
        );
    }

    #[test]
    fn master_timeline_two_lanes_gap_folding_and_annotation_modal_render_cleanly() {
        let mut router = daqcore::router::VirtualTimelineRouter::live(
            daqcore::time::Time::from_unix_millis(1_000_000),
            30.0,
        )
        .expect("live router");
        router
            .set_track_day_label("2026-09-06", "Purdue Grand Prix Track")
            .expect("set track day label");

        let ann1 = daqcore::storage::Annotation {
            id: "stint_1".to_owned(),
            track_day_id: "2026-09-06".to_owned(),
            name: "Driving Stint #1".to_owned(),
            category: "driving_stint".to_owned(),
            color: "#14B8A6".to_owned(),
            start_utc_ms: 1_000_000,
            end_utc_ms: Some(1_300_000),
            author: "auto".to_owned(),
            source: "auto".to_owned(),
            notes: "Warmup laps".to_owned(),
            updated_at_ms: 1_300_000,
            deleted: false,
        };
        let ann2 = daqcore::storage::Annotation {
            id: "stint_2".to_owned(),
            track_day_id: "2026-09-06".to_owned(),
            name: "Driving Stint #2".to_owned(),
            category: "driving_stint".to_owned(),
            color: "#22C55E".to_owned(),
            start_utc_ms: 8_500_000,
            end_utc_ms: Some(8_900_000),
            author: "auto".to_owned(),
            source: "auto".to_owned(),
            notes: "Endurance run".to_owned(),
            updated_at_ms: 8_900_000,
            deleted: false,
        };
        let fault_ann = daqcore::storage::Annotation {
            id: "auto_fault_a_box_fault_sync.IMD_1100000".to_owned(),
            track_day_id: "2026-09-06".to_owned(),
            name: "Fault: a_box_fault_sync.IMD".to_owned(),
            category: "fault".to_owned(),
            color: "#f97316".to_owned(),
            start_utc_ms: 1_100_000,
            end_utc_ms: Some(1_105_000),
            author: "daqcore".to_owned(),
            source: "auto".to_owned(),
            notes:
                "[ERROR] Isolation fault — Auto-detected fault transition on a_box_fault_sync.IMD"
                    .to_owned(),
            updated_at_ms: 1_105_000,
            deleted: false,
        };
        router.upsert_annotation(&ann1).expect("upsert ann1");
        router.upsert_annotation(&ann2).expect("upsert ann2");
        router.upsert_annotation(&fault_ann).expect("upsert fault");

        router
            .timeline_mut()
            .set_day_bounds_ms(Some((1_000_000, 9_000_000)));
        router.timeline_mut().snap_to_range_ms(1_000_000, 1_300_000);
        router.timeline_mut().select_single_annotation(
            "stint_1".to_owned(),
            1_000_000,
            Some(1_300_000),
        );
        router.timeline_mut().toggle_comparison_annotation(
            "stint_2".to_owned(),
            8_500_000,
            Some(8_900_000),
        );
        assert!(router.timeline().is_comparison_mode());

        let mut timeline_ui = TimelineUiState {
            show_chunk_navigator: true,
            ..Default::default()
        };
        timeline_ui.open_new_at_playhead(&router, true);
        assert!(timeline_ui.editor.is_some());

        let ctx = eframe::egui::Context::default();
        ui::theme::store_theme(&ctx, ui::theme::ThemeSelection::default().get_colors());

        for size in [
            eframe::egui::vec2(1280.0, 400.0),
            eframe::egui::vec2(640.0, 320.0),
            eframe::egui::vec2(320.0, 260.0),
        ] {
            let screen_rect = eframe::egui::Rect::from_min_size(eframe::egui::Pos2::ZERO, size);
            let mut raw_input = eframe::egui::RawInput {
                screen_rect: Some(screen_rect),
                ..Default::default()
            };
            raw_input.viewport_id = eframe::egui::ViewportId::ROOT;
            let mut bus_load = Vec::new();
            let mut diagnostic = None;

            let _ = ctx.run(raw_input, |ctx| {
                eframe::egui::TopBottomPanel::top("test_shared_timeline").show(ctx, |ui| {
                    show_timeline_contents(
                        ui,
                        &mut router,
                        &mut timeline_ui,
                        &mut bus_load,
                        &mut diagnostic,
                    );
                });
                show_annotation_editor_modal(ctx, &mut router, &mut timeline_ui, &mut diagnostic);
                show_chunk_navigator_window(ctx, &mut router, &mut timeline_ui, &mut diagnostic);
            });
            assert!(
                diagnostic.is_none(),
                "unexpected timeline diagnostic: {diagnostic:?}"
            );
        }

        // Verify editing an auto-generated driving stint via AnnotationDraft persists the rename
        let existing_stint = router
            .query_annotations_in_range(1_000_000..=9_000_000)
            .expect("query annotations")
            .into_iter()
            .find(|a| a.id == "stint_1")
            .expect("stint_1 exists");
        assert_eq!(existing_stint.source, "auto");

        timeline_ui.open_edit(&existing_stint);
        let mut draft = timeline_ui.editor.take().expect("draft opened");
        draft.name = "Autocross Run 1 (Dry)".to_owned();
        let updated_ann = draft.into_annotation(1_000_500);
        assert_eq!(updated_ann.source, "manual");
        assert!(
            router
                .upsert_annotation(&updated_ann)
                .expect("upsert edited stint"),
            "editing an auto-generated driving stint must update the registry row"
        );

        let refreshed_stint = router
            .query_annotations_in_range(1_000_000..=9_000_000)
            .expect("query annotations after edit")
            .into_iter()
            .find(|a| a.id == "stint_1")
            .expect("stint_1 still exists");
        assert_eq!(refreshed_stint.name, "Autocross Run 1 (Dry)");
        assert_eq!(refreshed_stint.source, "manual");
    }

    #[test]
    fn format_window_label_formats_presets_and_custom_spans() {
        assert_eq!(format_window_label(30_000), "30s");
        assert_eq!(format_window_label(60_000), "1min");
        assert_eq!(format_window_label(300_000), "5min");
        assert_eq!(format_window_label(600_000), "10min");

        assert_eq!(format_window_label(500), "500ms");
        assert_eq!(format_window_label(15_000), "15s");
        assert_eq!(format_window_label(12_500), "12.5s");
        assert_eq!(format_window_label(120_000), "2min");
        assert_eq!(format_window_label(150_000), "2m30s");
        assert_eq!(format_window_label(3_600_000), "1h");
        assert_eq!(format_window_label(4_500_000), "1h15m");
    }
}
