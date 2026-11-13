use crate::{app, telemetry, ui};

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
        }
    }

    pub fn from_existing(ann: &daqcore::storage::Annotation) -> Self {
        let is_point = ann.end_utc_ms.is_none_or(|e| e <= ann.start_utc_ms);
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
            author: ann.author.clone(),
            source: ann.source.clone(),
        }
    }

    pub fn into_annotation(self, updated_at_ms: i64) -> daqcore::storage::Annotation {
        let start = self.start_utc_ms;
        let end_utc_ms = if self.is_point_bookmark {
            None
        } else {
            Some(self.end_utc_ms.max(start))
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
            author: self.author,
            source: self.source,
            notes: self.notes,
            updated_at_ms,
            deleted: false,
        }
    }
}

/// Persistent UI state for the Master Timeline Bar and its Annotation Editor modal.
#[derive(Debug, Default, Clone)]
pub struct TimelineUiState {
    pub editor: Option<AnnotationDraft>,
}

impl TimelineUiState {
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
                format!("🕒 {}", p.badge_label()),
                color,
                "Canonical SD Partition clock correlation provenance at Playhead",
            )
        }
        Some(p @ daqcore::router::ClockProvenance::LiveProvisional) => (
            format!("📡 {}", p.badge_label()),
            accent_color,
            "Provisional Live UDP / CAN telemetry stream",
        ),
        None => (
            "🕒 No Telemetry at Playhead".to_owned(),
            eframe::egui::Color32::from_gray(130),
            "Playhead is currently over an unlogged interval",
        ),
    }
}

pub fn show(app: &mut app::DAQApp, ctx: &eframe::egui::Context) {
    let dt_secs = ctx.input(|i| i.stable_dt as f64).clamp(0.0, 0.25);
    if app.router.timeline().is_playing() {
        app.router.timeline_mut().advance_playback(dt_secs);
        ctx.request_repaint();
    }

    let mut span_changed = false;
    eframe::egui::TopBottomPanel::top("shared_timeline").show(ctx, |ui| {
        span_changed = show_timeline_contents(
            ui,
            &mut app.router,
            &mut app.timeline_ui,
            &mut app.bus_load_samples,
            &mut app.diagnostic,
        );
    });

    show_annotation_editor_modal(
        ctx,
        &mut app.router,
        &mut app.timeline_ui,
        &mut app.diagnostic,
    );

    if span_changed {
        app.save_settings();
    }
}

/// Render the two-lane Master Timeline Bar inside `ui`. Returns `true` if `window_secs` changed.
pub fn show_timeline_contents(
    ui: &mut eframe::egui::Ui,
    router: &mut daqcore::router::VirtualTimelineRouter,
    timeline_ui: &mut TimelineUiState,
    bus_load_samples: &mut Vec<telemetry::BusLoadSample>,
    diagnostic: &mut Option<String>,
) -> bool {
    let theme = ui::theme::get_theme(ui.ctx());
    let accent_color =
        parse_hex_color(&theme.accent, eframe::egui::Color32::from_rgb(56, 189, 248));
    let previous_span = router.timeline().window_secs();

    // -------------------------------------------------------------------------
    // Row 1: Transport, Track Day Selector, Gap Folding, Clock Provenance Badge
    // -------------------------------------------------------------------------
    let track_days = router.list_track_days().unwrap_or_default();
    let active_day_id = router.timeline().active_track_day_id().map(str::to_owned);
    let mut requested_day_change: Option<Option<String>> = None;
    let mut clear_live_clicked = false;
    let mut open_new_bookmark = false;
    let mut open_new_interval = false;

    ui.horizontal_wrapped(|ui| {
        // Track Day ComboBox
        if !track_days.is_empty() {
            let selected_text = active_day_id.as_deref().unwrap_or("All Track Days");
            eframe::egui::ComboBox::from_id_salt("timeline_track_day_selector")
                .selected_text(format!("📅 {selected_text}"))
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(active_day_id.is_none(), "All Track Days")
                        .clicked()
                    {
                        requested_day_change = Some(None);
                    }
                    for td in &track_days {
                        let label = if td.location.is_empty() {
                            td.id.clone()
                        } else {
                            format!("{} ({})", td.id, td.location)
                        };
                        if ui
                            .selectable_label(
                                active_day_id.as_deref() == Some(td.id.as_str()),
                                label,
                            )
                            .clicked()
                        {
                            requested_day_change = Some(Some(td.id.clone()));
                        }
                    }
                });
        }

        let timeline = router.timeline_mut();

        // LIVE Follow toggle
        let is_live = timeline.is_live();
        let live_btn = eframe::egui::Button::new(
            eframe::egui::RichText::new(if is_live { "🔴 LIVE" } else { "⚪ LIVE" }).color(
                if is_live {
                    eframe::egui::Color32::WHITE
                } else {
                    ui.visuals().text_color()
                },
            ),
        )
        .fill(if is_live {
            eframe::egui::Color32::from_rgb(220, 38, 38)
        } else {
            ui.visuals().widgets.inactive.bg_fill
        });
        if ui
            .add(live_btn)
            .on_hover_text("Follow incoming live telemetry (click to toggle)")
            .clicked()
        {
            if is_live {
                timeline.set_start(timeline.start());
                timeline.set_end(timeline.end());
                timeline.set_setpoint(timeline.setpoint());
            } else {
                timeline.go_live();
            }
        }

        if let Some((day_min, day_max)) = timeline.day_bounds_ms()
            && ui
                .button("↔ Fit Day")
                .on_hover_text("Zoom out to the full Track Day overview")
                .clicked()
        {
            timeline.snap_to_range_ms(day_min, day_max);
        }

        ui.separator();

        // Play / Pause & Playback Speeds + Frame Stepping
        let is_playing = timeline.is_playing();
        if ui
            .button(if is_playing { "⏸ Pause" } else { "▶ Play" })
            .on_hover_text("Play / pause playhead scrub")
            .clicked()
        {
            timeline.toggle_playing();
        }

        let current_speed = timeline.playback_speed();
        eframe::egui::ComboBox::from_id_salt("timeline_playback_speed")
            .width(58.0)
            .selected_text(format!("{current_speed}x"))
            .show_ui(ui, |ui| {
                for &preset in &daqcore::timeline::PLAYBACK_SPEED_PRESETS {
                    if ui
                        .selectable_label(
                            (current_speed - preset).abs() < 1e-4,
                            format!("{preset}x"),
                        )
                        .clicked()
                    {
                        timeline.set_playback_speed(preset);
                    }
                }
            });

        if ui
            .small_button("⏮ -100ms")
            .on_hover_text("Step playhead backward 100 ms")
            .clicked()
        {
            timeline.step_by_ms(-100);
        }
        if ui
            .small_button("⏪ -10ms")
            .on_hover_text("Step playhead backward 10 ms")
            .clicked()
        {
            timeline.step_by_ms(-10);
        }
        if ui
            .small_button("⏩ +10ms")
            .on_hover_text("Step playhead forward 10 ms")
            .clicked()
        {
            timeline.step_by_ms(10);
        }
        if ui
            .small_button("⏭ +100ms")
            .on_hover_text("Step playhead forward 100 ms")
            .clicked()
        {
            timeline.step_by_ms(100);
        }

        ui.separator();

        // Active window span & zoom buttons
        let mut span = timeline.window_secs();
        ui.label("Window:");
        let span_control = eframe::egui::DragValue::new(&mut span)
            .range(0.1..=86_400.0)
            .speed(0.5)
            .suffix(" s");
        if ui.add(span_control).changed() {
            let center_ms = timeline.setpoint_utc_ms();
            let half_ms = ((span * 500.0).round() as i64).max(50);
            timeline.set_start(daqcore::time::Time::from_unix_millis(center_ms - half_ms));
            timeline.set_end(daqcore::time::Time::from_unix_millis(center_ms + half_ms));
            if let Err(error) = timeline.set_window_secs(span) {
                *diagnostic = Some(error);
            }
        }
        if ui
            .small_button("🔍+")
            .on_hover_text("Zoom in 2x around playhead")
            .clicked()
        {
            timeline.zoom_around_ms(timeline.setpoint_utc_ms(), 0.5);
        }
        if ui
            .small_button("🔍-")
            .on_hover_text("Zoom out 2x around playhead")
            .clicked()
        {
            timeline.zoom_around_ms(timeline.setpoint_utc_ms(), 2.0);
        }

        ui.separator();

        // Dead-Time Gap Folding toggle
        let mut fold_gaps = timeline.collapse_idle_gaps();
        if ui
            .checkbox(&mut fold_gaps, "Fold Idle Gaps")
            .on_hover_text(
                "Collapse unlogged intervals (>= 60s) into thin dividers so active stints fill the horizontal ribbon",
            )
            .changed()
        {
            timeline.set_collapse_idle_gaps(fold_gaps);
        }

        // Add Event / Bookmark buttons
        if ui
            .button("📍 + Bookmark (M)")
            .on_hover_text("Drop a point event bookmark at the current Playhead (Hotkey: M)")
            .clicked()
        {
            open_new_bookmark = true;
        }
        if ui
            .button("🏷 + Interval Event")
            .on_hover_text("Create a named interval annotation spanning [start .. end]")
            .clicked()
        {
            open_new_interval = true;
        }

        if ui
            .small_button("Clear Live")
            .on_hover_text("Clear in-memory live ring buffer")
            .clicked()
        {
            clear_live_clicked = true;
        }
    });

    if let Some(day_choice) = requested_day_change
        && let Err(err) = router.select_track_day(day_choice)
    {
        *diagnostic = Some(format!("Failed to switch track day: {err}"));
    }
    if clear_live_clicked {
        router.reset_live(daqcore::time::Time::now());
        bus_load_samples.clear();
    }
    if open_new_bookmark {
        timeline_ui.open_new_at_playhead(router, true);
    }
    if open_new_interval {
        timeline_ui.open_new_at_playhead(router, false);
    }

    // -------------------------------------------------------------------------
    // Top Lane: Day Overview, 1Hz Activity Envelope & Event Annotation Ribbon
    // -------------------------------------------------------------------------
    show_day_overview_lane(ui, router, timeline_ui, diagnostic, accent_color);

    ui.add_space(2.0);

    // -------------------------------------------------------------------------
    // Bottom Lane: Active Window Scrub & Playhead Controls
    // -------------------------------------------------------------------------
    show_active_window_scrub_lane(ui, router, accent_color);

    // -------------------------------------------------------------------------
    // Footer Status & Badges Row (Clock Provenance, LOD Tier, Comparison Mode)
    // -------------------------------------------------------------------------
    let setpoint_ms = router.timeline().setpoint_utc_ms();
    let provenance = router.clock_provenance_at(setpoint_ms);
    let (prov_text, prov_color, prov_hover) =
        provenance_badge_style(provenance.as_ref(), accent_color);
    let active_res = match router
        .timeline()
        .resolution_for_width(ui.available_width().max(1.0))
    {
        daqcore::router::Resolution::Overview1Hz => "1Hz Overview",
        daqcore::router::Resolution::Overview10Hz => "10Hz Overview",
        daqcore::router::Resolution::FullRes => "Full-Res",
    };
    let tile_cache_mib = router.tile_cache().lru_bytes() as f64 / (1024.0 * 1024.0);
    let partition_count = router.partitions().len();
    let live_frame_count = router.live_buffer().len();

    let selected_ids: Vec<String> = router.timeline().selected_annotation_ids().to_vec();
    let is_comparing = router.timeline().is_comparison_mode();
    let mut clear_comparison = false;

    ui.horizontal_wrapped(|ui| {
        // Clock Provenance Badge
        eframe::egui::Frame::NONE
            .fill(prov_color.gamma_multiply(0.18))
            .stroke(eframe::egui::Stroke::new(1.0_f32, prov_color))
            .corner_radius(4.0)
            .inner_margin(eframe::egui::Margin::symmetric(6, 2))
            .show(ui, |ui| {
                ui.label(
                    eframe::egui::RichText::new(&prov_text)
                        .color(prov_color)
                        .small()
                        .strong(),
                )
                .on_hover_text(prov_hover);
            });

        // LOD Tier Badge
        eframe::egui::Frame::NONE
            .fill(ui.visuals().widgets.noninteractive.bg_fill)
            .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
            .corner_radius(4.0)
            .inner_margin(eframe::egui::Margin::symmetric(6, 2))
            .show(ui, |ui| {
                ui.label(
                    eframe::egui::RichText::new(format!(
                        "LOD: {active_res} · Cache: {tile_cache_mib:.1} MiB · {partition_count} SD Partitions · {live_frame_count} Live Frames"
                    ))
                    .small(),
                );
            });

        ui.label(
            eframe::egui::RichText::new(format!(
                "Viewport: {} .. {} ({:.2}s) · Playhead: {}",
                router.timeline().start().label(),
                router.timeline().end().label(),
                router.timeline().window_secs(),
                router.timeline().setpoint().label(),
            ))
            .monospace()
            .small(),
        );

        if is_comparing {
            let names: Vec<String> = router
                .query_annotations_in_range(i64::MIN / 2..=i64::MAX / 2)
                .unwrap_or_default()
                .into_iter()
                .filter(|a| selected_ids.contains(&a.id))
                .map(|a| a.name)
                .collect();
            let compare_label = if names.is_empty() {
                format!("⚖ Comparing {} Events", selected_ids.len())
            } else {
                format!("⚖ Comparing: {}", names.join(" vs "))
            };
            let compare_color = eframe::egui::Color32::from_rgb(168, 85, 247);
            eframe::egui::Frame::NONE
                .fill(compare_color.gamma_multiply(0.2))
                .stroke(eframe::egui::Stroke::new(1.0_f32, compare_color))
                .corner_radius(4.0)
                .inner_margin(eframe::egui::Margin::symmetric(6, 2))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            eframe::egui::RichText::new(compare_label)
                                .color(compare_color)
                                .small()
                                .strong(),
                        );
                        if ui.small_button("✕").on_hover_text("Exit Comparison Mode").clicked() {
                            clear_comparison = true;
                        }
                    });
                });
        }
    });

    if clear_comparison {
        router.timeline_mut().clear_selected_annotations();
    }

    if let Some(error) = diagnostic {
        ui.colored_label(ui.visuals().error_fg_color, error);
    }

    previous_span != router.timeline().window_secs()
}

fn show_day_overview_lane(
    ui: &mut eframe::egui::Ui,
    router: &mut daqcore::router::VirtualTimelineRouter,
    timeline_ui: &mut TimelineUiState,
    diagnostic: &mut Option<String>,
    accent_color: eframe::egui::Color32,
) {
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

    let collapse_idle_gaps = router.timeline().collapse_idle_gaps();
    let projection = daqcore::timeline::GapFoldedProjection::new(
        day_min_ms,
        day_max_ms,
        &active_spans_for_folding,
        collapse_idle_gaps,
    );

    let activity_1hz = router.activity_envelope_1hz(day_min_ms..=day_max_ms);
    let annotations: Vec<daqcore::storage::Annotation> = router
        .query_annotations_in_range(day_min_ms..=day_max_ms)
        .unwrap_or_default()
        .into_iter()
        .filter(|a| {
            active_day_filter
                .as_deref()
                .is_none_or(|day_id| a.track_day_id == day_id)
        })
        .collect();
    let selected_ann_ids: Vec<String> = router.timeline().selected_annotation_ids().to_vec();

    let lane_height = 54.0;
    let width = ui.available_width().max(1.0);
    let (rect, lane_response) = ui.allocate_exact_size(
        eframe::egui::vec2(width, lane_height),
        eframe::egui::Sense::click_and_drag(),
    );
    let clip_rect = rect.intersect(ui.clip_rect());
    let painter = ui.painter_at(clip_rect);

    // 1. Background (Idle Gap dark gray)
    painter.rect_filled(rect, 4.0, eframe::egui::Color32::from_gray(24));

    let time_to_x =
        |utc_ms: i64| -> f32 { rect.left() + projection.time_to_fraction(utc_ms) * rect.width() };
    let x_to_time = |px: f32| -> i64 {
        let frac = ((px - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
        projection.fraction_to_time(frac)
    };

    // 2. Shade Canonical SD Partitions (solid accent tint)
    let canonical_fill = eframe::egui::Color32::from_rgba_unmultiplied(14, 165, 233, 48);
    let canonical_border = eframe::egui::Color32::from_rgba_unmultiplied(56, 189, 248, 120);
    for (p_start, p_end, _) in &canonical_spans {
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
        if eff_start <= live_end {
            let x0 = time_to_x(eff_start).max(rect.left());
            let x1 = time_to_x(live_end).min(rect.right()).max(x0 + 3.0);
            let live_rect = eframe::egui::Rect::from_min_max(
                eframe::egui::pos2(x0, rect.top() + 1.0),
                eframe::egui::pos2(x1, rect.bottom() - 1.0),
            );
            let live_color = eframe::egui::Color32::from_rgb(249, 115, 22);
            painter.rect_filled(live_rect, 2.0, live_color.gamma_multiply(0.16));
            // Diagonal hatch lines for Provisional Live UDP
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

    // 4. Render 1Hz Activity Envelope along the bottom half of the Day Overview lane
    if !activity_1hz.is_empty() {
        let env_bottom = rect.bottom() - 2.0;
        let env_max_h = 18.0;
        let pixel_cols = (rect.width().ceil() as usize).clamp(1, 2048);
        let mut col_peaks = vec![0.0f32; pixel_cols];
        for &(bucket_ms, intensity) in &activity_1hz {
            let frac = projection.time_to_fraction(bucket_ms);
            let col = ((frac * (pixel_cols as f32 - 1.0)).round() as usize).min(pixel_cols - 1);
            col_peaks[col] = col_peaks[col].max(intensity);
        }
        let bar_color = eframe::egui::Color32::from_rgba_unmultiplied(56, 189, 248, 95);
        for (col, &peak) in col_peaks.iter().enumerate() {
            if peak <= 0.0 {
                continue;
            }
            let x = rect.left() + (col as f32 + 0.5) / (pixel_cols as f32) * rect.width();
            let h = (peak * env_max_h).max(1.5);
            painter.line_segment(
                [
                    eframe::egui::pos2(x, env_bottom),
                    eframe::egui::pos2(x, env_bottom - h),
                ],
                eframe::egui::Stroke::new(1.5_f32, bar_color),
            );
        }
    }

    // 5. Render Folded Idle Gap dividers (`// 1h 45m idle //`)
    for seg in projection.segments() {
        if seg.is_folded_gap() {
            let x0 = rect.left() + seg.frac_start * rect.width();
            let x1 = rect.left() + seg.frac_end * rect.width();
            let gap_rect = eframe::egui::Rect::from_min_max(
                eframe::egui::pos2(x0, rect.top()),
                eframe::egui::pos2(x1, rect.bottom()),
            );
            painter.rect_filled(gap_rect, 0.0, eframe::egui::Color32::from_gray(16));
            painter.line_segment(
                [gap_rect.left_top(), gap_rect.left_bottom()],
                eframe::egui::Stroke::new(1.0_f32, eframe::egui::Color32::from_gray(90)),
            );
            painter.line_segment(
                [gap_rect.right_top(), gap_rect.right_bottom()],
                eframe::egui::Stroke::new(1.0_f32, eframe::egui::Color32::from_gray(90)),
            );
            let label = format!(
                "// {} //",
                daqcore::timeline::format_idle_duration(seg.duration_ms())
            );
            if gap_rect.width() >= 28.0 {
                painter.text(
                    gap_rect.center(),
                    eframe::egui::Align2::CENTER_CENTER,
                    &label,
                    eframe::egui::FontId::monospace(9.5),
                    eframe::egui::Color32::from_gray(175),
                );
            }
        }
    }

    // 6. Render Event Annotation Pills (2-row lane packing so stints & faults don't collide)
    let mut pill_interacted = false;
    let mut row_end_x = [rect.left() - 10.0; 2];
    let mut pending_single_click: Option<(String, i64, Option<i64>)> = None;
    let mut pending_shift_click: Option<(String, i64, Option<i64>)> = None;
    let mut pending_edit: Option<daqcore::storage::Annotation> = None;
    let mut pending_delete_id: Option<String> = None;

    for (idx, ann) in annotations.iter().enumerate() {
        let ann_end_ms = ann.end_utc_ms.unwrap_or(ann.start_utc_ms);
        let x0 = time_to_x(ann.start_utc_ms);
        let x1 = time_to_x(ann_end_ms);
        let is_point =
            ann.end_utc_ms.is_none() || ann_end_ms <= ann.start_utc_ms || (x1 - x0).abs() < 8.0;
        let pill_w = if is_point {
            (ann.name.len() as f32 * 6.0 + 18.0).clamp(36.0, 110.0)
        } else {
            (x1 - x0).max(36.0)
        };
        let pill_left = if is_point {
            (x0 - 6.0).clamp(rect.left(), (rect.right() - pill_w).max(rect.left()))
        } else {
            x0.clamp(rect.left(), rect.right())
        };
        let pill_right = (pill_left + pill_w).min(rect.right());
        if pill_right <= pill_left + 2.0 {
            continue;
        }

        let row = if pill_left >= row_end_x[0] + 3.0 {
            0
        } else if pill_left >= row_end_x[1] + 3.0 {
            1
        } else if row_end_x[0] <= row_end_x[1] {
            0
        } else {
            1
        };
        row_end_x[row] = pill_right;

        let y_top = rect.top() + 3.0 + (row as f32) * 19.0;
        let pill_rect = eframe::egui::Rect::from_min_max(
            eframe::egui::pos2(pill_left, y_top),
            eframe::egui::pos2(pill_right, y_top + 16.0),
        );

        let base_color = parse_hex_color(&ann.color, accent_color);
        let is_selected = selected_ann_ids.contains(&ann.id);
        let fill_color = if is_selected {
            base_color.gamma_multiply(0.85)
        } else {
            base_color.gamma_multiply(0.55)
        };
        let stroke = if is_selected {
            eframe::egui::Stroke::new(2.0_f32, eframe::egui::Color32::WHITE)
        } else {
            eframe::egui::Stroke::new(1.0_f32, base_color)
        };

        painter.rect_filled(pill_rect, 4.0, fill_color);
        painter.rect_stroke(pill_rect, 4.0, stroke, eframe::egui::StrokeKind::Inside);

        let text_clip = pill_rect
            .shrink2(eframe::egui::vec2(3.0, 0.0))
            .intersect(clip_rect);
        if text_clip.width() > 6.0 {
            let prefix = if ann.category == "fault" {
                "⚠ "
            } else if is_point {
                "📍 "
            } else {
                ""
            };
            ui.painter_at(text_clip).text(
                eframe::egui::pos2(pill_rect.left() + 4.0, pill_rect.center().y),
                eframe::egui::Align2::LEFT_CENTER,
                format!("{prefix}{}", ann.name),
                eframe::egui::FontId::proportional(10.5),
                eframe::egui::Color32::WHITE,
            );
        }

        let pill_resp = ui
            .interact(
                pill_rect,
                ui.id().with(("day_annotation_pill", &ann.id, idx)),
                eframe::egui::Sense::click(),
            )
            .on_hover_text(format!(
                "{} [{}]\n{} .. {}\n{}\nClick: Snap viewport · Shift+Click: Compare · Right-Click: Edit/Delete",
                ann.name,
                ann.category,
                daqcore::time::Time::from_unix_millis(ann.start_utc_ms).label(),
                daqcore::time::Time::from_unix_millis(ann_end_ms).label(),
                if ann.notes.is_empty() {
                    "(no notes)"
                } else {
                    ann.notes.as_str()
                }
            ));

        if pill_resp.clicked() {
            pill_interacted = true;
            let shift = ui.input(|i| i.modifiers.shift);
            if shift {
                pending_shift_click = Some((ann.id.clone(), ann.start_utc_ms, ann.end_utc_ms));
            } else {
                pending_single_click = Some((ann.id.clone(), ann.start_utc_ms, ann.end_utc_ms));
            }
        }

        pill_resp.context_menu(|ui| {
            ui.label(eframe::egui::RichText::new(&ann.name).strong());
            ui.separator();
            if ui.button("🔍 Snap Viewport to Event").clicked() {
                pending_single_click = Some((ann.id.clone(), ann.start_utc_ms, ann.end_utc_ms));
                ui.close();
            }
            if ui
                .button("⚖ Toggle Comparison Mode (Shift+Click)")
                .clicked()
            {
                pending_shift_click = Some((ann.id.clone(), ann.start_utc_ms, ann.end_utc_ms));
                ui.close();
            }
            if ui.button("✏ Edit Event…").clicked() {
                pending_edit = Some(ann.clone());
                ui.close();
            }
            if ui.button("🗑 Delete Event").clicked() {
                pending_delete_id = Some(ann.id.clone());
                ui.close();
            }
        });
    }

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

    // 7. Draw Active Viewport `[start .. end]` & Playhead on the Day Overview lane
    let vp_left = time_to_x(router.timeline().start_utc_ms()).clamp(rect.left(), rect.right());
    let vp_right = time_to_x(router.timeline().end_utc_ms())
        .clamp(rect.left(), rect.right())
        .max(vp_left + 3.0);
    let vp_rect = eframe::egui::Rect::from_min_max(
        eframe::egui::pos2(vp_left, rect.top()),
        eframe::egui::pos2(vp_right, rect.bottom()),
    );
    painter.rect_filled(
        vp_rect,
        2.0,
        eframe::egui::Color32::from_rgba_unmultiplied(255, 255, 255, 26),
    );
    painter.rect_stroke(
        vp_rect,
        2.0,
        eframe::egui::Stroke::new(1.5_f32, eframe::egui::Color32::WHITE),
        eframe::egui::StrokeKind::Inside,
    );

    let ph_x = time_to_x(router.timeline().setpoint_utc_ms()).clamp(rect.left(), rect.right());
    painter.line_segment(
        [
            eframe::egui::pos2(ph_x, rect.top()),
            eframe::egui::pos2(ph_x, rect.bottom()),
        ],
        eframe::egui::Stroke::new(1.5_f32, eframe::egui::Color32::YELLOW),
    );

    // Outer frame stroke
    painter.rect_stroke(
        rect,
        4.0,
        ui.visuals().widgets.noninteractive.bg_stroke,
        eframe::egui::StrokeKind::Inside,
    );

    // 8. Interactions on the Day Overview background (pan/jump, scroll-wheel zoom, right-click add event)
    if !pill_interacted
        && (lane_response.dragged() || lane_response.clicked())
        && let Some(pointer) = lane_response.interact_pointer_pos()
    {
        let target_ms = x_to_time(pointer.x);
        let span_ms = (router.timeline().end_utc_ms() - router.timeline().start_utc_ms()).max(100);
        router
            .timeline_mut()
            .pan_to_start_ms(target_ms - span_ms / 2);
    }

    if lane_response.hovered() {
        let scroll_y = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll_y.abs() > 0.5
            && let Some(pointer) = ui.input(|i| i.pointer.hover_pos())
        {
            let anchor_ms = x_to_time(pointer.x);
            let factor = if scroll_y > 0.0 { 0.8 } else { 1.25 };
            router.timeline_mut().zoom_around_ms(anchor_ms, factor);
        }
    }

    let hover_time_ms = lane_response
        .hover_pos()
        .map(|p| x_to_time(p.x))
        .unwrap_or_else(|| router.timeline().setpoint_utc_ms());

    lane_response.context_menu(|ui| {
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

fn show_active_window_scrub_lane(
    ui: &mut eframe::egui::Ui,
    router: &mut daqcore::router::VirtualTimelineRouter,
    accent_color: eframe::egui::Color32,
) {
    let win_start_ms = router.timeline().start_utc_ms();
    let win_end_ms = router.timeline().end_utc_ms().max(win_start_ms + 1);
    let win_span_ms = (win_end_ms - win_start_ms).max(1) as f64;
    let overlapping_annotations: Vec<daqcore::storage::Annotation> = router
        .query_annotations_in_range(win_start_ms..=win_end_ms)
        .unwrap_or_default();

    let scrub_height = 46.0;
    let width = ui.available_width().max(1.0);
    let (rect, background) = ui.allocate_exact_size(
        eframe::egui::vec2(width, scrub_height),
        eframe::egui::Sense::click_and_drag(),
    );
    let clip_rect = rect.intersect(ui.clip_rect());
    let painter = ui.painter_at(clip_rect);

    painter.rect_filled(rect, 4.0, eframe::egui::Color32::from_gray(18));

    let pad_x = (rect.width() * 0.03).clamp(6.0, 32.0);
    let area = rect.shrink2(eframe::egui::vec2(pad_x, 0.0));
    let axis_y = area.bottom() - 15.0;

    let time_to_x = |utc_ms: i64| -> f32 {
        let frac = ((utc_ms - win_start_ms) as f64 / win_span_ms).clamp(0.0, 1.0) as f32;
        area.left() + frac * area.width()
    };
    let x_to_time = |px: f32| -> i64 {
        let frac = ((px - area.left()) / area.width().max(1.0)).clamp(0.0, 1.0) as f64;
        win_start_ms + (frac * win_span_ms).round() as i64
    };

    // Subtle background bands for annotations inside active window
    for ann in &overlapping_annotations {
        let ann_end = ann.end_utc_ms.unwrap_or(ann.start_utc_ms);
        let x0 = time_to_x(ann.start_utc_ms.max(win_start_ms));
        let x1 = time_to_x(ann_end.min(win_end_ms)).max(x0 + 2.0);
        let band_rect = eframe::egui::Rect::from_min_max(
            eframe::egui::pos2(x0, area.top() + 2.0),
            eframe::egui::pos2(x1, axis_y),
        );
        let color = parse_hex_color(&ann.color, accent_color);
        painter.rect_filled(band_rect, 2.0, color.gamma_multiply(0.18));
    }

    // Ruler axis line and timestamp ticks
    painter.line_segment(
        [
            eframe::egui::pos2(area.left(), axis_y),
            eframe::egui::pos2(area.right(), axis_y),
        ],
        ui.visuals().widgets.noninteractive.fg_stroke,
    );

    let tick_count = ((area.width() / 130.0).floor() as usize).clamp(2, 8);
    for i in 0..=tick_count {
        let frac = i as f64 / tick_count as f64;
        let tick_ms = win_start_ms + (win_span_ms * frac).round() as i64;
        let px = area.left() + (frac as f32) * area.width();
        painter.line_segment(
            [
                eframe::egui::pos2(px, axis_y - 4.0),
                eframe::egui::pos2(px, axis_y + 2.0),
            ],
            ui.visuals().widgets.noninteractive.fg_stroke,
        );
        let align = if i == 0 {
            eframe::egui::Align2::LEFT_TOP
        } else if i == tick_count {
            eframe::egui::Align2::RIGHT_TOP
        } else {
            eframe::egui::Align2::CENTER_TOP
        };
        painter.text(
            eframe::egui::pos2(px, axis_y + 2.0),
            align,
            daqcore::time::Time::from_unix_millis(tick_ms).label(),
            eframe::egui::FontId::monospace(9.5),
            ui.visuals().text_color(),
        );
    }

    let timeline = router.timeline_mut();
    let mut handle_dragged = false;

    // Left (`start`) and Right (`end`) zoom handles + Playhead (`setpoint`) handle
    let handles = [
        ("start", timeline.start_utc_ms(), area.left(), accent_color),
        ("end", timeline.end_utc_ms(), area.right(), accent_color),
        (
            "playhead",
            timeline.setpoint_utc_ms(),
            time_to_x(timeline.setpoint_utc_ms()),
            eframe::egui::Color32::YELLOW,
        ),
    ];

    for (idx, (name, val_ms, px, color)) in handles.into_iter().enumerate() {
        let y = area.top() + 12.0;
        let handle_rect = eframe::egui::Rect::from_center_size(
            eframe::egui::pos2(px, y),
            eframe::egui::vec2(18.0, 18.0),
        );
        let resp = ui
            .interact(
                handle_rect,
                ui.id().with(("scrub_handle", idx)),
                eframe::egui::Sense::drag(),
            )
            .on_hover_text(format!(
                "Drag {name}: {}",
                daqcore::time::Time::from_unix_millis(val_ms).label()
            ));

        painter.line_segment(
            [
                eframe::egui::pos2(px, area.top() + 2.0),
                eframe::egui::pos2(px, axis_y),
            ],
            eframe::egui::Stroke::new(if idx == 2 { 2.0_f32 } else { 1.5_f32 }, color),
        );
        painter.circle_filled(eframe::egui::pos2(px, y), 4.5, color);

        if resp.dragged() {
            handle_dragged = true;
            let delta_x = resp.drag_delta().x;
            let delta_ms = ((delta_x / area.width().max(1.0)) as f64 * win_span_ms).round() as i64;
            match idx {
                0 => {
                    let new_start = (timeline.start_utc_ms() + delta_ms)
                        .min(timeline.end_utc_ms() - daqcore::timeline::MIN_WINDOW_MS);
                    timeline.set_start(daqcore::time::Time::from_unix_millis(new_start));
                    let _ = timeline.set_window_secs(timeline.end().secs(timeline.start()));
                }
                1 => {
                    let new_end = (timeline.end_utc_ms() + delta_ms)
                        .max(timeline.start_utc_ms() + daqcore::timeline::MIN_WINDOW_MS);
                    timeline.set_end(daqcore::time::Time::from_unix_millis(new_end));
                    let _ = timeline.set_window_secs(timeline.end().secs(timeline.start()));
                }
                _ => {
                    if let Some(pointer) = resp.interact_pointer_pos() {
                        let new_sp = x_to_time(pointer.x);
                        timeline.set_setpoint(daqcore::time::Time::from_unix_millis(new_sp));
                    }
                }
            }
        }
    }

    if !handle_dragged
        && (background.clicked() || background.dragged())
        && let Some(pointer) = background.interact_pointer_pos()
    {
        let new_sp = x_to_time(pointer.x);
        timeline.set_setpoint(daqcore::time::Time::from_unix_millis(new_sp));
    }

    if background.hovered() {
        let scroll_y = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll_y.abs() > 0.5
            && let Some(pointer) = ui.input(|i| i.pointer.hover_pos())
        {
            let anchor_ms = x_to_time(pointer.x);
            let factor = if scroll_y > 0.0 { 0.8 } else { 1.25 };
            timeline.zoom_around_ms(anchor_ms, factor);
        }
    }

    painter.rect_stroke(
        rect,
        4.0,
        ui.visuals().widgets.noninteractive.bg_stroke,
        eframe::egui::StrokeKind::Inside,
    );
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
        "➕ Add Track Day Event Annotation"
    } else {
        "✏ Edit Track Day Event Annotation"
    };

    eframe::egui::Window::new(title)
        .id(eframe::egui::Id::new("annotation_editor_modal"))
        .collapsible(false)
        .resizable(false)
        .open(&mut keep_open)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Event Name:");
                ui.text_edit_singleline(&mut draft.name);
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
                        "✓"
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
                if ui.button("💾 Save Annotation").clicked() {
                    save_clicked = true;
                }
                if !draft.is_new && ui.button("🗑 Delete").clicked() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn master_timeline_two_lanes_gap_folding_and_annotation_modal_render_cleanly() {
        let tmp_store = tempfile::tempdir().expect("temp store");
        let reg = daqcore::storage::Registry::open_store(tmp_store.path()).expect("open store");
        reg.upsert_track_day(&daqcore::storage::TrackDay {
            id: "2026-09-06".to_owned(),
            title: "2026-09-06".to_owned(),
            location: "Purdue Grand Prix Track".to_owned(),
            start_utc_ms: 1_000_000,
            end_utc_ms: 9_000_000,
        })
        .unwrap();

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
        reg.upsert_annotation(&ann1).unwrap();
        reg.upsert_annotation(&ann2).unwrap();

        let mut router = daqcore::router::VirtualTimelineRouter::open_store(
            tmp_store.path(),
            daqcore::time::Time::from_unix_millis(1_000_000),
            30.0,
        )
        .expect("router open_store");
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

        let mut timeline_ui = TimelineUiState::default();
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
            });
            assert!(
                diagnostic.is_none(),
                "unexpected timeline diagnostic: {diagnostic:?}"
            );
        }
    }

    #[test]
    fn per26_multi_stint_day_timeline_gap_folding_comparison_and_signal_browser_e2e() {
        let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let repo_root = manifest_dir.join("../..");
        let dbc_path = repo_root.join("superdbc/superdbc_per26.json");
        let log1 = repo_root.join("PER26/2026-09-06/1_no_cooling/raw/log-2026-09-06--18-36-02.log");
        let log2 = repo_root.join("PER26/2026-09-06/2_regen/raw/log-2026-09-06--20-21-44.log");
        if !log1.exists() || !log2.exists() || !dbc_path.exists() {
            return;
        }

        let database = daqcore::superdbc::database::Database::load(&dbc_path).expect("load PER26");
        let tmp_store = tempfile::tempdir().expect("temp store");
        let tmp_raw1 = tempfile::tempdir().expect("temp raw 1");
        let tmp_raw2 = tempfile::tempdir().expect("temp raw 2");
        std::fs::copy(&log1, tmp_raw1.path().join("log-2026-09-06--18-36-02.log")).unwrap();
        std::fs::copy(&log2, tmp_raw2.path().join("log-2026-09-06--20-21-44.log")).unwrap();

        let mut saw_finished = false;
        let summary_1 = daqcore::log_parse::import_logs_to_store_with_progress(
            tmp_raw1.path(),
            tmp_store.path(),
            &database,
            |p| {
                if matches!(p, daqcore::log_parse::ImportProgress::Complete(_)) {
                    saw_finished = true;
                }
            },
        )
        .expect("import stint 1");
        let summary_2 = daqcore::log_parse::import_logs_to_store_with_progress(
            tmp_raw2.path(),
            tmp_store.path(),
            &database,
            |_| {},
        )
        .expect("import stint 2");
        assert!(saw_finished);
        assert!(!summary_1.manifests.is_empty() && !summary_2.manifests.is_empty());

        let m1_start = summary_1.manifests[0].start_utc_ms;
        let m2_start = summary_2.manifests[0].start_utc_ms;
        let m2_end = summary_2.manifests[0].end_utc_ms;

        let mut router = daqcore::router::VirtualTimelineRouter::open_store(
            tmp_store.path(),
            daqcore::time::Time::from_unix_millis(m1_start),
            30.0,
        )
        .expect("open_store router");

        assert_eq!(router.partitions().len(), 2);
        let track_days = router.list_track_days().expect("track_days");
        assert!(track_days.iter().any(|td| td.id == "2026-09-06"));
        router
            .select_track_day(Some("2026-09-06".to_owned()))
            .expect("select 2026-09-06");

        let (day_start, day_end) = router
            .timeline()
            .day_bounds_ms()
            .expect("day bounds populated");
        assert!(day_end - day_start > 5_000_000, "span across ~1h45m gap");

        // Verify 1Hz activity envelope and canonical partition clock provenance
        let activity = router.activity_envelope_1hz(day_start..=day_end);
        assert!(
            !activity.is_empty(),
            "expected non-empty 1Hz activity envelope across PER26 stints"
        );
        let clock_prov = router
            .clock_provenance_at(m1_start + 1_000)
            .expect("clock provenance present");
        assert!(
            matches!(
                clock_prov,
                daqcore::router::ClockProvenance::CanonicalPartition(_)
            ),
            "expected CanonicalPartition provenance on PER26 logs, got {clock_prov:?}"
        );

        // Verify dead-time gap folding across the ~1h45m gap between 1_no_cooling and 2_regen
        let active_spans: Vec<(i64, i64)> = router
            .partitions()
            .iter()
            .map(|p| (p.start_utc_ms, p.end_utc_ms))
            .collect();
        let proj_folded =
            daqcore::timeline::GapFoldedProjection::new(day_start, day_end, &active_spans, true);
        assert!(
            proj_folded.segments().iter().any(|s| s.is_folded_gap()),
            "expected >=1 folded idle gap between 1_no_cooling and 2_regen"
        );
        for seg in proj_folded.segments() {
            let mid = (seg.start_utc_ms + seg.end_utc_ms) / 2;
            let f = proj_folded.time_to_fraction(mid);
            let roundtrip = proj_folded.fraction_to_time(f);
            assert!((roundtrip - mid).abs() <= 5_000);
        }

        // Verify auto-detected annotations and manual annotation CRUD
        let initial_anns = router
            .query_annotations_in_range(day_start..=day_end)
            .expect("query annotations");
        assert!(
            !initial_anns.is_empty(),
            "expected auto-detected stints/faults from PER26 import"
        );

        let manual_ann = daqcore::storage::Annotation {
            id: "manual_bookmark_test".to_owned(),
            track_day_id: "2026-09-06".to_owned(),
            name: "Driver Swap Check".to_owned(),
            category: "note".to_owned(),
            color: "#F59E0B".to_owned(),
            start_utc_ms: m1_start + 5_000,
            end_utc_ms: Some(m1_start + 25_000),
            author: "user".to_owned(),
            source: "manual".to_owned(),
            notes: "Checked accumulator temps".to_owned(),
            updated_at_ms: m1_start + 25_000,
            deleted: false,
        };
        router
            .upsert_annotation(&manual_ann)
            .expect("upsert manual annotation");

        let second_ann = daqcore::storage::Annotation {
            id: "manual_stint_2".to_owned(),
            track_day_id: "2026-09-06".to_owned(),
            name: "Regen Stint Window".to_owned(),
            category: "driving_stint".to_owned(),
            color: "#22C55E".to_owned(),
            start_utc_ms: m2_start + 2_000,
            end_utc_ms: Some(m2_start + 22_000),
            author: "user".to_owned(),
            source: "manual".to_owned(),
            notes: "Regen test".to_owned(),
            updated_at_ms: m2_start + 22_000,
            deleted: false,
        };
        router
            .upsert_annotation(&second_ann)
            .expect("upsert second annotation");

        // Enter Comparison Mode across both stints and render Timeline + Scope + Signal Browser
        router.timeline_mut().select_single_annotation(
            manual_ann.id.clone(),
            manual_ann.start_utc_ms,
            manual_ann.end_utc_ms,
        );
        router.timeline_mut().toggle_comparison_annotation(
            second_ann.id.clone(),
            second_ann.start_utc_ms,
            second_ann.end_utc_ms,
        );
        assert!(router.timeline().is_comparison_mode());

        let selection = daqcore::can_thread::BusSelection::default_single(&database)
            .unwrap_or(daqcore::can_thread::BusSelection::Single(0));
        let db_sel = app::DatabaseSelection {
            database,
            selection,
        };

        let mut timeline_ui = TimelineUiState::default();
        let mut browser_state = ui::signal_browser::SignalBrowserState {
            search_query: "*vbatt*".to_owned(),
            ..Default::default()
        };
        let mut scope = ui::scope::Scope::new_channel(
            1,
            "pack_stats.vbatt_adc".to_owned(),
            "Battery Voltage".to_owned(),
            "V".to_owned(),
        );

        let ctx = eframe::egui::Context::default();
        ui::theme::store_theme(&ctx, ui::theme::ThemeSelection::default().get_colors());
        let raw_input = eframe::egui::RawInput {
            screen_rect: Some(eframe::egui::Rect::from_min_size(
                eframe::egui::Pos2::ZERO,
                eframe::egui::vec2(1440.0, 900.0),
            )),
            ..Default::default()
        };
        let mut bus_load = Vec::new();
        let mut diagnostic = None;
        let mut action_queue = Vec::new();

        let _ = ctx.run(raw_input, |ctx| {
            eframe::egui::TopBottomPanel::top("e2e_timeline").show(ctx, |ui| {
                show_timeline_contents(
                    ui,
                    &mut router,
                    &mut timeline_ui,
                    &mut bus_load,
                    &mut diagnostic,
                );
            });
            eframe::egui::SidePanel::left("e2e_browser").show(ctx, |ui| {
                ui::signal_browser::show_signal_browser(
                    ui,
                    &mut browser_state,
                    Some(&db_sel),
                    &mut router,
                    &mut action_queue,
                    &mut diagnostic,
                );
            });
            eframe::egui::CentralPanel::default().show(ctx, |ui| {
                let view = crate::telemetry::TelemetryView::new(&router);
                let _ = scope.show(ui, Some(&db_sel), &view);
            });
        });
        assert!(
            diagnostic.is_none(),
            "unexpected diagnostic: {diagnostic:?}"
        );

        // Soft-delete manual annotation and verify removal
        router
            .soft_delete_annotation(&manual_ann.id, m2_end + 1_000)
            .expect("soft delete");
        let after_delete = router
            .query_annotations_in_range(day_start..=day_end)
            .expect("query after delete");
        assert!(!after_delete.iter().any(|a| a.id == manual_ann.id));
    }
}
