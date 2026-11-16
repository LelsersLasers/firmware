use crate::{action, app, ui, widget_constructor, widgets, workspace_view};

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

fn show_workspace_tabs_bar(
    app: &mut app::ViewerState,
    supports_hardware_widgets: bool,
    ctx: &eframe::egui::Context,
) {
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
                        ] {
                            if ui.button(label).clicked() {
                                app.action_queue.push(action::AppAction::SpawnWidget(ctor));
                                ui.close();
                            }
                        }
                        if supports_hardware_widgets {
                            for (label, ctor) in [
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

pub fn show(
    app: &mut app::ViewerState,
    platform: &mut dyn app::ViewerPlatform,
    ctx: &eframe::egui::Context,
) {
    ui::timeline::show(app, platform, ctx);
    show_workspace_tabs_bar(app, platform.supports_hardware_widgets(), ctx);
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
    if view.requested_open_export_modal() {
        app.action_queue
            .push(action::AppAction::OpenExportDataModal);
    }

    if hover_touched {
        app.hover_utc_ms = next_hover_ms;
        app.hover_distance_m = next_hover_dist;
    } else {
        app.hover_utc_ms = None;
        app.hover_distance_m = None;
    }
    let mut timeline_changed = false;
    if let Some((anchor_ms, zoom_factor)) = requested_zoom {
        app.router
            .timeline_mut()
            .zoom_around_ms(anchor_ms, zoom_factor);
        timeline_changed = true;
    }
    if let Some(delta_ms) = requested_pan_delta_ms {
        let next_start_ms = app
            .router
            .timeline()
            .start_utc_ms()
            .saturating_add(delta_ms);
        app.router.timeline_mut().pan_to_start_ms(next_start_ms);
        timeline_changed = true;
    }
    if let Some(setpoint_ms) = requested_setpoint_ms {
        app.router
            .timeline_mut()
            .set_setpoint(daqcore::time::Time::from_unix_millis(setpoint_ms));
        timeline_changed = true;
    }
    if timeline_changed {
        platform.on_workspace_changed(app);
    }
}

pub struct WorkspaceTileBehavior<'a> {
    pub view: &'a workspace_view::WorkspaceDataView<'a>,
    pub bus_load: &'a [workspace_view::BusLoadSample],
    pub action_queue: &'a mut Vec<action::AppAction>,
    pub database: Option<&'a app::DatabaseSelection>,
    pub formatter: &'a Option<daqcore::formatter::Formatter>,
    pub canvas_color: eframe::egui::Color32,
    pub accent_color: eframe::egui::Color32,
    pub muted_color: eframe::egui::Color32,
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
