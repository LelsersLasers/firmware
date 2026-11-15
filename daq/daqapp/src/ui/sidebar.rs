use crate::{action, app, assets, settings, ui, util, widget_constructor};
use std::path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SidebarTab {
    #[default]
    Channels,
    Sessions,
    Widgets,
    Settings,
}

impl SidebarTab {
    pub const ALL: [Self; 4] = [
        Self::Channels,
        Self::Sessions,
        Self::Widgets,
        Self::Settings,
    ];

    pub fn icon(self) -> &'static str {
        match self {
            Self::Channels => "ƒ",
            Self::Sessions => "📅",
            Self::Widgets => "+",
            Self::Settings => "📡",
        }
    }

    pub fn tooltip(self) -> &'static str {
        match self {
            Self::Channels => "Channels & Derived Formulas (Cmd+S)",
            Self::Sessions => "Sessions, Stints & Partitions",
            Self::Widgets => "Add Widgets & Subteam Presets",
            Self::Settings => "Hardware Connection, SuperDBC & Settings",
        }
    }
}

pub fn select_database(app: &mut app::DAQApp) {
    let mut dialog = rfd::FileDialog::new().add_filter("SuperDBC JSON", &["json"]);
    if let Some(dir) = settings::database_dir() {
        dialog = dialog.set_directory(dir);
    }
    if let Some(path) = dialog.pick_file() {
        let preferred = app.database.as_ref().map(|selection| selection.selection);
        app.request_database(&path, preferred);
    }
}

pub fn show(app: &mut app::DAQApp, ctx: &eframe::egui::Context) {
    show_top_header_bar(app, ctx);
    show_share_popover(app, ctx);

    let theme = ui::theme::get_theme(ctx);
    let border_color = theme.border_color();
    let accent_color = theme.accent_color();
    let muted_color = theme.muted_text_color();

    // 1. Narrow Left Activity Rail (Nominal-style icon switcher)
    let rail_frame = eframe::egui::Frame::side_top_panel(&ctx.style())
        .inner_margin(eframe::egui::Margin::symmetric(4, 6))
        .stroke(eframe::egui::Stroke::new(1.0_f32, border_color));

    eframe::egui::SidePanel::left("left_activity_rail")
        .resizable(false)
        .exact_width(34.0)
        .frame(rail_frame)
        .show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                for tab in SidebarTab::ALL {
                    let is_active = app.is_sidebar_open && app.sidebar_tab == tab;
                    let fill = if is_active {
                        accent_color.gamma_multiply(0.22_f32)
                    } else {
                        eframe::egui::Color32::TRANSPARENT
                    };
                    let stroke = if is_active {
                        eframe::egui::Stroke::new(1.0_f32, accent_color)
                    } else {
                        eframe::egui::Stroke::NONE
                    };
                    let text_color = if is_active {
                        ui.visuals().text_color()
                    } else {
                        muted_color
                    };

                    let btn = eframe::egui::Button::new(
                        eframe::egui::RichText::new(tab.icon())
                            .color(text_color)
                            .strong(),
                    )
                    .fill(fill)
                    .stroke(stroke)
                    .min_size(eframe::egui::vec2(26.0, 26.0));

                    if ui.add(btn).on_hover_text(tab.tooltip()).clicked() {
                        if app.is_sidebar_open && app.sidebar_tab == tab {
                            app.is_sidebar_open = false;
                        } else {
                            app.sidebar_tab = tab;
                            app.is_sidebar_open = true;
                        }
                    }
                }
            });

            // Subtle FPS status indicator at the bottom of the rail
            ui.with_layout(
                eframe::egui::Layout::bottom_up(eframe::egui::Align::Center),
                |ui| {
                    let fps_color = if app.fps >= 50.0 {
                        eframe::egui::Color32::from_rgb(63, 185, 80)
                    } else if app.fps >= 25.0 {
                        eframe::egui::Color32::from_rgb(210, 153, 34)
                    } else {
                        eframe::egui::Color32::from_rgb(248, 81, 73)
                    };
                    ui.label(
                        eframe::egui::RichText::new(format!("{:.0}", app.fps))
                            .monospace()
                            .size(9.5)
                            .color(fps_color),
                    )
                    .on_hover_text(format!(
                        "UI Render Rate: {:.1} FPS ({:.2} ms/frame)",
                        app.fps, app.frame_time_ms
                    ));
                },
            );
        });

    // 2. Collapsible Focused Sidebar Drawer
    let drawer_frame = eframe::egui::Frame::side_top_panel(&ctx.style())
        .fill(theme.panel_color())
        .inner_margin(eframe::egui::Margin::symmetric(6, 6))
        .stroke(eframe::egui::Stroke::new(1.0_f32, border_color));

    eframe::egui::SidePanel::left("left_sidebar")
        .resizable(true)
        .default_width(250.0)
        .width_range(190.0..=440.0)
        .frame(drawer_frame)
        .show_animated(ctx, app.is_sidebar_open, |ui| {
            let panel_width = ui.available_width().max(1.0);
            ui.set_max_width(panel_width);
            ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));

            match app.sidebar_tab {
                SidebarTab::Sessions => {
                    show_sessions_tab(ui, app, muted_color);
                }
                SidebarTab::Channels => {
                    app.signal_browser.hover_utc_ms = app.hover_utc_ms;
                    eframe::egui::ScrollArea::vertical()
                        .id_salt("sidebar_channels_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.set_max_width(ui.available_width().max(1.0));
                            ui::signal_browser::show_signal_browser(
                                ui,
                                &mut app.signal_browser,
                                &mut app.router,
                                &mut app.action_queue,
                                &mut app.diagnostic,
                            );
                        });
                }
                SidebarTab::Widgets => {
                    eframe::egui::ScrollArea::vertical()
                        .id_salt("sidebar_widgets_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.set_max_width(ui.available_width().max(1.0));
                            show_widgets_tab(ui, app, muted_color);
                        });
                }
                SidebarTab::Settings => {
                    eframe::egui::ScrollArea::vertical()
                        .id_salt("sidebar_settings_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.set_max_width(ui.available_width().max(1.0));
                            show_settings_tab(ui, app, muted_color);
                        });
                }
            }
        });
}

fn show_top_header_bar(app: &mut app::DAQApp, ctx: &eframe::egui::Context) {
    let theme = ui::theme::get_theme(ctx);
    let border_color = theme.border_color();
    let accent_color = theme.accent_color();
    let muted_color = theme.muted_text_color();

    let header_frame = eframe::egui::Frame::side_top_panel(&ctx.style())
        .inner_margin(eframe::egui::Margin::symmetric(8, 3))
        .stroke(eframe::egui::Stroke::new(1.0_f32, border_color));

    eframe::egui::TopBottomPanel::top("top_app_header_bar")
        .exact_height(28.0)
        .frame(header_frame)
        .show(ctx, |ui| {
            let full_rect = ui.max_rect();
            ui.horizontal_centered(|ui| {
                // Left: Brand + Context Breadcrumb (`PER DAQ  /  2026-09-06  /  Stints`)
                ui.add(
                    eframe::egui::Image::from_bytes(assets::PER_LOGO_PATH, assets::PER_LOGO_BYTES)
                        .max_size(eframe::egui::vec2(16.0, 16.0))
                        .corner_radius(3),
                );
                ui.label(eframe::egui::RichText::new("PER DAQ").strong().small());
                ui.label(eframe::egui::RichText::new("/").color(muted_color).small());

                // Track Day Selector in Breadcrumb
                let track_days = app.router.list_track_days().unwrap_or_default();
                let active_day_id = app
                    .router
                    .timeline()
                    .active_track_day_id()
                    .map(str::to_owned);
                let mut requested_day_change: Option<Option<String>> = None;

                let day_text = active_day_id.as_deref().unwrap_or("All Track Days");
                eframe::egui::ComboBox::from_id_salt("header_track_day_selector")
                    .selected_text(eframe::egui::RichText::new(day_text).small().strong())
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

                if let Some(day_choice) = requested_day_change
                    && let Err(err) = app.router.select_track_day(day_choice)
                {
                    app.diagnostic = Some(format!("Failed to switch track day: {err}"));
                }

                // Connection / Provenance compact pill
                let (conn_label, conn_color) = match &app.connection_status {
                    app::ConnectionStatus::Connected => {
                        ("✔ Connected", eframe::egui::Color32::from_rgb(34, 197, 94))
                    }
                    app::ConnectionStatus::Error(_) => {
                        ("🔴 CAN Error", eframe::egui::Color32::from_rgb(239, 68, 68))
                    }
                    app::ConnectionStatus::Disconnected => {
                        let setpoint_ms = app.router.timeline().setpoint_utc_ms();
                        match app.router.clock_provenance_at(setpoint_ms) {
                            Some(daqcore::router::ClockProvenance::CanonicalPartition(_)) => {
                                ("🕒 SD Synced", eframe::egui::Color32::from_rgb(34, 197, 94))
                            }
                            Some(daqcore::router::ClockProvenance::LiveProvisional) => {
                                ("📡 Live Stream", accent_color)
                            }
                            None => ("• Offline", muted_color),
                        }
                    }
                };
                if ui
                    .add(
                        eframe::egui::Button::new(
                            eframe::egui::RichText::new(conn_label)
                                .small()
                                .color(conn_color),
                        )
                        .frame(false),
                    )
                    .on_hover_text("Click to open Hardware & Connection settings")
                    .clicked()
                {
                    app.sidebar_tab = SidebarTab::Settings;
                    app.is_sidebar_open = true;
                }

                // Right: Theme selector, Export, and Nominal-style SHARE button
                ui.with_layout(
                    eframe::egui::Layout::right_to_left(eframe::egui::Align::Center),
                    |ui| {
                        let share_btn = eframe::egui::Button::new(
                            eframe::egui::RichText::new("SHARE")
                                .size(10.5)
                                .strong()
                                .color(if theme.is_light {
                                    eframe::egui::Color32::WHITE
                                } else {
                                    ui.visuals().text_color()
                                }),
                        )
                        .fill(if theme.is_light {
                            eframe::egui::Color32::from_rgb(15, 23, 42)
                        } else {
                            accent_color.gamma_multiply(0.32_f32)
                        })
                        .stroke(eframe::egui::Stroke::new(1.0_f32, accent_color));

                        if ui
                            .add(share_btn)
                            .on_hover_text(
                                "Share workspace view (.daqview.json or daqview1:... link)",
                            )
                            .clicked()
                        {
                            app.show_share_popover = !app.show_share_popover;
                        }

                        if ui
                            .small_button("📤 Export")
                            .on_hover_text("Export dataset (Parquet / CSV / MATLAB .mat)")
                            .clicked()
                        {
                            app.action_queue
                                .push(action::AppAction::OpenExportDataModal);
                        }

                        let mut next_theme = app.theme_selection;
                        eframe::egui::ComboBox::from_id_salt("header_theme_selector")
                            .width(105.0)
                            .selected_text(
                                eframe::egui::RichText::new(format!(
                                    "🎨 {}",
                                    app.theme_selection.get_name()
                                ))
                                .small(),
                            )
                            .show_ui(ui, |ui| {
                                for candidate in ui::theme::ThemeSelection::ALL {
                                    ui.selectable_value(
                                        &mut next_theme,
                                        candidate,
                                        candidate.get_name(),
                                    );
                                }
                            });
                        if next_theme != app.theme_selection {
                            app.set_theme(ctx, next_theme);
                            app.save_settings();
                        }
                    },
                );
            });

            // Centered Nominal-style Command & Search Bar (drawn when window is wide enough)
            if full_rect.width() >= 720.0 {
                let cmd_w = (full_rect.width() * 0.26).clamp(200.0, 320.0);
                let cmd_rect = eframe::egui::Rect::from_center_size(
                    full_rect.center(),
                    eframe::egui::vec2(cmd_w, 20.0),
                );
                let mut cmd_ui =
                    ui.new_child(eframe::egui::UiBuilder::new().max_rect(cmd_rect).layout(
                        eframe::egui::Layout::centered_and_justified(
                            eframe::egui::Direction::LeftToRight,
                        ),
                    ));
                let cmd_btn = eframe::egui::Button::new(
                    eframe::egui::RichText::new("🔍 Search or type a command… (Ctrl+P)")
                        .small()
                        .color(muted_color),
                )
                .fill(theme.canvas_color())
                .stroke(eframe::egui::Stroke::new(1.0_f32, border_color));

                if cmd_ui
                    .add(cmd_btn)
                    .on_hover_text("Open Command Palette (Ctrl+P / Cmd+P)")
                    .clicked()
                {
                    app.action_queue
                        .push(action::AppAction::ToggleCommandPalette);
                }
            }
        });
}

fn show_share_popover(app: &mut app::DAQApp, ctx: &eframe::egui::Context) {
    if !app.show_share_popover {
        return;
    }

    let mut keep_open = true;
    eframe::egui::Window::new("Share Workspace")
        .id(eframe::egui::Id::new("nominal_share_popover"))
        .anchor(eframe::egui::Align2::RIGHT_TOP, [-10.0, 32.0])
        .collapsible(false)
        .resizable(false)
        .default_width(290.0)
        .open(&mut keep_open)
        .show(ctx, |ui| {
            ui.label(
                eframe::egui::RichText::new(
                    "Share layout, custom formulas, and active viewport with subteam members.",
                )
                .small()
                .weak(),
            );
            ui.add_space(4.0);

            ui.checkbox(
                &mut app.workspace_view_modal.jump_to_saved_time_range,
                "Jump to saved time range on load",
            );

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui
                    .button("📋 Copy Link (daqview1:...)")
                    .on_hover_text("Encode current workspace layout and copy share code")
                    .clicked()
                {
                    app.action_queue
                        .push(action::AppAction::CopyWorkspaceShareString);
                }
                if ui
                    .button("📁 .daqview.json…")
                    .on_hover_text("Save, import, or paste a workspace view")
                    .clicked()
                {
                    app.action_queue
                        .push(action::AppAction::OpenWorkspaceViewModal);
                    app.show_share_popover = false;
                }
            });

            if !app.workspace_view_modal.generated_share_string.is_empty() {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label(
                        eframe::egui::RichText::new("Share Code (Ctrl+C):")
                            .small()
                            .weak(),
                    );
                    if ui.small_button("Select All").clicked() {
                        app.workspace_view_modal.select_all_share_code = true;
                    }
                });
                let text_output = eframe::egui::TextEdit::multiline(
                    &mut app.workspace_view_modal.generated_share_string,
                )
                .font(eframe::egui::TextStyle::Monospace)
                .desired_rows(2)
                .desired_width(f32::INFINITY)
                .show(ui);

                if app.workspace_view_modal.select_all_share_code
                    && !app.workspace_view_modal.is_open
                {
                    app.workspace_view_modal.select_all_share_code = false;
                    text_output.response.request_focus();
                    let mut state = text_output.state;
                    let char_len = app
                        .workspace_view_modal
                        .generated_share_string
                        .chars()
                        .count();
                    state
                        .cursor
                        .set_char_range(Some(eframe::egui::text::CCursorRange::two(
                            eframe::egui::text::CCursor::new(0),
                            eframe::egui::text::CCursor::new(char_len),
                        )));
                    state.store(ui.ctx(), text_output.response.id);
                }
            }
        });

    if !keep_open {
        app.show_share_popover = false;
    }
}

fn show_sessions_tab(
    ui: &mut eframe::egui::Ui,
    app: &mut app::DAQApp,
    muted_color: eframe::egui::Color32,
) {
    ui.set_max_width(ui.available_width().max(1.0));
    ui.label(
        eframe::egui::RichText::new("SESSIONS, STINTS & PARTITIONS")
            .small()
            .strong()
            .color(muted_color),
    );
    ui.add_space(4.0);
    let list_h = ui.available_height().max(120.0);
    ui::timeline::show_chunk_navigator_contents(
        ui,
        &mut app.router,
        &mut app.timeline_ui,
        &mut app.diagnostic,
        list_h,
    );
}

fn show_widgets_tab(
    ui: &mut eframe::egui::Ui,
    app: &mut app::DAQApp,
    muted_color: eframe::egui::Color32,
) {
    ui.label(
        eframe::egui::RichText::new("ANALYSIS WIDGETS")
            .small()
            .strong()
            .color(muted_color),
    );
    ui.add_space(2.0);

    egui_grid_buttons(
        ui,
        &mut app.action_queue,
        &[
            (
                "Scope (Time / Dist)",
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
        ],
    );

    ui.add_space(8.0);
    ui.label(
        eframe::egui::RichText::new("QUICK ANALYSIS PRESETS")
            .small()
            .strong()
            .color(muted_color),
    );
    ui.add_space(2.0);
    if ui.button("Preset: G-G Diagram").clicked() {
        app.action_queue.push(action::AppAction::SpawnWidget(
            widget_constructor::WidgetConstructor::PresetGgDiagram,
        ));
    }
    if ui.button("Preset: Handling Understeer").clicked() {
        app.action_queue.push(action::AppAction::SpawnWidget(
            widget_constructor::WidgetConstructor::PresetHandlingUndersteer,
        ));
    }
    if ui.button("Preset: Battery Pack Overview").clicked() {
        app.action_queue.push(action::AppAction::SpawnWidget(
            widget_constructor::WidgetConstructor::PresetBatteryOverview,
        ));
    }

    ui.add_space(8.0);
    ui.label(
        eframe::egui::RichText::new("OPERATIONAL & HARDWARE TOOLS")
            .small()
            .strong()
            .color(muted_color),
    );
    ui.add_space(2.0);
    egui_grid_buttons(
        ui,
        &mut app.action_queue,
        &[
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
        ],
    );

    ui.add_space(8.0);
    ui.label(
        eframe::egui::RichText::new("SUBTEAM WORKSPACES")
            .small()
            .strong()
            .color(muted_color),
    );
    ui.checkbox(
        &mut app.workspace_view_modal.jump_to_saved_time_range,
        "Jump to saved time range on load",
    );
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
        if ui
            .button(format!("📁 {preset_name}"))
            .on_hover_text(preset_desc)
            .clicked()
        {
            app.action_queue
                .push(action::AppAction::LoadWorkspacePreset {
                    preset_id,
                    apply_time_context: app.workspace_view_modal.jump_to_saved_time_range,
                });
        }
    }
}

fn egui_grid_buttons(
    ui: &mut eframe::egui::Ui,
    action_queue: &mut Vec<action::AppAction>,
    items: &[(&str, widget_constructor::WidgetConstructor)],
) {
    let col_w = ((ui.available_width() - 6.0) * 0.5).max(70.0);
    for pair in items.chunks(2) {
        ui.horizontal(|ui| {
            for (label, ctor) in pair {
                if ui
                    .add_sized(
                        [col_w, 22.0],
                        eframe::egui::Button::new(eframe::egui::RichText::new(*label).small()),
                    )
                    .clicked()
                {
                    action_queue.push(action::AppAction::SpawnWidget(ctor.clone()));
                }
            }
        });
    }
}

fn show_settings_tab(
    ui: &mut eframe::egui::Ui,
    app: &mut app::DAQApp,
    muted_color: eframe::egui::Color32,
) {
    ui.label(
        eframe::egui::RichText::new("CONNECTION & HARDWARE")
            .small()
            .strong()
            .color(muted_color),
    );
    ui.add_space(4.0);

    ui.horizontal_wrapped(|ui| {
        ui.label("CAN Speed:");
        let speed_options = daqcore::connection::CanBusSpeed::options();
        let selected_speed = app.can_bus_speed;
        eframe::egui::ComboBox::from_id_salt("can_speed_combo")
            .selected_text(selected_speed.display_name())
            .show_ui(ui, |ui| {
                for speed in speed_options {
                    if ui
                        .selectable_value(&mut app.can_bus_speed, speed, speed.display_name())
                        .changed()
                    {
                        app.serial_ports = util::get_available_serial_ports();
                        app.save_settings();
                    }
                }
            });
    });

    ui.horizontal_wrapped(|ui| {
        ui.label("UDP Port:");
        if ui
            .add(eframe::egui::DragValue::new(&mut app.udp_port).range(1..=65535))
            .changed()
        {
            app.save_settings();
        }
    });

    ui.horizontal_wrapped(|ui| {
        let selected_text = match &app.selected_source {
            Some(daqcore::connection::ConnectionSource::Serial(path, speed)) => {
                format!("Serial: {} ({})", path, speed.display_name())
            }
            Some(connection_source) => connection_source.display_name(),
            None => "Select Source".to_string(),
        };

        eframe::egui::ComboBox::from_label("Source")
            .selected_text(selected_text)
            .show_ui(ui, |ui| {
                ui.label("Serial Ports");
                let ports: Vec<_> = app
                    .serial_ports
                    .iter()
                    .map(|p| p.port_name.clone())
                    .collect();
                for port_name in ports {
                    let source = daqcore::connection::ConnectionSource::Serial(
                        port_name.clone(),
                        app.can_bus_speed,
                    );
                    if ui
                        .selectable_value(
                            &mut app.selected_source,
                            Some(source),
                            format!("{} ({})", port_name, app.can_bus_speed.display_name()),
                        )
                        .changed()
                    {
                        app.connect_can();
                        app.save_settings();
                    }
                }
                ui.separator();
                ui.label("CANnector");
                if app.cannector_available
                    && ui
                        .selectable_value(
                            &mut app.selected_source,
                            Some(daqcore::connection::ConnectionSource::Cannector),
                            "CANnector (USB)",
                        )
                        .changed()
                {
                    app.connect_can();
                    app.save_settings();
                }
                ui.separator();
                ui.label("Network");
                let udp_source = daqcore::connection::ConnectionSource::Udp(app.udp_port);
                if ui
                    .selectable_value(
                        &mut app.selected_source,
                        Some(udp_source),
                        format!("UDP ({})", app.udp_port),
                    )
                    .changed()
                {
                    app.connect_can();
                    app.save_settings();
                }
                ui.separator();
                ui.label("Simulated");
                let sim_sources = [
                    (true, "Simulated (connected)"),
                    (false, "Simulated (disconnected)"),
                ];

                for (connected, label) in sim_sources {
                    let sim_source = daqcore::connection::ConnectionSource::Simulated(connected);

                    if ui
                        .selectable_value(&mut app.selected_source, Some(sim_source), label)
                        .changed()
                    {
                        app.connect_can();
                        app.save_settings();
                    }
                }
                ui.separator();
                ui.label("Development");
                let loopback_source = daqcore::connection::ConnectionSource::Loopback;
                if ui
                    .selectable_value(&mut app.selected_source, Some(loopback_source), "Loopback")
                    .changed()
                {
                    app.connect_can();
                    app.save_settings();
                }
            });

        if ui
            .button("🔄")
            .on_hover_text("Refresh serial & USB ports")
            .clicked()
        {
            app.serial_ports = util::get_available_serial_ports();
            app.cannector_available = util::is_cannector_available();
        }
    });

    ui.horizontal_wrapped(|ui| {
        let (status_icon, status_color) = match &app.connection_status {
            app::ConnectionStatus::Disconnected => {
                ("■ Disconnected".to_string(), eframe::egui::Color32::GRAY)
            }
            app::ConnectionStatus::Connected => {
                ("✔ Connected".to_string(), eframe::egui::Color32::GREEN)
            }
            app::ConnectionStatus::Error(e) => {
                (format!("🔴 Error: {}", e), eframe::egui::Color32::RED)
            }
        };
        ui.label(eframe::egui::RichText::new(status_icon).color(status_color));
    });

    ui.separator();
    ui.label(
        eframe::egui::RichText::new("DATABASE & STORAGE")
            .small()
            .strong()
            .color(muted_color),
    );
    ui.add_space(2.0);

    ui.add_enabled_ui(!app.selection_pending(), |ui| {
        show_database_controls(ui, app);
    });
    if app.selection_pending() {
        ui.label("Applying database selection…");
    }
    if let Some(error) = &app.diagnostic {
        ui.colored_label(ui.visuals().error_fg_color, error);
    }

    ui.horizontal_wrapped(|ui| {
        if ui.button("📁 Select Log Folder").clicked()
            && let Some(path) = rfd::FileDialog::new().pick_folder()
        {
            let command = daqcore::can_thread::CanThreadCommand::UpdateLogFolder(path.clone());

            if let Err(error) = app.ui_to_can_tx.send(command) {
                log::error!("Failed to send log folder update: {error}");
                app.diagnostic = Some(error.to_string());
                return;
            }

            app.log_folder = Some(path.clone());
            app.action_queue.push(action::AppAction::AttachStore(path));
            app.save_settings();
        }

        let log_display = app
            .log_folder
            .clone()
            .unwrap_or_else(|| path::PathBuf::from(settings::DEFAULT_LOG_FOLDER));

        ui.add(
            eframe::egui::Label::new(
                eframe::egui::RichText::new(log_display.display().to_string())
                    .small()
                    .weak(),
            )
            .wrap(),
        );
    });

    ui.separator();
    if ui.small_button("Reload formatter").clicked() {
        app.value_formatter = app::load_formatter();
    }
}

fn show_database_controls(ui: &mut eframe::egui::Ui, app: &mut app::DAQApp) {
    if ui
        .button("📁 Select Live SuperDBC (Decode / TX)")
        .on_hover_text(
            "Select SuperDBC used for live CAN frame decoding and message transmission (past logs use embedded partition metadata)",
        )
        .clicked()
    {
        select_database(app);
    }
    let Some(selection) = &app.database else {
        ui.label("Live SuperDBC: None selected");
        return;
    };
    if let Some(source) = selection.source() {
        ui.label(
            eframe::egui::RichText::new(
                source
                    .path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy(),
            )
            .small()
            .weak(),
        );
    }
    let mut current_selection = selection.selection;
    let multi_bus_allowed = app.supports_multi_bus();
    eframe::egui::ComboBox::from_label("CAN Bus")
        .selected_text(current_selection.display_name(&selection.database))
        .show_ui(ui, |ui| {
            show_bus_options(
                ui,
                &selection.database,
                multi_bus_allowed,
                &mut current_selection,
            );
        });
    if current_selection != selection.selection {
        app.request_bus_selection(current_selection);
    }
}

fn show_bus_options(
    ui: &mut eframe::egui::Ui,
    database: &daqcore::superdbc::database::Database,
    multi_bus_allowed: bool,
    current_selection: &mut daqcore::can_thread::BusSelection,
) {
    let presets = [
        daqcore::can_thread::BusSelection::Vehicle,
        daqcore::can_thread::BusSelection::Charger,
    ];
    for preset in presets {
        let supported = multi_bus_allowed && preset.resolve_buses(database).is_some();
        ui.add_enabled_ui(supported, |ui| {
            ui.selectable_value(current_selection, preset, preset.display_name(database));
        });
    }
    ui.separator();
    for bus in database.buses() {
        let single = daqcore::can_thread::BusSelection::Single(bus.bus_id());
        ui.selectable_value(current_selection, single, single.display_name(database));
    }
}
