use crate::{action, app, assets, settings, ui, util, widget_constructor};
use std::path;

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
    eframe::egui::SidePanel::left("left_sidebar")
        .resizable(true)
        .default_width(250.0)
        .width_range(190.0..=480.0)
        .show_animated(ctx, app.is_sidebar_open, |ui| {
            let panel_width = ui.available_width();
            ui.set_max_width(panel_width);
            eframe::egui::TopBottomPanel::bottom("sidebar_fps_footer")
                .exact_height(22.0)
                .show_inside(ui, |ui| {
                    ui.horizontal_centered(|ui| {
                        let fps_color = if app.fps >= 50.0 {
                            eframe::egui::Color32::from_rgb(90, 205, 120)
                        } else if app.fps >= 25.0 {
                            eframe::egui::Color32::from_rgb(235, 190, 75)
                        } else {
                            eframe::egui::Color32::from_rgb(235, 95, 95)
                        };
                        ui.label(
                            eframe::egui::RichText::new(format!(
                                "FPS: {:.1} ({:.1} ms)",
                                app.fps, app.frame_time_ms
                            ))
                            .monospace()
                            .small()
                            .color(fps_color),
                        );
                    });
                });
            eframe::egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_max_width(ui.available_width());
                    show_sidebar_contents(ui, app);
                });
        });
}

fn show_sidebar_contents(ui: &mut eframe::egui::Ui, app: &mut app::DAQApp) {
    ui.horizontal(|ui| {
        ui.add(
            eframe::egui::Image::from_bytes(assets::PER_LOGO_PATH, assets::PER_LOGO_BYTES)
                .show_loading_spinner(true)
                .corner_radius(5),
        );
        ui.heading("Side bar");
    });
    ui.separator();

    let theme_label = format!("🎨 Theme: {}", app.theme_selection.get_name());

    if ui.button(theme_label).clicked() {
        app.toggle_theme(ui.ctx());
        app.save_settings();
    }

    ui.separator();

    eframe::egui::CollapsingHeader::new("📅 Sessions, Stints & Chunks")
        .id_salt("sidebar_chunk_navigator")
        .default_open(false)
        .show(ui, |ui| {
            ui::timeline::show_chunk_navigator_contents(
                ui,
                &mut app.router,
                &mut app.timeline_ui,
                &mut app.diagnostic,
                240.0,
            );
        });

    ui.separator();

    ui::signal_browser::show_signal_browser(
        ui,
        &mut app.signal_browser,
        app.database.as_ref(),
        &mut app.router,
        &mut app.action_queue,
        &mut app.diagnostic,
    );

    ui.separator();
    ui.heading("Widgets");

    if ui.button("Add CAN Viewer Table").clicked() {
        app.action_queue.push(action::AppAction::SpawnWidget(
            widget_constructor::WidgetConstructor::ViewerTable,
        ));
    }

    if ui.button("Add CAN Viewer List").clicked() {
        app.action_queue.push(action::AppAction::SpawnWidget(
            widget_constructor::WidgetConstructor::ViewerList,
        ));
    }

    if ui.button("Add Scope").clicked() {
        app.action_queue.push(action::AppAction::SpawnWidget(
            widget_constructor::WidgetConstructor::ScopeEmpty,
        ));
    }

    if ui.button("Add State Lanes").clicked() {
        app.action_queue.push(action::AppAction::SpawnWidget(
            widget_constructor::WidgetConstructor::StateLane,
        ));
    }

    if ui.button("Add XY Scatter").clicked() {
        app.action_queue.push(action::AppAction::SpawnWidget(
            widget_constructor::WidgetConstructor::XYScatter,
        ));
    }

    if ui.button("Add GPS Plot").clicked() {
        app.action_queue.push(action::AppAction::SpawnWidget(
            widget_constructor::WidgetConstructor::GpsPlot,
        ));
    }

    if ui.button("Add Bootloader").clicked() {
        app.action_queue.push(action::AppAction::SpawnWidget(
            widget_constructor::WidgetConstructor::Bootloader,
        ));
    }

    if ui.button("Add Log Parser").clicked() {
        app.action_queue.push(action::AppAction::SpawnWidget(
            widget_constructor::WidgetConstructor::LogParser,
        ));
    }

    if ui.button("Add Message Sender").clicked() {
        app.action_queue.push(action::AppAction::SpawnWidget(
            widget_constructor::WidgetConstructor::SendUi,
        ));
    }

    if ui.button("Add Bus Load").clicked() {
        app.action_queue.push(action::AppAction::SpawnWidget(
            widget_constructor::WidgetConstructor::BusLoad,
        ));
    }

    if ui.button("Add Jitter").clicked() {
        app.action_queue.push(action::AppAction::SpawnWidget(
            widget_constructor::WidgetConstructor::Jitter,
        ));
    }

    if ui.button("Add HIL").clicked() {
        app.action_queue.push(action::AppAction::SpawnWidget(
            widget_constructor::WidgetConstructor::Hil,
        ));
    }

    ui.separator();
    ui.heading("Subteam Workspaces & Presets");

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

    eframe::egui::CollapsingHeader::new("Quick Single-Pane Presets")
        .id_salt("sidebar_single_pane_presets")
        .default_open(false)
        .show(ui, |ui| {
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
        });

    ui.separator();
    ui.heading("Shared Views & Export");

    if ui
        .button("📁 Save / Import View (.daqview.json)...")
        .on_hover_text(
            "Save or load a .daqview.json workspace layout or paste a daqview1:... share string",
        )
        .clicked()
    {
        app.action_queue
            .push(action::AppAction::OpenWorkspaceViewModal);
    }

    if ui
        .button("📋 Copy Share String (daqview1:...)")
        .on_hover_text(
            "Encode current workspace layout, custom formulas, and time range (shows raw code for manual copy if clipboard is blocked)",
        )
        .clicked()
    {
        app.action_queue
            .push(action::AppAction::CopyWorkspaceShareString);
    }

    if app.workspace_view_modal.show_sidebar_share_code
        && !app.workspace_view_modal.generated_share_string.is_empty()
    {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                eframe::egui::RichText::new("Raw Share Code (Ctrl+C):")
                    .small()
                    .weak(),
            );
            if ui.small_button("Select All").clicked() {
                app.workspace_view_modal.select_all_share_code = true;
            }
            if ui.small_button("Hide").clicked() {
                app.workspace_view_modal.show_sidebar_share_code = false;
            }
        });
        let text_output =
            eframe::egui::TextEdit::multiline(&mut app.workspace_view_modal.generated_share_string)
                .font(eframe::egui::TextStyle::Monospace)
                .desired_rows(2)
                .desired_width(f32::INFINITY)
                .show(ui);
        if app.workspace_view_modal.select_all_share_code && !app.workspace_view_modal.is_open {
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

    if ui
        .button("📤 Export Dataset (Parquet / CSV / .mat)...")
        .on_hover_text("Export physical signals and derived formulas for MATLAB or Python")
        .clicked()
    {
        app.action_queue
            .push(action::AppAction::OpenExportDataModal);
    }

    ui.separator();
    ui.heading("Connection Settings");

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
                            Some(source.clone()),
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
                        Some(udp_source.clone()),
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
                        .selectable_value(&mut app.selected_source, Some(sim_source.clone()), label)
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

        if ui.button("🔄").clicked() {
            app.serial_ports = util::get_available_serial_ports();
            app.cannector_available = util::is_cannector_available();
        }
    });

    ui.horizontal_wrapped(|ui| {
        // Connection status indicator
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
        if ui.button("Select Log Folder").clicked()
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

        ui.add(eframe::egui::Label::new(log_display.display().to_string()).wrap());
    });

    ui.separator();

    if ui.button("Reload formatter").clicked() {
        app.value_formatter = app::load_formatter();
    }
}

fn show_database_controls(ui: &mut eframe::egui::Ui, app: &mut app::DAQApp) {
    if ui.button("📁 Select SuperDBC").clicked() {
        select_database(app);
    }
    let Some(selection) = &app.database else {
        ui.label("SuperDBC: None selected");
        return;
    };
    if let Some(source) = selection.source() {
        ui.label(
            source
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy(),
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
