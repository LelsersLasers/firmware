use crate::{action, app, telemetry, ui, widgets};
use std::sync::mpsc;

pub fn show(app: &mut app::DAQApp, ctx: &eframe::egui::Context) {
    ui::timeline::show(app, ctx);
    app.router.poll_background_tiles();
    let start = app.router.timeline().start();
    let removed = app
        .bus_load_samples
        .partition_point(|s| s.timestamp < start);
    app.bus_load_samples.drain(..removed);
    let selection_pending = app.selection_pending();
    let view = telemetry::TelemetryView::new(&app.router);
    eframe::egui::CentralPanel::default().show(ctx, |ui| {
        if app.tile_tree.is_empty() {
            ui.vertical_centered(|ui| {
                ui.label("No widgets in workspace yet.");
                ui.label("CMD+S to toggle the sidebar.");
                ui.label("CMD+P to toggle the command palette.");
                ui.label("Drag any signal from the Signal Browser to open a Scope.");
            });
        } else {
            let mut behavior = WorkspaceTileBehavior {
                view: &view,
                bus_load: &app.bus_load_samples,
                action_queue: &mut app.action_queue,
                database: app.database.as_ref(),
                ui_to_can_tx: app.ui_to_can_tx.clone(),
                formatter: &app.value_formatter,
            };
            ui.add_enabled_ui(!selection_pending, |ui| {
                app.tile_tree.ui(&mut behavior, ui);
            });
        }

        let workspace_rect = ui.max_rect();
        if eframe::egui::DragAndDrop::payload::<ui::signal_browser::DraggedSignal>(ctx).is_some()
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
                && let Some(payload) =
                    eframe::egui::DragAndDrop::take_payload::<ui::signal_browser::DraggedSignal>(
                        ctx,
                    )
            {
                app.action_queue
                    .push(action::AppAction::SpawnWidget(payload.to_widget_constructor()));
            }
        }
    });
}

struct WorkspaceTileBehavior<'a> {
    view: &'a telemetry::TelemetryView<'a>,
    bus_load: &'a [telemetry::BusLoadSample],
    action_queue: &'a mut Vec<action::AppAction>,
    database: Option<&'a app::DatabaseSelection>,
    ui_to_can_tx: mpsc::Sender<daqcore::can_thread::CanThreadCommand>,
    formatter: &'a Option<daqcore::formatter::Formatter>,
}

impl egui_tiles::Behavior<widgets::Widget> for WorkspaceTileBehavior<'_> {
    fn pane_ui(
        &mut self,
        ui: &mut eframe::egui::Ui,
        _tile_id: egui_tiles::TileId,
        widget: &mut widgets::Widget,
    ) -> egui_tiles::UiResponse {
        ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));
        eframe::egui::Frame::NONE
            .inner_margin(eframe::egui::Margin::same(6))
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

    fn tab_title_for_pane(&mut self, widget: &widgets::Widget) -> eframe::egui::WidgetText {
        widget.title().into()
    }

    fn tab_bar_color(&self, visuals: &eframe::egui::Visuals) -> eframe::egui::Color32 {
        visuals.window_fill
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
            widget_constructor::WidgetConstructor::LogParser,
            widget_constructor::WidgetConstructor::SendUi,
            widget_constructor::WidgetConstructor::BusLoad,
            widget_constructor::WidgetConstructor::BatteryVoltage,
            widget_constructor::WidgetConstructor::BatteryTemps,
            widget_constructor::WidgetConstructor::GgPlot,
            widget_constructor::WidgetConstructor::GpsPlot,
            widget_constructor::WidgetConstructor::Dynamics,
            widget_constructor::WidgetConstructor::Jitter,
            widget_constructor::WidgetConstructor::Hil,
        ];
        let mut list: Vec<widgets::Widget> = constructors
            .into_iter()
            .filter_map(|c| c.create(&mut ids, tx.clone(), 0))
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

        let bus_load = vec![telemetry::BusLoadSample {
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
        assert!(all_widgets.len() >= 14);

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
                    let view = telemetry::TelemetryView::new(&router);
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
            let view = telemetry::TelemetryView::new(&router);
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
                        };
                        tree.ui(&mut behavior, ui);
                    });
            });
        }
    }
}
