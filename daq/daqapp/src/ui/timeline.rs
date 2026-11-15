use crate::{app, ui, workspace_view};

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

/// Persistent UI state for the Master Timeline Bar, Calendar/Subchunk Navigator, and Annotation Editor modal.
#[derive(Debug, Clone)]
pub struct TimelineUiState {
    pub editor: Option<AnnotationDraft>,
    pub show_stints: bool,
    pub show_faults: bool,
    pub show_chunk_navigator: bool,
    pub chunk_search_query: String,
    pub chunk_filter: ChunkFilter,
}

impl Default for TimelineUiState {
    fn default() -> Self {
        Self {
            editor: None,
            show_stints: true,
            show_faults: true,
            show_chunk_navigator: false,
            chunk_search_query: String::new(),
            chunk_filter: ChunkFilter::All,
        }
    }
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

    if span_changed {
        app.save_settings();
    }
}

/// Render the bottom-docked Master Timeline & Transport Bar inside `ui`. Returns `true` if `window_secs` changed.
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

    // -------------------------------------------------------------------------
    // Top Strip: Day Overview, 1Hz Activity Envelope & Event Annotation Ribbon
    // -------------------------------------------------------------------------
    show_day_overview_lane(ui, router, timeline_ui, diagnostic, accent_color);

    ui.add_space(1.0);

    // -------------------------------------------------------------------------
    // Middle Strip: Active Window Scrub & Playhead Ruler
    // -------------------------------------------------------------------------
    show_active_window_scrub_lane(ui, router, timeline_ui, accent_color);

    ui.add_space(2.0);

    // -------------------------------------------------------------------------
    // Bottom Strip: Unified Transport, Time Readout, Stints/Faults & Provenance
    // -------------------------------------------------------------------------
    let track_days = router.list_track_days().unwrap_or_default();
    let active_day_id = router.timeline().active_track_day_id().map(str::to_owned);
    let mut requested_day_change: Option<Option<String>> = None;
    let mut clear_live_clicked = false;
    let mut open_new_bookmark = false;
    let mut open_new_interval = false;

    // Pre-query active day annotations so the transport footer can display `Stints (N)` and `Faults (N)` menus
    let captured_for_menu = router.captured_time_span();
    let (menu_day_min, menu_day_max) = router
        .timeline()
        .day_bounds_ms()
        .or_else(|| captured_for_menu.map(|(s, e)| (s.unix_millis(), e.unix_millis())))
        .unwrap_or_else(|| {
            (
                router.timeline().start_utc_ms(),
                router.timeline().end_utc_ms(),
            )
        });
    let menu_day_min = menu_day_min.min(router.timeline().start_utc_ms());
    let menu_day_max = menu_day_max
        .max(router.timeline().end_utc_ms())
        .max(menu_day_min + 1_000);
    let all_day_annotations: Vec<daqcore::storage::Annotation> = router
        .query_annotations_in_range(menu_day_min..=menu_day_max)
        .unwrap_or_default()
        .into_iter()
        .filter(|a| {
            active_day_id
                .as_deref()
                .is_none_or(|day_id| a.track_day_id == day_id)
        })
        .collect();
    let mut stint_annotations: Vec<daqcore::storage::Annotation> = all_day_annotations
        .iter()
        .filter(|a| a.category != "fault")
        .cloned()
        .collect();
    stint_annotations.sort_by_key(|a| (a.start_utc_ms, a.end_utc_ms.unwrap_or(a.start_utc_ms)));
    let mut fault_annotations: Vec<daqcore::storage::Annotation> = all_day_annotations
        .iter()
        .filter(|a| a.category == "fault")
        .cloned()
        .collect();
    fault_annotations.sort_by_key(|a| (a.start_utc_ms, a.end_utc_ms.unwrap_or(a.start_utc_ms)));

    let mut menu_single_click: Option<(String, i64, Option<i64>)> = None;
    let mut menu_shift_click: Option<(String, i64, Option<i64>)> = None;
    let mut menu_edit_ann: Option<daqcore::storage::Annotation> = None;
    let mut menu_delete_id: Option<String> = None;
    let mut menu_jump_fault_ms: Option<i64> = None;
    let selected_ann_ids_for_menu: Vec<String> =
        router.timeline().selected_annotation_ids().to_vec();

    // Provenance & LOD metadata for compact badge
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

    let selected_ids: Vec<String> = router.timeline().selected_annotation_ids().to_vec();
    let is_comparing = router.timeline().is_comparison_mode();
    let mut clear_comparison = false;

    let theme = ui::theme::get_theme(ui.ctx());
    let border_color = theme.border_color();
    let bg_color = ui.visuals().extreme_bg_color;
    let stint_color = eframe::egui::Color32::from_rgb(34, 197, 94);
    let fault_color = eframe::egui::Color32::from_rgb(245, 158, 11);
    let event_color = eframe::egui::Color32::from_rgb(168, 85, 247);

    ui.horizontal_wrapped(|ui| {
        ui.style_mut().wrap_mode = Some(eframe::egui::TextWrapMode::Extend);
        ui.spacing_mut().item_spacing = eframe::egui::vec2(3.0, 2.0);

        // =====================================================================
        // GROUP 1: Playhead Readout & Clock Provenance (Far Left)
        // =====================================================================
        let ts_pill = eframe::egui::Button::new(
            eframe::egui::RichText::new(format!("{active_date_lbl}  {playhead_lbl} UTC"))
                .monospace()
                .size(11.0)
                .strong()
                .color(ui.visuals().strong_text_color()),
        )
        .wrap_mode(eframe::egui::TextWrapMode::Extend)
        .fill(bg_color)
        .stroke(eframe::egui::Stroke::new(1.0_f32, border_color))
        .corner_radius(3.0)
        .sense(eframe::egui::Sense::hover());
        ui.add(ts_pill).on_hover_text(format!(
            "Playhead: {playhead_lbl}\nViewport: {viewport_start_lbl} .. {viewport_end_lbl} ({:.2}s)",
            router.timeline().window_secs()
        ));

        let prov_pill = eframe::egui::Button::new(
            eframe::egui::RichText::new(format!("{prov_text} · {active_res}"))
                .color(prov_color)
                .small(),
        )
        .wrap_mode(eframe::egui::TextWrapMode::Extend)
        .fill(prov_color.gamma_multiply(0.14))
        .stroke(eframe::egui::Stroke::new(
            1.0_f32,
            prov_color.gamma_multiply(0.55),
        ))
        .corner_radius(3.0)
        .sense(eframe::egui::Sense::hover());
        ui.add(prov_pill).on_hover_text(format!(
            "{prov_hover}\nLOD Tier: {active_res}\nTile Cache: {tile_cache_mib:.1} MiB\nSD Partitions: {partition_count} · Live Frames: {live_frame_count}"
        ));

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(2.0);

        // =====================================================================
        // GROUP 2: Playback Transport Deck (Center-Left, Accent-Highlighted Play)
        // =====================================================================
        let timeline = router.timeline_mut();
        if ui
            .small_button("⏮")
            .on_hover_text("Step playhead backward 100 ms")
            .clicked()
        {
            timeline.step_by_ms(-100);
        }
        if ui
            .small_button("⏪")
            .on_hover_text("Step playhead backward 10 ms")
            .clicked()
        {
            timeline.step_by_ms(-10);
        }

        let is_playing = timeline.is_playing();
        let play_color = if is_playing {
            fault_color
        } else {
            accent_color
        };
        let play_btn = eframe::egui::Button::new(
            eframe::egui::RichText::new(if is_playing { "⏸ Pause" } else { "▶ Play" })
                .size(11.0)
                .strong()
                .color(if theme.is_light && !is_playing {
                    accent_color
                } else {
                    ui.visuals().strong_text_color()
                }),
        )
        .wrap_mode(eframe::egui::TextWrapMode::Extend)
        .fill(play_color.gamma_multiply(0.24))
        .stroke(eframe::egui::Stroke::new(1.0_f32, play_color))
        .corner_radius(3.0);
        if ui
            .add(play_btn)
            .on_hover_text("Play / pause playhead scrub (Space)")
            .clicked()
        {
            timeline.toggle_playing();
        }

        if ui
            .small_button("⏩")
            .on_hover_text("Step playhead forward 10 ms")
            .clicked()
        {
            timeline.step_by_ms(10);
        }
        if ui
            .small_button("⏭")
            .on_hover_text("Step playhead forward 100 ms")
            .clicked()
        {
            timeline.step_by_ms(100);
        }

        let current_speed = timeline.playback_speed();
        eframe::egui::ComboBox::from_id_salt("timeline_playback_speed")
            .width(46.0)
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

        // LIVE Follow toggle
        let is_live = timeline.is_live();
        let live_btn = eframe::egui::Button::new(
            eframe::egui::RichText::new(if is_live { "🔴 LIVE" } else { "• LIVE" })
                .size(10.5)
                .strong()
                .color(if is_live {
                    eframe::egui::Color32::WHITE
                } else {
                    theme.muted_text_color()
                }),
        )
        .wrap_mode(eframe::egui::TextWrapMode::Extend)
        .fill(if is_live {
            eframe::egui::Color32::from_rgb(220, 38, 38)
        } else {
            bg_color
        })
        .stroke(eframe::egui::Stroke::new(
            1.0_f32,
            if is_live {
                eframe::egui::Color32::from_rgb(248, 113, 113)
            } else {
                border_color
            },
        ))
        .corner_radius(3.0);
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
        if live_frame_count > 0
            && ui
                .small_button("Clear Live")
                .on_hover_text("Clear in-memory live ring buffer")
                .clicked()
        {
            clear_live_clicked = true;
        }

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(2.0);

        // =====================================================================
        // GROUP 3: Stints, Faults & Event Markers (Color-Coded by Semantics)
        // =====================================================================
        // 3a. Stint Prev / Menu / Next cluster (Emerald green tint matching stint blocks)
        let cur_start = router.timeline().start_utc_ms();
        if !stint_annotations.is_empty()
            && ui
                .add(
                    eframe::egui::Button::new(
                        eframe::egui::RichText::new("◀")
                            .size(10.0)
                            .color(stint_color),
                    )
                    .small()
                    .fill(stint_color.gamma_multiply(0.12))
                    .stroke(eframe::egui::Stroke::new(
                        1.0_f32,
                        stint_color.gamma_multiply(0.45),
                    )),
                )
                .on_hover_text("Jump to previous Driving Stint / Event")
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

        ui.scope(|ui| {
            if !stint_annotations.is_empty() {
                ui.visuals_mut().widgets.inactive.bg_fill = stint_color.gamma_multiply(0.14);
                ui.visuals_mut().widgets.inactive.bg_stroke =
                    eframe::egui::Stroke::new(1.0_f32, stint_color.gamma_multiply(0.5));
            }
            ui.menu_button(
                eframe::egui::RichText::new(format!("🏷 Stints ({})", stint_annotations.len()))
                    .size(11.0)
                    .color(if stint_annotations.is_empty() {
                        ui.visuals().text_color()
                    } else {
                        stint_color
                    }),
                |ui| {
                    ui.set_min_width(310.0);
                    ui.horizontal(|ui| {
                        ui.checkbox(
                            &mut timeline_ui.show_stints,
                            "Show Stints & Events on timeline",
                        );
                        if ui
                            .small_button("📅 Browse All Chunks…")
                            .on_hover_text("Open floating Calendar & Subchunk Navigator")
                            .clicked()
                        {
                            timeline_ui.show_chunk_navigator = !timeline_ui.show_chunk_navigator;
                            ui.close();
                        }
                    });
                    if !track_days.is_empty() {
                        ui.horizontal(|ui| {
                            ui.label(eframe::egui::RichText::new("Track Day:").small().weak());
                            let selected_text = active_day_id.as_deref().unwrap_or("All Days");
                            eframe::egui::ComboBox::from_id_salt("timeline_track_day_selector")
                                .width(140.0)
                                .selected_text(selected_text)
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
                        });
                    }
                    ui.separator();
                    if stint_annotations.is_empty() {
                        ui.label(
                            eframe::egui::RichText::new("No stints or events in this track day.")
                                .italics()
                                .weak(),
                        );
                    } else {
                        eframe::egui::ScrollArea::vertical()
                            .max_height(260.0)
                            .show(ui, |ui| {
                                for ann in &stint_annotations {
                                    let is_sel = selected_ann_ids_for_menu.contains(&ann.id);
                                    let dur_s = ann
                                        .end_utc_ms
                                        .map(|e| (e - ann.start_utc_ms).max(0) as f64 / 1000.0);
                                    let time_lbl =
                                        daqcore::time::Time::from_unix_millis(ann.start_utc_ms)
                                            .label();
                                    let row_lbl = match dur_s {
                                        Some(d) if d > 0.0 => {
                                            format!("{}  ({time_lbl}, {d:.1}s)", ann.name)
                                        }
                                        _ => format!("📍 {}  ({time_lbl})", ann.name),
                                    };
                                    ui.horizontal(|ui| {
                                        if ui
                                            .selectable_label(is_sel, row_lbl)
                                            .on_hover_text(
                                                "Click to snap viewport to this stint/event",
                                            )
                                            .clicked()
                                        {
                                            menu_single_click = Some((
                                                ann.id.clone(),
                                                ann.start_utc_ms,
                                                ann.end_utc_ms,
                                            ));
                                            ui.close();
                                        }
                                        if ui
                                            .small_button(if is_sel { "-Cmp" } else { "+Cmp" })
                                            .on_hover_text(
                                                "Toggle multi-stint Comparison / Overlay Mode",
                                            )
                                            .clicked()
                                        {
                                            menu_shift_click = Some((
                                                ann.id.clone(),
                                                ann.start_utc_ms,
                                                ann.end_utc_ms,
                                            ));
                                        }
                                        if ui
                                            .small_button("Edit")
                                            .on_hover_text("Edit event")
                                            .clicked()
                                        {
                                            menu_edit_ann = Some(ann.clone());
                                            ui.close();
                                        }
                                        if ui
                                            .small_button("×")
                                            .on_hover_text("Delete event")
                                            .clicked()
                                        {
                                            menu_delete_id = Some(ann.id.clone());
                                        }
                                    });
                                }
                            });
                    }
                },
            );
        });

        if !stint_annotations.is_empty()
            && ui
                .add(
                    eframe::egui::Button::new(
                        eframe::egui::RichText::new("▶")
                            .size(10.0)
                            .color(stint_color),
                    )
                    .small()
                    .fill(stint_color.gamma_multiply(0.12))
                    .stroke(eframe::egui::Stroke::new(
                        1.0_f32,
                        stint_color.gamma_multiply(0.45),
                    )),
                )
                .on_hover_text("Jump to next Driving Stint / Event")
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

        ui.add_space(2.0);

        // 3b. Faults Menu Pill (Amber tint when faults > 0 so faults stand out)
        ui.scope(|ui| {
            if !fault_annotations.is_empty() {
                ui.visuals_mut().widgets.inactive.bg_fill = fault_color.gamma_multiply(0.14);
                ui.visuals_mut().widgets.inactive.bg_stroke =
                    eframe::egui::Stroke::new(1.0_f32, fault_color.gamma_multiply(0.55));
            }
            ui.menu_button(
                eframe::egui::RichText::new(format!("⚠ Faults ({})", fault_annotations.len()))
                    .size(11.0)
                    .color(if fault_annotations.is_empty() {
                        theme.muted_text_color()
                    } else {
                        fault_color
                    }),
                |ui| {
                    ui.set_min_width(340.0);
                    ui.checkbox(
                        &mut timeline_ui.show_faults,
                        "Show fault tick markers on timeline",
                    );
                    ui.separator();
                    if fault_annotations.is_empty() {
                        ui.label(
                            eframe::egui::RichText::new("No faults detected in this track day.")
                                .italics()
                                .weak(),
                        );
                    } else {
                        let mut grouped: Vec<(String, Vec<&daqcore::storage::Annotation>)> =
                            Vec::new();
                        for f in &fault_annotations {
                            let short = f
                                .name
                                .strip_prefix("Fault: ")
                                .unwrap_or(&f.name)
                                .to_owned();
                            if let Some((_, list)) = grouped.iter_mut().find(|(k, _)| k == &short)
                            {
                                list.push(f);
                            } else {
                                grouped.push((short, vec![f]));
                            }
                        }
                        ui.label(
                            eframe::egui::RichText::new(format!(
                                "{} fault types ({} total transitions) — click to jump:",
                                grouped.len(),
                                fault_annotations.len()
                            ))
                            .weak()
                            .small(),
                        );
                        eframe::egui::ScrollArea::vertical()
                            .max_height(300.0)
                            .show(ui, |ui| {
                                for (fault_name, occurrences) in grouped {
                                    let first_ms = occurrences[0].start_utc_ms;
                                    let first_lbl =
                                        daqcore::time::Time::from_unix_millis(first_ms).label();
                                    let header = format!(
                                        "⚠ {}  [{}×, first @ {}]",
                                        fault_name,
                                        occurrences.len(),
                                        first_lbl
                                    );
                                    ui.collapsing(header, |ui| {
                                        for (idx, occ) in occurrences.iter().take(50).enumerate() {
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
                                                .small_button(format!(
                                                    "#{} @ {}{}",
                                                    idx + 1,
                                                    t_lbl,
                                                    dur_str
                                                ))
                                                .on_hover_text(
                                                    "Jump playhead and 10s window to this fault",
                                                )
                                                .clicked()
                                            {
                                                menu_jump_fault_ms = Some(occ.start_utc_ms);
                                                ui.close();
                                            }
                                        }
                                        if occurrences.len() > 50 {
                                            ui.label(
                                                eframe::egui::RichText::new(format!(
                                                    "…and {} more occurrences",
                                                    occurrences.len() - 50
                                                ))
                                                .weak()
                                                .small(),
                                            );
                                        }
                                    });
                                }
                            });
                    }
                },
            );
        });

        // 3c. Quick Bookmark / Event creation buttons (Violet/Indigo accent)
        let mark_btn = eframe::egui::Button::new(
            eframe::egui::RichText::new("📍 +Mark")
                .size(10.5)
                .color(event_color),
        )
        .wrap_mode(eframe::egui::TextWrapMode::Extend)
        .fill(event_color.gamma_multiply(0.10))
        .stroke(eframe::egui::Stroke::new(
            1.0_f32,
            event_color.gamma_multiply(0.45),
        ))
        .corner_radius(3.0);
        if ui
            .add(mark_btn)
            .on_hover_text("Drop a point event bookmark at the current Playhead (Hotkey: M)")
            .clicked()
        {
            open_new_bookmark = true;
        }

        let event_btn = eframe::egui::Button::new(
            eframe::egui::RichText::new("🏷 +Event")
                .size(10.5)
                .color(event_color),
        )
        .wrap_mode(eframe::egui::TextWrapMode::Extend)
        .fill(event_color.gamma_multiply(0.10))
        .stroke(eframe::egui::Stroke::new(
            1.0_f32,
            event_color.gamma_multiply(0.45),
        ))
        .corner_radius(3.0);
        if ui
            .add(event_btn)
            .on_hover_text("Create a named interval annotation spanning [start .. end]")
            .clicked()
        {
            open_new_interval = true;
        }

        if is_comparing {
            let names: Vec<String> = router
                .query_annotations_in_range(i64::MIN / 2..=i64::MAX / 2)
                .unwrap_or_default()
                .into_iter()
                .filter(|a| selected_ids.contains(&a.id))
                .map(|a| a.name)
                .collect();
            let compare_label = if names.is_empty() {
                format!("Comparing {} Events  ×", selected_ids.len())
            } else {
                format!("Comparing: {}  ×", names.join(" vs "))
            };
            let cmp_pill = eframe::egui::Button::new(
                eframe::egui::RichText::new(compare_label)
                    .color(event_color)
                    .small()
                    .strong(),
            )
            .wrap_mode(eframe::egui::TextWrapMode::Extend)
            .fill(event_color.gamma_multiply(0.20))
            .stroke(eframe::egui::Stroke::new(1.0_f32, event_color))
            .corner_radius(4.0);
            if ui
                .add(cmp_pill)
                .on_hover_text("Click to exit Comparison Mode")
                .clicked()
            {
                clear_comparison = true;
            }
        }

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(2.0);

        // =====================================================================
        // GROUP 4: Viewport Window, Zoom & Gap Folding
        // =====================================================================
        let timeline = router.timeline_mut();
        let mut span = timeline.window_secs();
        ui.label(
            eframe::egui::RichText::new("Win:")
                .small()
                .color(theme.muted_text_color()),
        );
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
            .small_button("−")
            .on_hover_text("Zoom out 2x around playhead")
            .clicked()
        {
            timeline.zoom_around_ms(timeline.setpoint_utc_ms(), 2.0);
        }
        if ui
            .small_button("+")
            .on_hover_text("Zoom in 2x around playhead")
            .clicked()
        {
            timeline.zoom_around_ms(timeline.setpoint_utc_ms(), 0.5);
        }
        if let Some((day_min, day_max)) = timeline.day_bounds_ms()
            && ui
                .small_button("↔ Fit Day")
                .on_hover_text("Zoom out to the full Track Day overview")
                .clicked()
        {
            timeline.snap_to_range_ms(day_min, day_max);
        }

        let mut fold_gaps = timeline.collapse_idle_gaps();
        let fold_btn = eframe::egui::Button::new(
            eframe::egui::RichText::new("Fold Gaps")
                .size(10.5)
                .color(if fold_gaps {
                    accent_color
                } else {
                    theme.muted_text_color()
                }),
        )
        .wrap_mode(eframe::egui::TextWrapMode::Extend)
        .fill(if fold_gaps {
            accent_color.gamma_multiply(0.18)
        } else {
            bg_color
        })
        .stroke(eframe::egui::Stroke::new(
            1.0_f32,
            if fold_gaps {
                accent_color.gamma_multiply(0.65)
            } else {
                border_color
            },
        ))
        .corner_radius(3.0);
        if ui
            .add(fold_btn)
            .on_hover_text(
                "Collapse unlogged intervals (>= 60s) into thin dividers so active stints fill the horizontal ribbon",
            )
            .clicked()
        {
            fold_gaps = !fold_gaps;
            timeline.set_collapse_idle_gaps(fold_gaps);
        }
    });

    if let Some((id, s, e)) = menu_single_click {
        router.timeline_mut().select_single_annotation(id, s, e);
    }
    if let Some((id, s, e)) = menu_shift_click {
        router.timeline_mut().toggle_comparison_annotation(id, s, e);
    }
    if let Some(ann) = menu_edit_ann {
        timeline_ui.open_edit(&ann);
    }
    if let Some(del_id) = menu_delete_id {
        let now_ms = daqcore::time::Time::now().unix_millis();
        if let Err(err) = router.soft_delete_annotation(&del_id, now_ms) {
            *diagnostic = Some(format!("Failed to delete annotation: {err}"));
        }
    }
    if let Some(fault_ms) = menu_jump_fault_ms {
        let tl = router.timeline_mut();
        let half_win = (tl.window_ms() / 2).clamp(2_500, 10_000);
        tl.snap_to_range_ms(
            fault_ms.saturating_sub(half_win),
            fault_ms.saturating_add(half_win),
        );
        tl.set_setpoint(daqcore::time::Time::from_unix_millis(fault_ms));
    }

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

    let lane_height = 28.0;
    let width = ui.available_width().max(16.0);
    let (rect, lane_response) = ui.allocate_exact_size(
        eframe::egui::vec2(width, lane_height),
        eframe::egui::Sense::click_and_drag(),
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
        let env_bottom = rect.bottom() - 1.5;
        let env_max_h = 12.0;
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

    // 6. Render Stints/Events (top band, true time width — never inflated or falsely merged)
    //    and Fault Onset Ticks (bottom 8px strip — crisp 2px ticks that never pile into a giant bar)
    let mut interacted_with_overlay = false;
    let mut pending_single_click: Option<(String, i64, Option<i64>)> = None;
    let mut pending_shift_click: Option<(String, i64, Option<i64>)> = None;
    let mut pending_edit: Option<daqcore::storage::Annotation> = None;
    let mut pending_delete_id: Option<String> = None;

    let opaque_pill_fill = |c: eframe::egui::Color32, selected: bool| -> eframe::egui::Color32 {
        let mix = if selected { 0.68_f32 } else { 0.45_f32 };
        let bg = [22.0_f32, 26.0_f32, 34.0_f32];
        let r = (bg[0] * (1.0 - mix) + (c.r() as f32) * mix).round() as u8;
        let g = (bg[1] * (1.0 - mix) + (c.g() as f32) * mix).round() as u8;
        let b = (bg[2] * (1.0 - mix) + (c.b() as f32) * mix).round() as u8;
        eframe::egui::Color32::from_rgb(r, g, b)
    };

    let r_left = rect.left();
    let r_right = rect.right().max(r_left);

    if timeline_ui.show_stints {
        let mut stints: Vec<&daqcore::storage::Annotation> = annotations
            .iter()
            .filter(|a| a.category != "fault")
            .collect();
        stints.sort_by_key(|a| (a.start_utc_ms, a.end_utc_ms.unwrap_or(a.start_utc_ms)));

        for (stint_idx, ann) in stints.into_iter().enumerate() {
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

            pill_resp.context_menu(|ui| {
                ui.label(eframe::egui::RichText::new(&ann.name).strong());
                ui.separator();
                if ui.button("Snap Viewport to Stint/Event").clicked() {
                    pending_single_click = Some((ann.id.clone(), ann.start_utc_ms, ann.end_utc_ms));
                    ui.close();
                }
                if ui.button("Toggle Comparison Mode (Shift+Click)").clicked() {
                    pending_shift_click = Some((ann.id.clone(), ann.start_utc_ms, ann.end_utc_ms));
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
        }
    }

    // Fault Onset Ticks along the bottom strip (`rect.bottom() - 10.0 .. rect.bottom() - 2.0`):
    // Crisp 2px red ticks quantized by horizontal pixel column so 189 faults never pile into a wide block.
    if timeline_ui.show_faults {
        let fault_y_top = rect.bottom() - 10.0;
        let fault_y_bot = rect.bottom() - 2.0;
        let fault_color = eframe::egui::Color32::from_rgb(239, 68, 68);
        let fx_min = (r_left + 1.0).min(r_right);
        let fx_max = (r_right - 1.0).max(fx_min);
        let mut last_tick_px: Option<i32> = None;
        for ann in annotations.iter().filter(|a| a.category == "fault") {
            let fx = time_to_x(ann.start_utc_ms).clamp(fx_min, fx_max);
            let px_col = (fx.round() as i32) / 2;
            if last_tick_px == Some(px_col) {
                continue;
            }
            last_tick_px = Some(px_col);
            painter.line_segment(
                [
                    eframe::egui::pos2(fx, fault_y_top),
                    eframe::egui::pos2(fx, fault_y_bot),
                ],
                eframe::egui::Stroke::new(2.0_f32, fault_color),
            );
        }
    }

    // 7. Draw Active Viewport `[start .. end]` with draggable Left/Right edge grips on the Day Overview lane
    let vp_left = time_to_x(router.timeline().start_utc_ms()).clamp(r_left, r_right);
    let vp_right = time_to_x(router.timeline().end_utc_ms())
        .clamp(r_left, r_right)
        .max((vp_left + 4.0).min(r_right));
    let vp_rect = eframe::egui::Rect::from_min_max(
        eframe::egui::pos2(vp_left, rect.top() + 1.0),
        eframe::egui::pos2(vp_right, rect.bottom() - 1.0),
    );
    painter.rect_filled(
        vp_rect,
        2.0,
        eframe::egui::Color32::from_rgba_unmultiplied(255, 255, 255, 24),
    );
    painter.rect_stroke(
        vp_rect,
        2.0,
        eframe::egui::Stroke::new(1.5_f32, eframe::egui::Color32::WHITE),
        eframe::egui::StrokeKind::Inside,
    );
    // Highlight left and right viewport resize grips
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
        .on_hover_text("Drag to adjust window start time");
    let right_grip_resp = ui
        .interact(
            right_grip_hit,
            ui.id().with("day_vp_right_grip"),
            eframe::egui::Sense::drag(),
        )
        .on_hover_cursor(eframe::egui::CursorIcon::ResizeHorizontal)
        .on_hover_text("Drag to adjust window end time");

    if left_grip_resp.dragged()
        && let Some(pointer) = left_grip_resp.interact_pointer_pos()
    {
        interacted_with_overlay = true;
        let tl = router.timeline_mut();
        let new_start =
            x_to_time(pointer.x).min(tl.end_utc_ms() - daqcore::timeline::MIN_WINDOW_MS);
        tl.set_start(daqcore::time::Time::from_unix_millis(new_start));
        let _ = tl.set_window_secs(tl.end().secs(tl.start()));
    } else if right_grip_resp.dragged()
        && let Some(pointer) = right_grip_resp.interact_pointer_pos()
    {
        interacted_with_overlay = true;
        let tl = router.timeline_mut();
        let new_end =
            x_to_time(pointer.x).max(tl.start_utc_ms() + daqcore::timeline::MIN_WINDOW_MS);
        tl.set_end(daqcore::time::Time::from_unix_millis(new_end));
        let _ = tl.set_window_secs(tl.end().secs(tl.start()));
    }

    let ph_x = time_to_x(router.timeline().setpoint_utc_ms()).clamp(r_left, r_right);
    painter.line_segment(
        [
            eframe::egui::pos2(ph_x, rect.top()),
            eframe::egui::pos2(ph_x, rect.bottom()),
        ],
        eframe::egui::Stroke::new(2.0_f32, eframe::egui::Color32::YELLOW),
    );

    // Subtle lane badge at top-left
    painter.text(
        eframe::egui::pos2(rect.left() + 4.0, rect.bottom() - 2.0),
        eframe::egui::Align2::LEFT_BOTTOM,
        "DAY OVERVIEW",
        eframe::egui::FontId::monospace(8.5),
        eframe::egui::Color32::from_gray(140),
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

    // Outer frame stroke
    painter.rect_stroke(
        rect,
        4.0,
        ui.visuals().widgets.noninteractive.bg_stroke,
        eframe::egui::StrokeKind::Inside,
    );

    // 8. Interactions on the Day Overview background (pan/jump, fault hover tooltip, scroll-wheel zoom)
    if let Some(hover_pos) = lane_response.hover_pos()
        && timeline_ui.show_faults
        && hover_pos.y >= rect.bottom() - 12.0
    {
        let mut nearby_faults: Vec<&daqcore::storage::Annotation> = annotations
            .iter()
            .filter(|a| {
                a.category == "fault" && (time_to_x(a.start_utc_ms) - hover_pos.x).abs() <= 5.0
            })
            .collect();
        nearby_faults.sort_by_key(|a| a.start_utc_ms);
        if !nearby_faults.is_empty() {
            let first_ms = nearby_faults[0].start_utc_ms;
            let mut unique_names: Vec<&str> = Vec::new();
            for f in &nearby_faults {
                let short = f.name.strip_prefix("Fault: ").unwrap_or(&f.name);
                if !unique_names.contains(&short) {
                    unique_names.push(short);
                }
            }
            let tip = format!(
                "⚠ {} fault transition(s) near {}:\n• {}\n(Click tick to jump playhead · Use '⚠ Faults' menu above to browse all faults)",
                nearby_faults.len(),
                daqcore::time::Time::from_unix_millis(first_ms).label(),
                unique_names
                    .into_iter()
                    .take(6)
                    .collect::<Vec<_>>()
                    .join("\n• "),
            );
            lane_response.clone().on_hover_text(tip);
            if !interacted_with_overlay && lane_response.clicked() {
                interacted_with_overlay = true;
                let span_ms =
                    (router.timeline().end_utc_ms() - router.timeline().start_utc_ms()).max(100);
                router
                    .timeline_mut()
                    .pan_to_start_ms(first_ms - span_ms / 2);
                router
                    .timeline_mut()
                    .set_setpoint(daqcore::time::Time::from_unix_millis(first_ms));
            }
        }
    }

    if !interacted_with_overlay
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
    timeline_ui: &TimelineUiState,
    accent_color: eframe::egui::Color32,
) {
    let win_start_ms = router.timeline().start_utc_ms();
    let win_end_ms = router.timeline().end_utc_ms().max(win_start_ms + 1);
    let win_span_ms = (win_end_ms - win_start_ms).max(1) as f64;
    let overlapping_annotations: Vec<daqcore::storage::Annotation> = router
        .query_annotations_in_range(win_start_ms..=win_end_ms)
        .unwrap_or_default();

    let scrub_height = 20.0;
    let width = ui.available_width().max(16.0);
    let (rect, background) = ui.allocate_exact_size(
        eframe::egui::vec2(width, scrub_height),
        eframe::egui::Sense::click_and_drag(),
    );
    let clip_rect = rect.intersect(ui.clip_rect());
    let painter = ui.painter_at(clip_rect);

    let theme = ui::theme::get_theme(ui.ctx());
    painter.rect_filled(rect, 3.0, theme.canvas_color());

    let pad_x = (rect.width() * 0.02).clamp(6.0, 20.0);
    let area = rect.shrink2(eframe::egui::vec2(pad_x, 0.0));
    let axis_y = area.bottom() - 12.0;

    let time_to_x = |utc_ms: i64| -> f32 {
        let frac = ((utc_ms - win_start_ms) as f64 / win_span_ms).clamp(0.0, 1.0) as f32;
        area.left() + frac * area.width()
    };
    let x_to_time = |px: f32| -> i64 {
        let frac = ((px - area.left()) / area.width().max(1.0)).clamp(0.0, 1.0) as f64;
        win_start_ms + (frac * win_span_ms).round() as i64
    };

    // Stint strip (top 3px) and Fault onset ticks (2px vertical ticks) inside the active window
    if timeline_ui.show_stints {
        for ann in overlapping_annotations
            .iter()
            .filter(|a| a.category != "fault")
        {
            let ann_end = ann.end_utc_ms.unwrap_or(ann.start_utc_ms);
            let x0 = time_to_x(ann.start_utc_ms.max(win_start_ms));
            let x1 = time_to_x(ann_end.min(win_end_ms)).max(x0 + 3.0);
            let color = parse_hex_color(&ann.color, accent_color);
            let strip_rect = eframe::egui::Rect::from_min_max(
                eframe::egui::pos2(x0, area.top() + 2.0),
                eframe::egui::pos2(x1, area.top() + 5.0),
            );
            painter.rect_filled(strip_rect, 1.5, color.gamma_multiply(0.75));
        }
    }
    if timeline_ui.show_faults {
        let fault_color = eframe::egui::Color32::from_rgb(239, 68, 68);
        let mut last_tick_px: Option<i32> = None;
        for ann in overlapping_annotations
            .iter()
            .filter(|a| a.category == "fault")
        {
            if ann.start_utc_ms < win_start_ms || ann.start_utc_ms > win_end_ms {
                continue;
            }
            let fx = time_to_x(ann.start_utc_ms);
            let px_col = (fx.round() as i32) / 2;
            if last_tick_px == Some(px_col) {
                continue;
            }
            last_tick_px = Some(px_col);
            painter.line_segment(
                [
                    eframe::egui::pos2(fx, area.top() + 2.0),
                    eframe::egui::pos2(fx, axis_y - 1.0),
                ],
                eframe::egui::Stroke::new(1.5_f32, fault_color),
            );
        }
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

    // Playhead (`setpoint`) indicator on the Active Window Scrub Bar.
    // Clicking or dragging anywhere on this bar smoothly scrubs the Playhead!
    let timeline = router.timeline_mut();
    let ph_px = time_to_x(timeline.setpoint_utc_ms());
    let ph_y = area.top() + 7.0;
    painter.line_segment(
        [
            eframe::egui::pos2(ph_px, rect.top() + 1.0),
            eframe::egui::pos2(ph_px, rect.bottom() - 1.0),
        ],
        eframe::egui::Stroke::new(2.0_f32, eframe::egui::Color32::YELLOW),
    );
    painter.circle_filled(
        eframe::egui::pos2(ph_px, ph_y),
        5.0,
        eframe::egui::Color32::YELLOW,
    );

    if (background.clicked() || background.dragged())
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

    background.on_hover_text(
        "Click or drag anywhere on this ruler to scrub the yellow Playhead · Scroll wheel to zoom",
    );

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

/// Floating window wrapper for the Calendar & Subchunk Navigator (`📅 Browse Chunks`).
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
    eframe::egui::Window::new("📅 Calendar & Subchunk Navigator")
        .open(&mut open)
        .default_width(420.0)
        .default_height(460.0)
        .resizable(true)
        .collapsible(true)
        .show(ctx, |ui| {
            show_chunk_navigator_contents(ui, router, timeline_ui, diagnostic, 380.0);
        });
    timeline_ui.show_chunk_navigator = open;
}

/// Shared Calendar & Subchunk Navigator view (used in both the floating `📅 Browse Chunks` window
/// and the collapsible section in the Left Sidebar).
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
    ui.spacing_mut().item_spacing = eframe::egui::vec2(4.0, 2.0);

    // Search & Category Filter Header (right-to-left so the clear button deducts width first)
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
            let edit_w = (ui.available_width() - 10.0).max(40.0);
            ui.add(
                eframe::egui::TextEdit::singleline(&mut timeline_ui.chunk_search_query)
                    .id(eframe::egui::Id::new("chunk_navigator_search_input"))
                    .desired_width(edit_w)
                    .hint_text("Search date, stint, partition, fault, or note…"),
            );
        },
    );

    ui.horizontal_wrapped(|ui| {
        for (filter, lbl) in [
            (ChunkFilter::All, "All"),
            (ChunkFilter::StintsAndEvents, "Stints & Events"),
            (ChunkFilter::Partitions, "Partitions"),
            (ChunkFilter::Faults, "Faults"),
        ] {
            if ui
                .selectable_label(timeline_ui.chunk_filter == filter, lbl)
                .clicked()
            {
                timeline_ui.chunk_filter = filter;
            }
        }
    });
    ui.separator();

    let query = timeline_ui.chunk_search_query.trim().to_lowercase();
    let matches = |text: &str| -> bool { query.is_empty() || text.to_lowercase().contains(&query) };

    // Collect all Track Days, SD Partitions, and Annotations across the entire store
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
    let all_annotations: Vec<daqcore::storage::Annotation> = router
        .query_annotations_in_range(i64::MIN / 2..=i64::MAX / 2)
        .unwrap_or_default();

    let mut day_ids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for td in &track_days {
        day_ids.insert(td.id.clone());
    }
    for (day_id, _, _, _, _) in &all_partitions {
        day_ids.insert(day_id.clone());
    }
    for ann in &all_annotations {
        day_ids.insert(ann.track_day_id.clone());
    }

    if day_ids.is_empty() {
        ui.label(
            eframe::egui::RichText::new("No recorded sessions or annotations loaded yet.")
                .italics()
                .weak(),
        );
        return;
    }

    let cur_win_start = router.timeline().start_utc_ms();
    let cur_win_end = router.timeline().end_utc_ms();
    let selected_ann_ids: Vec<String> = router.timeline().selected_annotation_ids().to_vec();

    let mut req_select_day: Option<Option<String>> = None;
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
            let inner_max_w = (nav_max_w - 8.0).max(60.0);
            ui.set_max_width(inner_max_w);
            ui.style_mut().wrap_mode = Some(eframe::egui::TextWrapMode::Truncate);

            // Show newest track days first for fast multi-day navigation
            for day_id in day_ids.into_iter().rev() {
                let mut day_parts: Vec<&(String, String, i64, i64, usize)> = all_partitions
                    .iter()
                    .filter(|(d, p_id, s, e, _)| {
                        d == &day_id
                            && (timeline_ui.chunk_filter == ChunkFilter::All
                                || timeline_ui.chunk_filter == ChunkFilter::Partitions)
                            && matches(&format!(
                                "{day_id} {p_id} {} {}",
                                daqcore::time::Time::from_unix_millis(*s).label(),
                                daqcore::time::Time::from_unix_millis(*e).label()
                            ))
                    })
                    .collect();
                day_parts.sort_by_key(|(_, _, s, _, _)| *s);

                let mut day_stints: Vec<&daqcore::storage::Annotation> = all_annotations
                    .iter()
                    .filter(|a| {
                        a.track_day_id == day_id
                            && a.category != "fault"
                            && (timeline_ui.chunk_filter == ChunkFilter::All
                                || timeline_ui.chunk_filter == ChunkFilter::StintsAndEvents)
                            && matches(&format!(
                                "{day_id} {} {} {}",
                                a.name,
                                a.category,
                                a.notes
                            ))
                    })
                    .collect();
                day_stints
                    .sort_by_key(|a| (a.start_utc_ms, a.end_utc_ms.unwrap_or(a.start_utc_ms)));

                let mut day_faults: Vec<&daqcore::storage::Annotation> = all_annotations
                    .iter()
                    .filter(|a| {
                        a.track_day_id == day_id
                            && a.category == "fault"
                            && (timeline_ui.chunk_filter == ChunkFilter::All
                                || timeline_ui.chunk_filter == ChunkFilter::Faults)
                            && matches(&format!("{day_id} {}", a.name))
                    })
                    .collect();
                day_faults.sort_by_key(|a| a.start_utc_ms);

                if day_parts.is_empty() && day_stints.is_empty() && day_faults.is_empty() {
                    continue;
                }

                let day_header = format!(
                    "📅 {}  ({} stints · {} parts · {} faults)",
                    day_id,
                    day_stints.len(),
                    day_parts.len(),
                    day_faults.len()
                );

                eframe::egui::CollapsingHeader::new(
                    eframe::egui::RichText::new(day_header).strong(),
                )
                .default_open(true)
                .show(ui, |ui| {
                    ui.set_max_width(ui.available_width().max(40.0));
                    ui.style_mut().wrap_mode = Some(eframe::egui::TextWrapMode::Truncate);

                    ui.horizontal(|ui| {
                        if ui
                            .small_button("↔ Fit Entire Day")
                            .on_hover_text("Select this Track Day and zoom out to its full span")
                            .clicked()
                        {
                            req_select_day = Some(Some(day_id.clone()));
                        }
                    });

                    // 1. Driving Stints & Event Annotations (Primary Subchunks)
                    if !day_stints.is_empty() {
                        ui.label(
                            eframe::egui::RichText::new("Driving Stints & Events:")
                                .weak()
                                .small(),
                        );
                        for ann in &day_stints {
                            let ann_end = ann.end_utc_ms.unwrap_or(ann.start_utc_ms);
                            let dur_s = (ann_end - ann.start_utc_ms).max(0) as f64 / 1000.0;
                            let start_lbl =
                                daqcore::time::Time::from_unix_millis(ann.start_utc_ms).label();
                            let end_lbl = daqcore::time::Time::from_unix_millis(ann_end).label();
                            let is_active = selected_ann_ids.contains(&ann.id)
                                || ((cur_win_start - ann.start_utc_ms).abs() <= 1_000
                                    && (cur_win_end - ann_end).abs() <= 1_000);
                            let label = if dur_s > 0.0 {
                                format!("🏷 {} ({dur_s:.1}s · {start_lbl})", ann.name)
                            } else {
                                format!("📍 {} [{start_lbl}]", ann.name)
                            };
                            let range_summary = if dur_s > 0.0 {
                                format!("{}  [{start_lbl} .. {end_lbl} · {dur_s:.1}s]", ann.name)
                            } else {
                                format!("{}  [{start_lbl}]", ann.name)
                            };
                            let hover = if ann.notes.is_empty() {
                                format!("{range_summary}\nClick to jump viewport to this subchunk")
                            } else {
                                format!(
                                    "{range_summary}\n{}\nClick to jump viewport to this subchunk",
                                    ann.notes
                                )
                            };
                            let row_w = ui.available_width().max(60.0);
                            ui.allocate_ui_with_layout(
                                eframe::egui::vec2(row_w, 20.0),
                                eframe::egui::Layout::right_to_left(eframe::egui::Align::Center),
                                |ui| {
                                    ui.set_max_width(row_w);
                                    if ui
                                        .small_button("×")
                                        .on_hover_text("Delete annotation")
                                        .clicked()
                                    {
                                        req_delete_ann_id = Some(ann.id.clone());
                                    }
                                    if ui
                                        .small_button("Edit")
                                        .on_hover_text("Edit annotation")
                                        .clicked()
                                    {
                                        req_edit_ann = Some((*ann).clone());
                                    }
                                    if ui
                                        .small_button(if selected_ann_ids.contains(&ann.id) {
                                            "-Cmp"
                                        } else {
                                            "+Cmp"
                                        })
                                        .on_hover_text(
                                            "Toggle multi-stint Comparison / Overlay Mode",
                                        )
                                        .clicked()
                                    {
                                        req_compare_ann =
                                            Some((ann.id.clone(), ann.start_utc_ms, ann.end_utc_ms));
                                    }
                                    let rem_w = ui.available_width().max(20.0);
                                    ui.allocate_ui_with_layout(
                                        eframe::egui::vec2(rem_w, 20.0),
                                        eframe::egui::Layout::left_to_right(
                                            eframe::egui::Align::Center,
                                        ),
                                        |ui| {
                                            ui.set_max_width(rem_w);
                                            ui.style_mut().wrap_mode =
                                                Some(eframe::egui::TextWrapMode::Truncate);
                                            if ui
                                                .selectable_label(is_active, label)
                                                .on_hover_text(hover)
                                                .clicked()
                                            {
                                                req_single_ann = Some((
                                                    day_id.clone(),
                                                    ann.id.clone(),
                                                    ann.start_utc_ms,
                                                    ann.end_utc_ms,
                                                ));
                                            }
                                        },
                                    );
                                },
                            );
                        }
                    }

                    // 2. Recorded SD Log Partitions (Session Subchunks)
                    if !day_parts.is_empty() {
                        ui.label(
                            eframe::egui::RichText::new("Recorded Log Partitions (SD Chunks):")
                                .weak()
                                .small(),
                        );
                        for (_, part_id, p_start, p_end, sig_count) in &day_parts {
                            let dur_s = (*p_end - *p_start).max(0) as f64 / 1000.0;
                            let start_lbl = daqcore::time::Time::from_unix_millis(*p_start).label();
                            let end_lbl = daqcore::time::Time::from_unix_millis(*p_end).label();
                            let is_active = (cur_win_start - *p_start).abs() <= 1_000
                                && (cur_win_end - *p_end).abs() <= 1_000;
                            let lbl = format!(
                                "• {start_lbl} .. {end_lbl}  ({dur_s:.0}s · {sig_count} signals)"
                            );
                            if ui
                                .selectable_label(is_active, lbl)
                                .on_hover_text(format!(
                                    "Partition {part_id}\nClick to snap viewport to this recorded partition"
                                ))
                                .clicked()
                            {
                                req_snap_range = Some((day_id.clone(), *p_start, *p_end));
                            }
                        }
                    }

                    // 3. Grouped Faults in this Track Day
                    if !day_faults.is_empty() {
                        let mut grouped: Vec<(String, Vec<&daqcore::storage::Annotation>)> =
                            Vec::new();
                        for f in &day_faults {
                            let short = f
                                .name
                                .strip_prefix("Fault: ")
                                .unwrap_or(&f.name)
                                .to_owned();
                            if let Some((_, list)) = grouped.iter_mut().find(|(k, _)| k == &short) {
                                list.push(*f);
                            } else {
                                grouped.push((short, vec![*f]));
                            }
                        }
                        ui.collapsing(
                            format!(
                                "⚠ Faults ({} types, {} transitions)",
                                grouped.len(),
                                day_faults.len()
                            ),
                            |ui| {
                                for (fault_name, occurrences) in grouped {
                                    let first_ms = occurrences[0].start_utc_ms;
                                    let first_lbl =
                                        daqcore::time::Time::from_unix_millis(first_ms).label();
                                    ui.horizontal(|ui| {
                                        if ui
                                            .small_button(format!(
                                                "⚠ {} ({}×, 1st @ {})",
                                                fault_name,
                                                occurrences.len(),
                                                first_lbl
                                            ))
                                            .on_hover_text(
                                                "Click to jump 10s window & playhead to first occurrence",
                                            )
                                            .clicked()
                                        {
                                            req_jump_fault = Some((day_id.clone(), first_ms));
                                        }
                                    });
                                }
                            },
                        );
                    }
                });
            }
        });

    if let Some(day_opt) = req_select_day {
        if let Err(err) = router.select_track_day(day_opt) {
            *diagnostic = Some(format!("Failed to select track day: {err}"));
        } else if let Some((lo, hi)) = router.timeline().day_bounds_ms() {
            router.timeline_mut().snap_to_range_ms(lo, hi);
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
        let tl = router.timeline_mut();
        let half_win = (tl.window_ms() / 2).clamp(2_500, 10_000);
        tl.snap_to_range_ms(
            fault_ms.saturating_sub(half_win),
            fault_ms.saturating_add(half_win),
        );
        tl.set_setpoint(daqcore::time::Time::from_unix_millis(fault_ms));
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
                    &mut router,
                    &mut action_queue,
                    &mut diagnostic,
                );
            });
            eframe::egui::CentralPanel::default().show(ctx, |ui| {
                let view = crate::workspace_view::WorkspaceDataView::new(&router);
                let _ = scope.show(ui, &view);
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
