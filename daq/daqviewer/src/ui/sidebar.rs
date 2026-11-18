use crate::{action, app, assets, ui, widget_constructor};

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

pub fn show(
    app: &mut app::ViewerState,
    platform: &mut dyn app::ViewerPlatform,
    ctx: &eframe::egui::Context,
) {
    ui::timeline::show(app, platform, ctx);
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
                ui.add(
                    eframe::egui::Image::from_bytes(assets::PER_LOGO_PATH, assets::PER_LOGO_BYTES)
                        .max_size(eframe::egui::vec2(20.0, 20.0))
                        .corner_radius(3),
                )
                .on_hover_text("PER DAQ");

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

            // Quick Export / Share controls + FPS indicator at the bottom of the rail
            ui.with_layout(
                eframe::egui::Layout::bottom_up(eframe::egui::Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
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

                    let share_active = app.show_share_popover;
                    let share_btn = eframe::egui::Button::new(
                        eframe::egui::RichText::new("🔗").color(if share_active {
                            ui.visuals().text_color()
                        } else {
                            muted_color
                        }),
                    )
                    .fill(if share_active {
                        accent_color.gamma_multiply(0.22_f32)
                    } else {
                        eframe::egui::Color32::TRANSPARENT
                    })
                    .stroke(if share_active {
                        eframe::egui::Stroke::new(1.0_f32, accent_color)
                    } else {
                        eframe::egui::Stroke::NONE
                    })
                    .min_size(eframe::egui::vec2(26.0, 26.0));

                    if ui
                        .add(share_btn)
                        .on_hover_text(
                            "Share workspace view (.daqview.json, Share URL, or daqview1:... link)",
                        )
                        .clicked()
                    {
                        app.show_share_popover = !app.show_share_popover;
                    }

                    let export_btn = eframe::egui::Button::new(
                        eframe::egui::RichText::new("📤").color(muted_color),
                    )
                    .fill(eframe::egui::Color32::TRANSPARENT)
                    .stroke(eframe::egui::Stroke::NONE)
                    .min_size(eframe::egui::vec2(26.0, 26.0));

                    if ui
                        .add(export_btn)
                        .on_hover_text("Export dataset (Parquet / CSV / MATLAB .mat)")
                        .clicked()
                    {
                        app.action_queue
                            .push(action::AppAction::OpenExportDataModal);
                    }
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
        .default_width(330.0)
        .width_range(235.0..=420.0)
        .frame(drawer_frame)
        .show_animated(ctx, app.is_sidebar_open, |ui| {
            let content_rect = ui.max_rect();
            let panel_width = content_rect.width().max(1.0);
            let _ = ui.allocate_rect(content_rect, eframe::egui::Sense::hover());
            let mut ui = ui.new_child(
                eframe::egui::UiBuilder::new()
                    .max_rect(content_rect)
                    .layout(eframe::egui::Layout::top_down(eframe::egui::Align::Min)),
            );
            ui.set_clip_rect(content_rect.intersect(ui.clip_rect()));
            ui.set_width(panel_width);
            ui.set_max_width(panel_width);
            ui.style_mut().wrap_mode = Some(eframe::egui::TextWrapMode::Truncate);

            show_sidebar_header(&mut ui, app, platform, ctx, panel_width);
            ui.separator();

            match app.sidebar_tab {
                SidebarTab::Sessions => {
                    show_sessions_tab(&mut ui, app, muted_color);
                }
                SidebarTab::Channels => {
                    app.signal_browser.hover_utc_ms = app.hover_utc_ms;
                    eframe::egui::ScrollArea::vertical()
                        .id_salt("sidebar_channels_scroll")
                        .auto_shrink([false, false])
                        .show(&mut ui, |ui| {
                            ui.set_max_width(panel_width);
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
                        .show(&mut ui, |ui| {
                            ui.set_max_width(panel_width);
                            show_widgets_tab(
                                ui,
                                app,
                                platform.supports_hardware_widgets(),
                                muted_color,
                            );
                        });
                }
                SidebarTab::Settings => {
                    eframe::egui::ScrollArea::vertical()
                        .id_salt("sidebar_settings_scroll")
                        .auto_shrink([false, false])
                        .show(&mut ui, |ui| {
                            ui.set_max_width(panel_width);
                            show_settings_tab(ui, app, platform, muted_color);
                        });
                }
            }
        });
}

fn show_sidebar_header(
    ui: &mut eframe::egui::Ui,
    app: &mut app::ViewerState,
    platform: &mut dyn app::ViewerPlatform,
    ctx: &eframe::egui::Context,
    panel_width: f32,
) {
    let theme = ui::theme::get_theme(ctx);
    let accent_color = theme.accent_color();
    let muted_color = theme.muted_text_color();

    ui.set_max_width(panel_width);

    // Row 1: Track Day Selector
    let track_days = app.router.list_track_days().unwrap_or_default();
    let active_day_id = app
        .router
        .timeline()
        .active_track_day_id()
        .map(str::to_owned);
    let mut requested_day_change: Option<Option<String>> = None;

    let day_text = match active_day_id.as_deref() {
        Some(id) => track_days
            .iter()
            .find(|td| td.id == id)
            .map(|td| {
                if td.location.is_empty() {
                    td.id.clone()
                } else {
                    format!("{} ({})", td.id, td.location)
                }
            })
            .unwrap_or_else(|| id.to_owned()),
        None => "All Track Days".to_owned(),
    };
    eframe::egui::ComboBox::from_id_salt("header_track_day_selector")
        .width(panel_width)
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
                    .selectable_label(active_day_id.as_deref() == Some(td.id.as_str()), label)
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

    ui.add_space(2.0);

    // Row 2: Export, Share, and Theme selector
    ui.allocate_ui_with_layout(
        eframe::egui::vec2(panel_width, 20.0),
        eframe::egui::Layout::left_to_right(eframe::egui::Align::Center),
        |ui| {
            ui.set_max_width(panel_width);
            ui.spacing_mut().item_spacing.x = 4.0;

            if ui
                .small_button("📤 Export")
                .on_hover_text("Export dataset (Parquet / CSV / MATLAB .mat)")
                .clicked()
            {
                app.action_queue
                    .push(action::AppAction::OpenExportDataModal);
            }

            let share_btn = eframe::egui::Button::new(
                eframe::egui::RichText::new("🔗 Share")
                    .small()
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
                    "Share workspace view (.daqview.json, Share URL, or daqview1:... link)",
                )
                .clicked()
            {
                app.show_share_popover = !app.show_share_popover;
            }

            let mut next_theme = app.theme_selection;
            let theme_w = ui.available_width().max(54.0);
            eframe::egui::ComboBox::from_id_salt("header_theme_selector")
                .width(theme_w)
                .selected_text(
                    eframe::egui::RichText::new(format!(
                        "🎨 {}",
                        app.theme_selection.get_name()
                    ))
                    .small(),
                )
                .show_ui(ui, |ui| {
                    for candidate in ui::theme::ThemeSelection::ALL {
                        ui.selectable_value(&mut next_theme, candidate, candidate.get_name());
                    }
                });
            if next_theme != app.theme_selection {
                app.set_theme(ctx, next_theme);
                platform.save_settings(app);
            }
        },
    );

    // Row 3: Connection / Provenance & Cloud Sync status pills
    ui.allocate_ui_with_layout(
        eframe::egui::vec2(panel_width, 18.0),
        eframe::egui::Layout::left_to_right(eframe::egui::Align::Center),
        |ui| {
            ui.set_max_width(panel_width);
            ui.spacing_mut().item_spacing.x = 6.0;

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
                .on_hover_text("Click to open Connection & Cloud settings")
                .clicked()
            {
                app.sidebar_tab = SidebarTab::Settings;
                app.is_sidebar_open = true;
            }

            match app.router.cloud_status() {
                daqcore::storage::CloudSyncStatus::Connected {
                    pending_downloads,
                    pending_uploads,
                    ws_connected,
                    ..
                } => {
                    let (cloud_label, cloud_color) =
                        if pending_downloads > 0 || pending_uploads > 0 {
                            (
                                format!(
                                    "☁ Syncing (dl:{} ul:{})",
                                    pending_downloads, pending_uploads
                                ),
                                accent_color,
                            )
                        } else if ws_connected {
                            (
                                "☁ Cloud Live".to_owned(),
                                eframe::egui::Color32::from_rgb(34, 197, 94),
                            )
                        } else {
                            ("☁ Cloud HTTP".to_owned(), muted_color)
                        };
                    if ui
                        .add(
                            eframe::egui::Button::new(
                                eframe::egui::RichText::new(cloud_label)
                                    .small()
                                    .color(cloud_color),
                            )
                            .frame(false),
                        )
                        .on_hover_text("daqcloud sync active — click to open settings")
                        .clicked()
                    {
                        app.sidebar_tab = SidebarTab::Settings;
                        app.is_sidebar_open = true;
                    }
                }
                daqcore::storage::CloudSyncStatus::Connecting { .. } => {
                    if ui
                        .add(
                            eframe::egui::Button::new(
                                eframe::egui::RichText::new("☁ Connecting…")
                                    .small()
                                    .color(muted_color),
                            )
                            .frame(false),
                        )
                        .clicked()
                    {
                        app.sidebar_tab = SidebarTab::Settings;
                        app.is_sidebar_open = true;
                    }
                }
                daqcore::storage::CloudSyncStatus::Error { error, .. } => {
                    if ui
                        .add(
                            eframe::egui::Button::new(
                                eframe::egui::RichText::new("☁ Cloud Error")
                                    .small()
                                    .color(eframe::egui::Color32::from_rgb(239, 68, 68)),
                            )
                            .frame(false),
                        )
                        .on_hover_text(error)
                        .clicked()
                    {
                        app.sidebar_tab = SidebarTab::Settings;
                        app.is_sidebar_open = true;
                    }
                }
                daqcore::storage::CloudSyncStatus::Disconnected => {}
            }
        },
    );
}

fn show_share_popover(app: &mut app::ViewerState, ctx: &eframe::egui::Context) {
    if !app.show_share_popover {
        return;
    }

    let mut keep_open = true;
    eframe::egui::Window::new("Share Workspace")
        .id(eframe::egui::Id::new("nominal_share_popover"))
        .anchor(eframe::egui::Align2::LEFT_TOP, [40.0, 36.0])
        .collapsible(false)
        .resizable(false)
        .default_width(310.0)
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
            ui.horizontal_wrapped(|ui| {
                if ui
                    .button("🔗 Copy Share URL")
                    .on_hover_text("Copy direct web link (/daqview/daqview1:...) that opens this exact workspace")
                    .clicked()
                {
                    app.action_queue
                        .push(action::AppAction::CopyWorkspaceShareUrl);
                }
                if ui
                    .button("📋 Copy Code")
                    .on_hover_text("Encode current workspace layout and copy daqview1:... share code")
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

            let code_to_display = if !app.workspace_view_modal.generated_share_url.is_empty() {
                &mut app.workspace_view_modal.generated_share_url
            } else {
                &mut app.workspace_view_modal.generated_share_string
            };

            if !code_to_display.is_empty() {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label(
                        eframe::egui::RichText::new("Share Link / Code (Ctrl+C):")
                            .small()
                            .weak(),
                    );
                    if ui.small_button("Select All").clicked() {
                        app.workspace_view_modal.select_all_share_code = true;
                    }
                });
                let text_output = eframe::egui::TextEdit::multiline(code_to_display)
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
                    let char_len = code_to_display.chars().count();
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
    app: &mut app::ViewerState,
    _muted_color: eframe::egui::Color32,
) {
    ui.set_max_width(ui.available_width().max(1.0));
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
    app: &mut app::ViewerState,
    supports_hardware_widgets: bool,
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
                "YT Plot (Time / Dist)",
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
        eframe::egui::RichText::new(if supports_hardware_widgets {
            "OPERATIONAL & HARDWARE TOOLS"
        } else {
            "INSPECTION TOOLS"
        })
        .small()
        .strong()
        .color(muted_color),
    );
    ui.add_space(2.0);
    let mut tools = vec![
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
    ];
    if supports_hardware_widgets {
        tools.extend([
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
        ]);
    }
    egui_grid_buttons(ui, &mut app.action_queue, &tools);

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
        let btn = eframe::egui::Button::new(
            eframe::egui::RichText::new(format!("📁 {preset_name}")).small(),
        )
        .truncate();
        if ui
            .add_sized([ui.available_width(), 22.0], btn)
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
    app: &mut app::ViewerState,
    platform: &mut dyn app::ViewerPlatform,
    muted_color: eframe::egui::Color32,
) {
    platform.show_hardware_settings_ui(ui, app, muted_color);

    if let Some(error) = &app.diagnostic {
        ui.colored_label(ui.visuals().error_fg_color, error);
    }

    ui.label(
        eframe::egui::RichText::new("DAQCLOUD SERVER SYNC")
            .small()
            .strong()
            .color(muted_color),
    );
    ui.add_space(2.0);

    ui.horizontal(|ui| {
        ui.label("Server URL:");
        let resp = ui.add(
            eframe::egui::TextEdit::singleline(&mut app.cloud_url)
                .hint_text("http://127.0.0.1:3000")
                .desired_width(ui.available_width().max(80.0)),
        );
        if resp.lost_focus() {
            platform.save_settings(app);
        }
    });

    if ui
        .checkbox(&mut app.cloud_auto_connect, "Auto-connect on startup")
        .changed()
    {
        platform.save_settings(app);
    }

    let cloud_status = app.router.cloud_status();
    ui.horizontal_wrapped(|ui| {
        let is_active = !matches!(
            cloud_status,
            daqcore::storage::CloudSyncStatus::Disconnected
        );
        if is_active {
            if ui.button("■ Disconnect").clicked() {
                app.action_queue.push(action::AppAction::DisconnectCloud);
            }
            if ui
                .button("🔄 Sync Now")
                .on_hover_text(
                    "Fetch catalog, upload new local SD partitions, and sync annotations",
                )
                .clicked()
            {
                app.action_queue.push(action::AppAction::SyncCloudNow);
            }
        } else if ui.button("☁ Connect to daqcloud").clicked() {
            app.action_queue
                .push(action::AppAction::ConnectCloud(app.cloud_url.clone()));
        }
    });

    match cloud_status {
        daqcore::storage::CloudSyncStatus::Disconnected => {
            ui.label(
                eframe::egui::RichText::new("■ Cloud Disconnected")
                    .small()
                    .color(eframe::egui::Color32::GRAY),
            );
        }
        daqcore::storage::CloudSyncStatus::Connecting { url } => {
            ui.label(
                eframe::egui::RichText::new(format!("• Connecting to {url}…"))
                    .small()
                    .color(muted_color),
            );
        }
        daqcore::storage::CloudSyncStatus::Connected {
            url,
            synced_partitions,
            pending_downloads,
            pending_uploads,
            ws_connected,
        } => {
            let ws_badge = if ws_connected { "WS Live" } else { "HTTP Only" };
            ui.label(
                eframe::egui::RichText::new(format!(
                    "✔ Connected ({ws_badge}) — {synced_partitions} partitions synced (dl:{pending_downloads} ul:{pending_uploads})"
                ))
                .small()
                .color(eframe::egui::Color32::GREEN),
            )
            .on_hover_text(url);
        }
        daqcore::storage::CloudSyncStatus::Error { url, error } => {
            ui.label(
                eframe::egui::RichText::new(format!("🔴 Cloud Error ({url}): {error}"))
                    .small()
                    .color(eframe::egui::Color32::RED),
            );
        }
    }

    ui.separator();
    if ui.small_button("Reload formatter").clicked() {
        app.value_formatter = platform.reload_formatter();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{NoopViewerPlatform, ViewerState};

    #[test]
    fn sidebar_width_stays_identical_across_all_tabs() {
        let ctx = eframe::egui::Context::default();
        let mut state = ViewerState::new(
            daqcore::router::VirtualTimelineRouter::live(
                daqcore::time::Time::from_unix_millis(1_794_500_000_000),
                30.0,
            )
            .expect("live router"),
            ui::theme::ThemeSelection::default(),
            None,
            String::new(),
            false,
            None,
            None,
        );
        state.is_sidebar_open = true;
        let mut platform = NoopViewerPlatform;

        let mut recorded_lefts = Vec::new();
        for tab in [
            SidebarTab::Channels,
            SidebarTab::Sessions,
            SidebarTab::Widgets,
            SidebarTab::Settings,
            SidebarTab::Channels,
        ] {
            state.sidebar_tab = tab;
            for _ in 0..2 {
                let raw = eframe::egui::RawInput {
                    screen_rect: Some(eframe::egui::Rect::from_min_size(
                        eframe::egui::Pos2::ZERO,
                        eframe::egui::vec2(1280.0, 800.0),
                    )),
                    ..Default::default()
                };
                let mut central_left = 0.0_f32;
                let _ = ctx.run(raw, |ctx| {
                    show(&mut state, &mut platform, ctx);
                    central_left = ctx.available_rect().left();
                });
                recorded_lefts.push((tab, central_left));
            }
        }

        let first_left = recorded_lefts[0].1;
        for (tab, left) in &recorded_lefts {
            assert!(
                (*left - first_left).abs() < 0.1,
                "Sidebar width changed on tab {tab:?}: expected {first_left}, got {left} ({recorded_lefts:?})"
            );
        }
    }
}
