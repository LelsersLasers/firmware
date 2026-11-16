use crate::{action, app, ui, widget_constructor, widgets, workspace_view};
use std::sync::mpsc;

fn short_preset_tab_name<'a>(preset_id: &str, full_name: &'a str) -> &'a str {
    match preset_id {
        "vd_handling_suspension" => "VEHICLE DYNAMICS",
        "powertrain_accumulator" => "POWERTRAIN",
        "thermals_cooling" => "THERMALS",
        "aero_ride_height" => "AERODYNAMICS",
        "sensors_controls" => "SENSORS",
        "trackside_live_debug" => "LIVE DEBUG",
        _ => full_name,
    }
}

fn show_workspace_tabs_bar(app: &mut app::DAQApp, ctx: &eframe::egui::Context) {
    let theme = ui::theme::get_theme(ctx);
    let border_color = theme.border_color();
    let accent_color = theme.accent_color();
    let muted_color = theme.muted_text_color();

    let bar_frame = eframe::egui::Frame::side_top_panel(&ctx.style())
        .inner_margin(eframe::egui::Margin::symmetric(6, 2))
        .stroke(eframe::egui::Stroke::new(1.0_f32, border_color));

    eframe::egui::TopBottomPanel::top("workspace_preset_tabs_bar")
        .exact_height(24.0)
        .frame(bar_frame)
        .show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;

                let preset_summaries: Vec<(String, String, String)> = app
                    .workspace_presets
                    .presets()
                    .iter()
                    .map(|p| {
                        (
                            p.id.clone().unwrap_or_else(|| p.name.clone()),
                            p.name.clone(),
                            p.description.clone(),
                        )
                    })
                    .collect();

                for (preset_id, preset_name, preset_desc) in preset_summaries {
                    let is_active = app.active_workspace_tab.as_deref() == Some(preset_id.as_str());
                    let tab_label = short_preset_tab_name(&preset_id, &preset_name);
                    let text_color = if is_active {
                        ui.visuals().text_color()
                    } else {
                        muted_color
                    };
                    let fill = if is_active {
                        theme.canvas_color()
                    } else {
                        eframe::egui::Color32::TRANSPARENT
                    };
                    let stroke = if is_active {
                        eframe::egui::Stroke::new(1.0_f32, accent_color)
                    } else {
                        eframe::egui::Stroke::NONE
                    };

                    let btn = eframe::egui::Button::new(
                        eframe::egui::RichText::new(tab_label)
                            .size(10.5)
                            .strong()
                            .color(text_color),
                    )
                    .fill(fill)
                    .stroke(stroke);

                    let resp = ui
                        .add(btn)
                        .on_hover_cursor(eframe::egui::CursorIcon::PointingHand)
                        .on_hover_text(preset_desc);
                    if resp.hovered() && !is_active {
                        ui.painter()
                            .rect_filled(resp.rect, 3.0, accent_color.gamma_multiply(0.14));
                        ui.painter().rect_stroke(
                            resp.rect,
                            3.0,
                            eframe::egui::Stroke::new(1.0_f32, accent_color.gamma_multiply(0.65)),
                            eframe::egui::StrokeKind::Inside,
                        );
                    }
                    if resp.clicked() {
                        app.action_queue
                            .push(action::AppAction::LoadWorkspacePreset {
                                preset_id,
                                apply_time_context: app
                                    .workspace_view_modal
                                    .jump_to_saved_time_range,
                            });
                    }
                }

                ui.separator();

                // Nominal-style `+` button to add a widget pane directly from the tab bar
                ui.menu_button(
                    eframe::egui::RichText::new("+")
                        .size(12.0)
                        .strong()
                        .color(muted_color),
                    |ui| {
                        ui.set_min_width(170.0);
                        ui.label(
                            eframe::egui::RichText::new("Add Analysis Pane")
                                .small()
                                .weak(),
                        );
                        for (label, ctor) in [
                            (
                                "Scope (Time / Distance)",
                                widget_constructor::WidgetConstructor::ScopeEmpty,
                            ),
                            (
                                "XY Scatter",
                                widget_constructor::WidgetConstructor::XYScatter,
                            ),
                            (
                                "State & Fault Lanes",
                                widget_constructor::WidgetConstructor::StateLane,
                            ),
                            (
                                "GPS Track Map",
                                widget_constructor::WidgetConstructor::GpsPlot,
                            ),
                        ] {
                            if ui.button(label).clicked() {
                                app.action_queue.push(action::AppAction::SpawnWidget(ctor));
                                ui.close();
                            }
                        }
                        ui.separator();
                        ui.label(
                            eframe::egui::RichText::new("Operational Tools")
                                .small()
                                .weak(),
                        );
                        for (label, ctor) in [
                            (
                                "CAN Viewer Table",
                                widget_constructor::WidgetConstructor::ViewerTable,
                            ),
                            (
                                "CAN Viewer List",
                                widget_constructor::WidgetConstructor::ViewerList,
                            ),
                            ("Bus Load", widget_constructor::WidgetConstructor::BusLoad),
                            (
                                "Jitter Histogram",
                                widget_constructor::WidgetConstructor::Jitter,
                            ),
                            (
                                "Message Sender",
                                widget_constructor::WidgetConstructor::SendUi,
                            ),
                            ("HIL Runner", widget_constructor::WidgetConstructor::Hil),
                            (
                                "Bootloader",
                                widget_constructor::WidgetConstructor::Bootloader,
                            ),
                            (
                                "SD Log Importer",
                                widget_constructor::WidgetConstructor::LogParser,
                            ),
                        ] {
                            if ui.button(label).clicked() {
                                app.action_queue.push(action::AppAction::SpawnWidget(ctor));
                                ui.close();
                            }
                        }
                    },
                )
                .response
                .on_hover_text("Add a new plot or tool pane to the workspace");

                let has_views = !app.tile_tree.is_empty();
                let clear_btn = eframe::egui::Button::new(
                    eframe::egui::RichText::new("× Clear")
                        .size(10.5)
                        .strong()
                        .color(if has_views {
                            muted_color
                        } else {
                            muted_color.gamma_multiply(0.45)
                        }),
                )
                .fill(eframe::egui::Color32::TRANSPARENT)
                .stroke(eframe::egui::Stroke::NONE);

                if ui
                    .add_enabled(has_views, clear_btn)
                    .on_hover_text("Clear workspace of all opened views")
                    .clicked()
                {
                    app.action_queue.push(action::AppAction::ClearWorkspace);
                }
            });
        });
}

pub fn show(app: &mut app::DAQApp, ctx: &eframe::egui::Context) {
    ui::timeline::show(app, ctx);
    show_workspace_tabs_bar(app, ctx);
    app.router.poll_background_tiles();
    let start = app.router.timeline().start();
    let removed = app
        .bus_load_samples
        .partition_point(|s| s.timestamp < start);
    app.bus_load_samples.drain(..removed);
    let selection_pending = app.selection_pending();
    let view = workspace_view::WorkspaceDataView::with_hover(
        &app.router,
        app.hover_utc_ms,
        app.hover_distance_m,
    );
    let theme = ui::theme::get_theme(ctx);
    let central_frame = eframe::egui::Frame::central_panel(&ctx.style())
        .fill(theme.border_color())
        .inner_margin(eframe::egui::Margin::same(1));
    eframe::egui::CentralPanel::default()
        .frame(central_frame)
        .show(ctx, |ui| {
            if app.tile_tree.is_empty() {
                eframe::egui::Frame::NONE
                    .fill(theme.canvas_color())
                    .show(ui, |ui| {
                        ui.set_min_size(ui.available_size());
                        ui.vertical_centered(|ui| {
                            ui.add_space((ui.available_height() * 0.35).max(16.0));
                            ui.label(
                                eframe::egui::RichText::new("No panes in active workspace")
                                    .strong(),
                            );
                            ui.label(
                                eframe::egui::RichText::new(
                                    "Select a subteam tab above, drag a channel from CHANNELS, or press Ctrl+P.",
                                )
                                .small()
                                .color(theme.muted_text_color()),
                            );
                        });
                    });
            } else {
                let mut behavior = WorkspaceTileBehavior {
                    view: &view,
                    bus_load: &app.bus_load_samples,
                    action_queue: &mut app.action_queue,
                    database: app.database.as_ref(),
                    ui_to_can_tx: app.ui_to_can_tx.clone(),
                    formatter: &app.value_formatter,
                    canvas_color: theme.canvas_color(),
                    accent_color: theme.accent_color(),
                    muted_color: theme.muted_text_color(),
                };
                ui.add_enabled_ui(!selection_pending, |ui| {
                    app.tile_tree.ui(&mut behavior, ui);
                });
            }

            let workspace_rect = ui.max_rect();
            if eframe::egui::DragAndDrop::payload::<ui::signal_browser::DraggedSignal>(ctx)
                .is_some()
                && ui.rect_contains_pointer(workspace_rect)
            {
                ui.painter().rect_stroke(
                    workspace_rect.shrink(1.0),
                    4.0,
                    eframe::egui::Stroke::new(
                        1.5_f32,
                        eframe::egui::Color32::from_rgba_unmultiplied(80, 190, 255, 160),
                    ),
                    eframe::egui::StrokeKind::Inside,
                );
                if ui.input(|i| i.pointer.any_released())
                    && let Some(payload) = eframe::egui::DragAndDrop::take_payload::<
                        ui::signal_browser::DraggedSignal,
                    >(ctx)
                {
                    app.action_queue.push(action::AppAction::SpawnWidget(
                        payload.to_widget_constructor(),
                    ));
                }
            }
        });

    let hover_touched = view.hover_touched();
    let next_hover_ms = view.hover_utc_ms();
    let next_hover_dist = view.hover_distance_m();
    let requested_setpoint_ms = view.requested_setpoint_utc_ms();
    let requested_zoom = view.requested_zoom();
    let requested_pan_delta_ms = view.requested_pan_delta_ms();

    if hover_touched {
        app.hover_utc_ms = next_hover_ms;
        app.hover_distance_m = next_hover_dist;
    } else {
        app.hover_utc_ms = None;
        app.hover_distance_m = None;
    }
    if let Some((anchor_ms, zoom_factor)) = requested_zoom {
        app.router
            .timeline_mut()
            .zoom_around_ms(anchor_ms, zoom_factor);
    }
    if let Some(delta_ms) = requested_pan_delta_ms {
        let next_start_ms = app
            .router
            .timeline()
            .start_utc_ms()
            .saturating_add(delta_ms);
        app.router.timeline_mut().pan_to_start_ms(next_start_ms);
    }
    if let Some(setpoint_ms) = requested_setpoint_ms {
        app.router
            .timeline_mut()
            .set_setpoint(daqcore::time::Time::from_unix_millis(setpoint_ms));
    }
}

struct WorkspaceTileBehavior<'a> {
    view: &'a workspace_view::WorkspaceDataView<'a>,
    bus_load: &'a [workspace_view::BusLoadSample],
    action_queue: &'a mut Vec<action::AppAction>,
    database: Option<&'a app::DatabaseSelection>,
    ui_to_can_tx: mpsc::Sender<daqcore::can_thread::CanThreadCommand>,
    formatter: &'a Option<daqcore::formatter::Formatter>,
    canvas_color: eframe::egui::Color32,
    accent_color: eframe::egui::Color32,
    muted_color: eframe::egui::Color32,
}

impl egui_tiles::Behavior<widgets::Widget> for WorkspaceTileBehavior<'_> {
    fn pane_ui(
        &mut self,
        ui: &mut eframe::egui::Ui,
        _tile_id: egui_tiles::TileId,
        widget: &mut widgets::Widget,
    ) -> egui_tiles::UiResponse {
        ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));
        let canvas_color = ui::theme::get_theme(ui.ctx()).canvas_color();
        eframe::egui::Frame::NONE
            .fill(canvas_color)
            .inner_margin(eframe::egui::Margin::symmetric(4, 2))
            .show(ui, |ui| {
                ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));
                widget.show(
                    ui,
                    widgets::WidgetContext {
                        view: self.view,
                        bus_load: self.bus_load,
                        action_queue: self.action_queue,
                        database: self.database,
                        ui_to_can_tx: self.ui_to_can_tx.clone(),
                        formatter: self.formatter,
                    },
                )
            })
            .inner
    }

    fn tab_bar_height(&self, _style: &eframe::egui::Style) -> f32 {
        20.0
    }

    fn gap_width(&self, _style: &eframe::egui::Style) -> f32 {
        1.0
    }

    fn tab_title_for_pane(&mut self, widget: &widgets::Widget) -> eframe::egui::WidgetText {
        widget.title().into()
    }

    fn tab_bar_color(&self, visuals: &eframe::egui::Visuals) -> eframe::egui::Color32 {
        visuals.faint_bg_color
    }

    fn tab_bg_color(
        &self,
        visuals: &eframe::egui::Visuals,
        _tiles: &egui_tiles::Tiles<widgets::Widget>,
        _tile_id: egui_tiles::TileId,
        state: &egui_tiles::TabState,
    ) -> eframe::egui::Color32 {
        if state.active {
            self.canvas_color
        } else if state.is_being_dragged {
            visuals.widgets.active.bg_fill
        } else {
            eframe::egui::Color32::TRANSPARENT
        }
    }

    fn tab_outline_stroke(
        &self,
        _visuals: &eframe::egui::Visuals,
        _tiles: &egui_tiles::Tiles<widgets::Widget>,
        _tile_id: egui_tiles::TileId,
        state: &egui_tiles::TabState,
    ) -> eframe::egui::Stroke {
        if state.active {
            eframe::egui::Stroke::new(1.5_f32, self.accent_color)
        } else {
            eframe::egui::Stroke::NONE
        }
    }

    fn tab_text_color(
        &self,
        visuals: &eframe::egui::Visuals,
        _tiles: &egui_tiles::Tiles<widgets::Widget>,
        _tile_id: egui_tiles::TileId,
        state: &egui_tiles::TabState,
    ) -> eframe::egui::Color32 {
        if state.active {
            visuals.strong_text_color()
        } else {
            self.muted_color
        }
    }

    fn tab_hover_cursor_icon(&self) -> eframe::egui::CursorIcon {
        eframe::egui::CursorIcon::PointingHand
    }

    fn simplification_options(&self) -> egui_tiles::SimplificationOptions {
        egui_tiles::SimplificationOptions {
            all_panes_must_have_tabs: true,
            ..Default::default()
        }
    }

    fn is_tab_closable(
        &self,
        _tiles: &egui_tiles::Tiles<widgets::Widget>,
        _tile_id: egui_tiles::TileId,
    ) -> bool {
        true
    }

    fn on_tab_button(
        &mut self,
        tiles: &egui_tiles::Tiles<widgets::Widget>,
        tile_id: egui_tiles::TileId,
        button_response: eframe::egui::Response,
    ) -> eframe::egui::Response {
        if let Some(egui_tiles::Tile::Pane(widget)) = tiles.get(tile_id) {
            let current_title = widget.title().to_owned();
            let has_custom_title = widget.has_custom_title();
            let mut pending_title: Option<String> = None;
            let mut reset_auto = false;
            let mut close_pane = false;

            let just_opened = button_response.secondary_clicked();
            eframe::egui::Popup::context_menu(&button_response)
                .close_behavior(eframe::egui::PopupCloseBehavior::CloseOnClickOutside)
                .sense(eframe::egui::Sense::click_and_drag())
                .show(|ui| {
                    ui.set_min_width(195.0);
                    ui.label(eframe::egui::RichText::new("Tab Title").weak().small());
                    let mut draft = current_title.clone();
                    ui.horizontal(|ui| {
                        let resp = ui.add(
                            eframe::egui::TextEdit::singleline(&mut draft)
                                .desired_width(140.0)
                                .hint_text("Tab title…"),
                        );
                        if just_opened {
                            resp.request_focus();
                        }
                        if resp.changed() {
                            pending_title = Some(draft.clone());
                        }
                        if resp.lost_focus() {
                            if draft.trim().is_empty() {
                                reset_auto = true;
                            }
                            if ui.input(|i| i.key_pressed(eframe::egui::Key::Enter)) {
                                ui.close();
                            }
                        }
                        if has_custom_title
                            && ui
                                .small_button("Auto")
                                .on_hover_text("Reset to automatic title")
                                .clicked()
                        {
                            reset_auto = true;
                            ui.close();
                        }
                    });
                    ui.separator();
                    if ui.button("× Close View").clicked() {
                        close_pane = true;
                        ui.close();
                    }
                });

            if reset_auto {
                self.action_queue
                    .push(action::AppAction::ResetPaneTitle { tile_id });
            } else if let Some(new_title) = pending_title {
                self.action_queue.push(action::AppAction::RenamePane {
                    tile_id,
                    title: new_title,
                });
            }
            if close_pane {
                self.action_queue
                    .push(action::AppAction::ClosePane { tile_id });
            }
        }
        button_response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ui, widget_constructor, widget_ids};
    use std::path::PathBuf;

    fn sample_database() -> Option<app::DatabaseSelection> {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let dbc_path = manifest_dir.join("../../superdbc/superdbc_per26.json");
        let database = daqcore::superdbc::database::Database::load(&dbc_path).ok()?;
        let selection = daqcore::can_thread::BusSelection::default_single(&database)
            .unwrap_or(daqcore::can_thread::BusSelection::Single(0));
        Some(app::DatabaseSelection {
            database,
            selection,
        })
    }

    fn build_all_widgets(
        db: Option<&app::DatabaseSelection>,
        tx: mpsc::Sender<daqcore::can_thread::CanThreadCommand>,
    ) -> Vec<widgets::Widget> {
        let mut ids = widget_ids::WidgetIds::new();
        let constructors = [
            widget_constructor::WidgetConstructor::ViewerTable,
            widget_constructor::WidgetConstructor::ViewerList,
            widget_constructor::WidgetConstructor::Bootloader,
            widget_constructor::WidgetConstructor::ScopeEmpty,
            widget_constructor::WidgetConstructor::StateLane,
            widget_constructor::WidgetConstructor::XYScatter,
            widget_constructor::WidgetConstructor::LogParser,
            widget_constructor::WidgetConstructor::SendUi,
            widget_constructor::WidgetConstructor::BusLoad,
            widget_constructor::WidgetConstructor::GpsPlot,
            widget_constructor::WidgetConstructor::Jitter,
            widget_constructor::WidgetConstructor::Hil,
        ];
        let mut list: Vec<widgets::Widget> = constructors
            .into_iter()
            .flat_map(|c| c.create_all(&mut ids, tx.clone(), 0))
            .collect();

        if let Some(db) = db
            && let Some(msg) = db.database.messages().find(|m| !m.signals().is_empty())
            && let Some(sig) = msg.signals().first()
            && let Some(configured_scope) = (widget_constructor::WidgetConstructor::Scope {
                key: msg.key(),
                msg_name: msg.name().to_owned(),
                signal_name: sig.name().to_owned(),
            })
            .create(&mut ids, tx, 1)
        {
            list.push(configured_scope);
        }
        list
    }

    #[test]
    fn all_widgets_render_cleanly_across_extreme_tile_resizes() {
        let t0 = daqcore::time::Time::from_unix_millis(1_794_500_000_000);
        let mut router =
            daqcore::router::VirtualTimelineRouter::live(t0, 30.0).expect("live router");
        let db = sample_database();

        if let Some(db_sel) = &db {
            for step in 0..10 {
                let ts = t0.offset(step * 100);
                for msg in db_sel.database.messages().take(8) {
                    let key = msg.key();
                    let raw = vec![0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0];
                    let decoded = db_sel.database.decode(key.bus_id, key.identity, &raw).ok();
                    router.ingest_frame(daqcore::frame::ParsedFrame {
                        timestamp: ts,
                        bus_id: key.bus_id,
                        identity: key.identity,
                        kind: daqcore::frame::FrameKind::Data,
                        dlc: 8,
                        raw_bytes: raw,
                        decoded,
                    });
                }
            }
        }

        let bus_load = vec![workspace_view::BusLoadSample {
            timestamp: t0.offset(500),
            buses: vec![daqcore::can::bus_load::BusLoadReading {
                bus_id: 0,
                bus_name: "Main".to_owned(),
                values: [14.2, 12.8, 11.5, 10.1],
            }],
        }];
        let formatter = app::load_formatter();
        let (tx, _rx) = mpsc::channel();

        let sizes = [
            eframe::egui::vec2(200.0, 700.0),  // Narrow & tall
            eframe::egui::vec2(900.0, 120.0),  // Wide & short
            eframe::egui::vec2(160.0, 110.0),  // Tiny tile
            eframe::egui::vec2(1280.0, 800.0), // Full size
        ];

        let mut all_widgets = build_all_widgets(db.as_ref(), tx.clone());
        assert!(all_widgets.len() >= 12);

        let ctx = eframe::egui::Context::default();
        ui::theme::store_theme(&ctx, ui::theme::ThemeSelection::default().get_colors());

        for widget in &mut all_widgets {
            let mut tiles = egui_tiles::Tiles::default();
            let pane_id = tiles.insert_pane(std::mem::replace(
                widget,
                widgets::Widget::ViewerList(ui::viewer_list::ViewerList::new(999)),
            ));
            let mut tree = egui_tiles::Tree::new("single_widget_test", pane_id, tiles);

            for &size in &sizes {
                let screen_rect = eframe::egui::Rect::from_min_size(eframe::egui::Pos2::ZERO, size);
                for _pass in 0..2 {
                    let mut raw_input = eframe::egui::RawInput {
                        screen_rect: Some(screen_rect),
                        ..Default::default()
                    };
                    raw_input.viewport_id = eframe::egui::ViewportId::ROOT;
                    let view = workspace_view::WorkspaceDataView::new(&router);
                    let mut action_queue = Vec::new();
                    let full_output = ctx.run(raw_input, |ctx| {
                        eframe::egui::CentralPanel::default()
                            .frame(eframe::egui::Frame::NONE)
                            .show(ctx, |ui| {
                                let mut behavior = WorkspaceTileBehavior {
                                    view: &view,
                                    bus_load: &bus_load,
                                    action_queue: &mut action_queue,
                                    database: db.as_ref(),
                                    ui_to_can_tx: tx.clone(),
                                    formatter: &formatter,
                                    canvas_color: eframe::egui::Color32::BLACK,
                                    accent_color: eframe::egui::Color32::LIGHT_BLUE,
                                    muted_color: eframe::egui::Color32::GRAY,
                                };
                                tree.ui(&mut behavior, ui);
                            });
                    });

                    for prim in &full_output.shapes {
                        assert!(
                            prim.clip_rect.min.x >= -1.0
                                && prim.clip_rect.min.y >= -1.0
                                && prim.clip_rect.max.x <= size.x + 1.0
                                && prim.clip_rect.max.y <= size.y + 1.0,
                            "Clip rect {:?} exceeded screen rect {:?}",
                            prim.clip_rect,
                            screen_rect
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn multi_pane_grid_with_all_widgets_resizes_cleanly() {
        let t0 = daqcore::time::Time::from_unix_millis(1_794_500_000_000);
        let router = daqcore::router::VirtualTimelineRouter::live(t0, 30.0).expect("live router");
        let db = sample_database();
        let bus_load = Vec::new();
        let formatter = app::load_formatter();
        let (tx, _rx) = mpsc::channel();

        let widgets = build_all_widgets(db.as_ref(), tx.clone());
        let mut tiles = egui_tiles::Tiles::default();
        let pane_ids: Vec<_> = widgets.into_iter().map(|w| tiles.insert_pane(w)).collect();
        let grid_id = tiles.insert_grid_tile(pane_ids);
        let mut tree = egui_tiles::Tree::new("multi_grid_test", grid_id, tiles);

        let ctx = eframe::egui::Context::default();
        ui::theme::store_theme(&ctx, ui::theme::ThemeSelection::default().get_colors());

        for size in [
            eframe::egui::vec2(1600.0, 900.0),
            eframe::egui::vec2(800.0, 600.0),
            eframe::egui::vec2(480.0, 360.0),
        ] {
            let screen_rect = eframe::egui::Rect::from_min_size(eframe::egui::Pos2::ZERO, size);
            let raw_input = eframe::egui::RawInput {
                screen_rect: Some(screen_rect),
                ..Default::default()
            };
            let view = workspace_view::WorkspaceDataView::new(&router);
            let mut action_queue = Vec::new();
            let _ = ctx.run(raw_input, |ctx| {
                eframe::egui::CentralPanel::default()
                    .frame(eframe::egui::Frame::NONE)
                    .show(ctx, |ui| {
                        let mut behavior = WorkspaceTileBehavior {
                            view: &view,
                            bus_load: &bus_load,
                            action_queue: &mut action_queue,
                            database: db.as_ref(),
                            ui_to_can_tx: tx.clone(),
                            formatter: &formatter,
                            canvas_color: eframe::egui::Color32::BLACK,
                            accent_color: eframe::egui::Color32::LIGHT_BLUE,
                            muted_color: eframe::egui::Color32::GRAY,
                        };
                        tree.ui(&mut behavior, ui);
                    });
            });
        }
    }

    #[test]
    fn per26_m6_analysis_widgets_v2_multi_axis_distance_overlay_state_lane_xy_scatter_and_gps_e2e()
    {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let dbc_path = manifest_dir.join("../../superdbc/superdbc_per26.json");
        let log_1 = manifest_dir
            .join("../../PER26/2026-09-06/1_no_cooling/raw/log-2026-09-06--18-36-02.log");
        let log_2 =
            manifest_dir.join("../../PER26/2026-09-06/2_regen/raw/log-2026-09-06--20-16-37.log");
        if !dbc_path.is_file() || !log_1.is_file() || !log_2.is_file() {
            return;
        }

        let db = sample_database().expect("load PER26 database");
        let tmp_raw = tempfile::tempdir().expect("temp raw dir");
        std::fs::copy(&log_1, tmp_raw.path().join("log-2026-09-06--18-36-02.log"))
            .expect("copy log 1");
        std::fs::copy(&log_2, tmp_raw.path().join("log-2026-09-06--20-16-37.log"))
            .expect("copy log 2");

        let tmp_store = tempfile::tempdir().expect("temp store dir");
        let summary = daqcore::log_parse::import_logs_to_store(
            tmp_raw.path(),
            tmp_store.path(),
            &db.database,
        )
        .expect("import PER26 logs");
        assert!(summary.partitions_written >= 2);

        let m1 = &summary.manifests[0];
        let m2 = &summary.manifests[1];
        let mut router = daqcore::router::VirtualTimelineRouter::live(
            daqcore::time::Time::from_unix_millis(m2.end_utc_ms),
            30.0,
        )
        .expect("live router");
        router
            .attach_store(tmp_store.path())
            .expect("attach PER26 store");

        // 1. Verify Preset constructors spawn expected widget ensembles
        let mut ids = widget_ids::WidgetIds::new();
        let (tx, _rx) = mpsc::channel();
        let gg_widgets = widget_constructor::WidgetConstructor::PresetGgDiagram.create_all(
            &mut ids,
            tx.clone(),
            0,
        );
        assert_eq!(gg_widgets.len(), 1);
        let understeer_widgets = widget_constructor::WidgetConstructor::PresetHandlingUndersteer
            .create_all(&mut ids, tx.clone(), 0);
        assert_eq!(understeer_widgets.len(), 2);
        let battery_widgets = widget_constructor::WidgetConstructor::PresetBatteryOverview
            .create_all(&mut ids, tx.clone(), 0);
        assert_eq!(battery_widgets.len(), 3);

        // 2. Focus on Stint 1 and prefetch key columns so FullRes / Overview queries have resident data
        router
            .timeline_mut()
            .snap_to_range_ms(m1.start_utc_ms, m1.end_utc_ms);
        for sig in [
            "IMU_acceleration.X_axis",
            "IMU_acceleration.Y_axis",
            "IMU_angular_rate.Z_axis",
            "LWS_Standard.LWS_ANGLE",
            "gps_speed_heading.gps_speed",
            "gps_coordinates.latitude",
            "gps_coordinates.longitude",
            "pack_stats.pack_voltage",
            "pack_stats.pack_current",
            "pack_stats.max_temp",
            "battery.power_kw",
        ] {
            let _ = router.query_signal_blocking(
                sig,
                m1.start_utc_ms..=m1.end_utc_ms,
                daqcore::router::Resolution::FullRes,
            );
            let _ = router.query_signal_blocking(
                sig,
                m2.start_utc_ms..=m2.end_utc_ms,
                daqcore::router::Resolution::FullRes,
            );
        }

        // Verify discrete state intervals exist in PER26 partitions
        let car_states = router.query_states("main_hb.car_state", m1.start_utc_ms..=m2.end_utc_ms);
        assert!(
            !car_states.is_empty(),
            "PER26 store should yield discrete car_state intervals"
        );
        let imd_states =
            router.query_states("a_box_fault_sync.IMD", m1.start_utc_ms..=m2.end_utc_ms);
        assert!(
            !imd_states.is_empty(),
            "PER26 store should yield 1-bit fault intervals via states.parquet or RLE fallback"
        );

        // Verify distance-indexed query on PER26 stint
        let dist_series = router.query_signal_vs_distance(
            "IMU_acceleration.X_axis",
            "gps_speed_heading.gps_speed",
            m1.start_utc_ms..=m1.end_utc_ms,
            daqcore::router::Resolution::FullRes,
        );
        assert!(!dist_series.is_empty());

        // 3. Build a 4-pane analysis workspace (Scope v2 + StateLane + XYScatter + GpsPlot v2)
        let mut scope = ui::scope::Scope::new_multi(
            1,
            "Scope: Multi-Unit Vehicle & Pack",
            vec![
                (
                    "pack_stats.pack_voltage".to_owned(),
                    "Pack Voltage".to_owned(),
                    "V".to_owned(),
                ),
                (
                    "pack_stats.pack_current".to_owned(),
                    "Pack Current".to_owned(),
                    "A".to_owned(),
                ),
                (
                    "battery.power_kw".to_owned(),
                    "Pack Power".to_owned(),
                    "kW".to_owned(),
                ),
                (
                    "IMU_angular_rate.Z_axis".to_owned(),
                    "Yaw Rate".to_owned(),
                    "deg/s".to_owned(),
                ),
            ],
        );
        let state_lane = ui::state_lane::StateLane::new(1);
        let xy_scatter = ui::xy_scatter::XYScatter::preset_gg_diagram(1);
        let gps_plot = ui::gps_plot::GpsPlot::new(1);

        // Select 2 annotations on the Master Timeline to exercise multi-lap comparison mode in Scope
        let all_anns = router
            .query_annotations_in_range(m1.start_utc_ms..=m2.end_utc_ms)
            .expect("query annotations");
        if all_anns.len() >= 2 {
            router.timeline_mut().toggle_comparison_annotation(
                all_anns[0].id.clone(),
                all_anns[0].start_utc_ms,
                all_anns[0].end_utc_ms,
            );
            router.timeline_mut().toggle_comparison_annotation(
                all_anns[1].id.clone(),
                all_anns[1].start_utc_ms,
                all_anns[1].end_utc_ms,
            );
            assert!(router.timeline().is_comparison_mode());
        }

        let ctx = eframe::egui::Context::default();
        ui::theme::store_theme(&ctx, ui::theme::ThemeSelection::default().get_colors());

        // Exercise Scope in comparison mode across ElapsedTime and TrackDistance modes
        for mode in [
            ui::scope::ScopeXAxisMode::ElapsedTime,
            ui::scope::ScopeXAxisMode::TrackDistance,
        ] {
            scope.x_axis_mode = mode;
            let view = workspace_view::WorkspaceDataView::with_hover(
                &router,
                Some((m1.start_utc_ms + m1.end_utc_ms) / 2),
                Some(25.0),
            );
            let raw_input = eframe::egui::RawInput {
                screen_rect: Some(eframe::egui::Rect::from_min_size(
                    eframe::egui::Pos2::ZERO,
                    eframe::egui::vec2(1280.0, 720.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(raw_input, |ctx| {
                let central_frame = eframe::egui::Frame::central_panel(&ctx.style())
                    .inner_margin(eframe::egui::Margin::same(2));
                eframe::egui::CentralPanel::default()
                    .frame(central_frame)
                    .show(ctx, |ui| {
                        let _ = scope.show(ui, &view);
                    });
            });
        }

        // Exit comparison mode, snap to Stint 1, and render all 4 widgets in a synchronized multi-pane grid
        router.timeline_mut().clear_selected_annotations();
        router
            .timeline_mut()
            .snap_to_range_ms(m1.start_utc_ms, m1.end_utc_ms);
        scope.x_axis_mode = ui::scope::ScopeXAxisMode::UtcTime;

        let mut tiles = egui_tiles::Tiles::default();
        let pane_ids = vec![
            tiles.insert_pane(widgets::Widget::Scope(scope)),
            tiles.insert_pane(widgets::Widget::StateLane(state_lane)),
            tiles.insert_pane(widgets::Widget::XYScatter(xy_scatter)),
            tiles.insert_pane(widgets::Widget::GpsPlot(gps_plot)),
        ];
        let grid_id = tiles.insert_grid_tile(pane_ids);
        let mut tree = egui_tiles::Tree::new("m6_e2e_grid", grid_id, tiles);

        let mid_ms = (m1.start_utc_ms + m1.end_utc_ms) / 2;
        let view = workspace_view::WorkspaceDataView::with_hover(&router, Some(mid_ms), Some(30.0));
        let bus_load = Vec::new();
        let formatter = app::load_formatter();
        let mut action_queue = Vec::new();
        let screen_rect = eframe::egui::Rect::from_min_size(
            eframe::egui::Pos2::ZERO,
            eframe::egui::vec2(1440.0, 900.0),
        );
        let raw_input = eframe::egui::RawInput {
            screen_rect: Some(screen_rect),
            ..Default::default()
        };
        let full_output = ctx.run(raw_input, |ctx| {
            eframe::egui::CentralPanel::default()
                .frame(eframe::egui::Frame::NONE)
                .show(ctx, |ui| {
                    let mut behavior = WorkspaceTileBehavior {
                        view: &view,
                        bus_load: &bus_load,
                        action_queue: &mut action_queue,
                        database: Some(&db),
                        ui_to_can_tx: tx.clone(),
                        formatter: &formatter,
                        canvas_color: eframe::egui::Color32::BLACK,
                        accent_color: eframe::egui::Color32::LIGHT_BLUE,
                        muted_color: eframe::egui::Color32::GRAY,
                    };
                    tree.ui(&mut behavior, ui);
                });
        });

        for prim in &full_output.shapes {
            assert!(
                prim.clip_rect.min.x >= -1.0
                    && prim.clip_rect.min.y >= -1.0
                    && prim.clip_rect.max.x <= 1441.0
                    && prim.clip_rect.max.y <= 901.0,
                "Clip rect {:?} exceeded screen rect {:?}",
                prim.clip_rect,
                screen_rect
            );
        }

        // Verify scrolling over the Scope pane zooms the MasterTimeline
        let initial_span_ms = router.timeline().end_utc_ms() - router.timeline().start_utc_ms();
        let zoom_view = workspace_view::WorkspaceDataView::with_hover(&router, Some(mid_ms), None);
        let zoom_input = eframe::egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![
                eframe::egui::Event::PointerMoved(eframe::egui::pos2(300.0, 220.0)),
                eframe::egui::Event::MouseWheel {
                    unit: eframe::egui::MouseWheelUnit::Point,
                    delta: eframe::egui::vec2(0.0, 24.0),
                    modifiers: eframe::egui::Modifiers::NONE,
                },
            ],
            ..Default::default()
        };
        let _ = ctx.run(zoom_input, |ctx| {
            eframe::egui::CentralPanel::default()
                .frame(eframe::egui::Frame::NONE)
                .show(ctx, |ui| {
                    let mut behavior = WorkspaceTileBehavior {
                        view: &zoom_view,
                        bus_load: &bus_load,
                        action_queue: &mut action_queue,
                        database: Some(&db),
                        ui_to_can_tx: tx.clone(),
                        formatter: &formatter,
                        canvas_color: eframe::egui::Color32::BLACK,
                        accent_color: eframe::egui::Color32::LIGHT_BLUE,
                        muted_color: eframe::egui::Color32::GRAY,
                    };
                    tree.ui(&mut behavior, ui);
                });
        });
        let (anchor_ms, factor) = zoom_view
            .requested_zoom()
            .expect("Scrolling on Scope should request MasterTimeline zoom");
        assert!(factor < 1.0, "Positive scroll wheel should zoom in");
        router.timeline_mut().zoom_around_ms(anchor_ms, factor);
        let zoomed_span_ms = router.timeline().end_utc_ms() - router.timeline().start_utc_ms();
        assert!(
            zoomed_span_ms < initial_span_ms,
            "MasterTimeline span should shrink after zooming on Scope ({zoomed_span_ms} vs {initial_span_ms})"
        );

        // Verify left-clicking and dragging horizontally on Scope pans the MasterTimeline
        let press_view = workspace_view::WorkspaceDataView::with_hover(&router, Some(mid_ms), None);
        let press_input = eframe::egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![
                eframe::egui::Event::PointerMoved(eframe::egui::pos2(300.0, 220.0)),
                eframe::egui::Event::PointerButton {
                    pos: eframe::egui::pos2(300.0, 220.0),
                    button: eframe::egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: eframe::egui::Modifiers::NONE,
                },
            ],
            ..Default::default()
        };
        let _ = ctx.run(press_input, |ctx| {
            eframe::egui::CentralPanel::default()
                .frame(eframe::egui::Frame::NONE)
                .show(ctx, |ui| {
                    let mut behavior = WorkspaceTileBehavior {
                        view: &press_view,
                        bus_load: &bus_load,
                        action_queue: &mut action_queue,
                        database: Some(&db),
                        ui_to_can_tx: tx.clone(),
                        formatter: &formatter,
                        canvas_color: eframe::egui::Color32::BLACK,
                        accent_color: eframe::egui::Color32::LIGHT_BLUE,
                        muted_color: eframe::egui::Color32::GRAY,
                    };
                    tree.ui(&mut behavior, ui);
                });
        });

        let pan_view = workspace_view::WorkspaceDataView::with_hover(&router, Some(mid_ms), None);
        let drag_input = eframe::egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![eframe::egui::Event::PointerMoved(eframe::egui::pos2(
                240.0, 220.0,
            ))],
            ..Default::default()
        };
        let _ = ctx.run(drag_input, |ctx| {
            eframe::egui::CentralPanel::default()
                .frame(eframe::egui::Frame::NONE)
                .show(ctx, |ui| {
                    let mut behavior = WorkspaceTileBehavior {
                        view: &pan_view,
                        bus_load: &bus_load,
                        action_queue: &mut action_queue,
                        database: Some(&db),
                        ui_to_can_tx: tx.clone(),
                        formatter: &formatter,
                        canvas_color: eframe::egui::Color32::BLACK,
                        accent_color: eframe::egui::Color32::LIGHT_BLUE,
                        muted_color: eframe::egui::Color32::GRAY,
                    };
                    tree.ui(&mut behavior, ui);
                });
        });
        let pan_delta_ms = pan_view
            .requested_pan_delta_ms()
            .expect("Dragging horizontally on Scope should request MasterTimeline pan");
        assert!(
            pan_delta_ms > 0,
            "Dragging left should shift the timeline window forward in time (got {pan_delta_ms})"
        );
    }

    #[test]
    fn nominal_redesign_full_shell_themes_tabs_and_bottom_timeline_render_cleanly() {
        let ctx = eframe::egui::Context::default();
        let mut app = app::DAQApp::new_for_test(&ctx);
        app.database = sample_database();
        app.show_share_popover = true;

        let screen_rect = eframe::egui::Rect::from_min_size(
            eframe::egui::Pos2::ZERO,
            eframe::egui::vec2(1440.0, 900.0),
        );

        for theme_sel in [
            ui::theme::ThemeSelection::Default,
            ui::theme::ThemeSelection::NominalLight,
        ] {
            app.set_theme(&ctx, theme_sel);
            for tree_mode in [false, true] {
                app.signal_browser.tree_mode = tree_mode;
                for tab in [
                    ui::sidebar::SidebarTab::Channels,
                    ui::sidebar::SidebarTab::Sessions,
                    ui::sidebar::SidebarTab::Widgets,
                    ui::sidebar::SidebarTab::Settings,
                ] {
                    app.sidebar_tab = tab;
                    app.is_sidebar_open = true;

                    let mut raw_input = eframe::egui::RawInput {
                        screen_rect: Some(screen_rect),
                        ..Default::default()
                    };
                    raw_input.viewport_id = eframe::egui::ViewportId::ROOT;

                    let full_output = ctx.run(raw_input, |ctx| {
                        ctx.set_style(app.theme.clone());
                        ui::sidebar::show(&mut app, ctx);
                        ui::timeline::show(&mut app, ctx);
                        super::show(&mut app, ctx);
                    });

                    for prim in &full_output.shapes {
                        assert!(
                            prim.clip_rect.min.x >= -1.0
                                && prim.clip_rect.min.y >= -1.0
                                && prim.clip_rect.max.x <= 1441.0
                                && prim.clip_rect.max.y <= 901.0,
                            "Clip rect {:?} exceeded screen rect {:?}",
                            prim.clip_rect,
                            screen_rect
                        );
                    }
                }
            }
        }

        // Regression test: ultra-narrow window (width = 214.0px where rail + drawer consume ~213.4px)
        // must never panic with `min > max` inside `timeline.rs` or `scope.rs`.
        let tiny_rect = eframe::egui::Rect::from_min_size(
            eframe::egui::Pos2::ZERO,
            eframe::egui::vec2(214.0, 180.0),
        );
        let mut tiny_input = eframe::egui::RawInput {
            screen_rect: Some(tiny_rect),
            ..Default::default()
        };
        tiny_input.viewport_id = eframe::egui::ViewportId::ROOT;
        let _ = ctx.run(tiny_input, |ctx| {
            ctx.set_style(app.theme.clone());
            ui::sidebar::show(&mut app, ctx);
            ui::timeline::show(&mut app, ctx);
            super::show(&mut app, ctx);
        });

        // Verify keyboard shortcuts: Space (play/pause), H/L (step setpoint), Shift+H/L (pan window),
        // Shift+K/J (zoom window), G (fold gaps), and `/` (focus sidebar search without typing '/').
        let send_key = |app: &mut app::DAQApp,
                        ctx: &eframe::egui::Context,
                        key: eframe::egui::Key,
                        modifiers: eframe::egui::Modifiers,
                        text: Option<&str>| {
            let mut events = vec![eframe::egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            }];
            if let Some(t) = text {
                events.push(eframe::egui::Event::Text(t.to_owned()));
            }
            let mut input = eframe::egui::RawInput {
                screen_rect: Some(screen_rect),
                events,
                modifiers,
                ..Default::default()
            };
            input.viewport_id = eframe::egui::ViewportId::ROOT;
            let _ = ctx.run(input, |ctx| {
                ctx.set_style(app.theme.clone());
                let actions = crate::shortcuts::ShortcutHandler::check_shortcuts(ctx);
                for act in actions {
                    app.handle_action(act, ctx);
                }
                ui::sidebar::show(app, ctx);
                ui::timeline::show(app, ctx);
                super::show(app, ctx);
            });
        };

        assert!(!app.router.timeline().is_playing());
        send_key(
            &mut app,
            &ctx,
            eframe::egui::Key::Space,
            eframe::egui::Modifiers::NONE,
            None,
        );
        assert!(app.router.timeline().is_playing());
        send_key(
            &mut app,
            &ctx,
            eframe::egui::Key::Space,
            eframe::egui::Modifiers::NONE,
            None,
        );
        assert!(!app.router.timeline().is_playing());

        let sp0 = app.router.timeline().setpoint_utc_ms();
        send_key(
            &mut app,
            &ctx,
            eframe::egui::Key::H,
            eframe::egui::Modifiers::NONE,
            None,
        );
        let sp1 = app.router.timeline().setpoint_utc_ms();
        assert!(sp1 < sp0, "H should step playhead backward");
        send_key(
            &mut app,
            &ctx,
            eframe::egui::Key::L,
            eframe::egui::Modifiers::NONE,
            None,
        );
        assert_eq!(app.router.timeline().setpoint_utc_ms(), sp0);

        let win_start0 = app.router.timeline().start_utc_ms();
        send_key(
            &mut app,
            &ctx,
            eframe::egui::Key::H,
            eframe::egui::Modifiers::SHIFT,
            None,
        );
        assert!(
            app.router.timeline().start_utc_ms() < win_start0,
            "Shift+H should pan window left"
        );

        let span0 = app.router.timeline().window_ms();
        send_key(
            &mut app,
            &ctx,
            eframe::egui::Key::K,
            eframe::egui::Modifiers::SHIFT,
            None,
        );
        assert!(
            app.router.timeline().window_ms() < span0,
            "Shift+K should shrink (zoom in) window"
        );

        let fold0 = app.router.timeline().collapse_idle_gaps();
        send_key(
            &mut app,
            &ctx,
            eframe::egui::Key::G,
            eframe::egui::Modifiers::NONE,
            None,
        );
        assert_eq!(app.router.timeline().collapse_idle_gaps(), !fold0);

        app.is_sidebar_open = false;
        app.signal_browser.search_query.clear();
        send_key(
            &mut app,
            &ctx,
            eframe::egui::Key::Slash,
            eframe::egui::Modifiers::NONE,
            Some("/"),
        );
        assert!(
            app.is_sidebar_open,
            "'/' should open the sidebar and focus search"
        );
        assert_eq!(
            app.signal_browser.search_query, "",
            "'/' keybind must not insert a literal '/' into the search query"
        );
    }
}
