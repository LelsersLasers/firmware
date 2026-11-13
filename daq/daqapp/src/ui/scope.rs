use crate::{app, telemetry, ui};
use std::{fs, mem};

// Makes invalid combinations of id/name/signal name unrepresentable
enum ScopeState {
    PickingMessage {
        picker: ui::message_picker::MessagePickerState,
    },
    PickingSignal {
        selected_msg: daqcore::superdbc::message::MessageKey,
    },
    Configured {
        key: daqcore::superdbc::message::MessageKey,
        msg_name: String,
        signal_name: String,
    },
    ConfiguredDerived {
        signal_id: String,
        label: String,
        unit: String,
    },
}

impl Default for ScopeState {
    fn default() -> Self {
        ScopeState::PickingMessage {
            picker: ui::message_picker::MessagePickerState::default(),
        }
    }
}

pub struct Scope {
    pub title: String,
    instance_num: usize,
    state: ScopeState,
}

impl Scope {
    pub fn new(
        instance_num: usize,
        key: daqcore::superdbc::message::MessageKey,
        msg_name: String,
        signal_name: String,
    ) -> Self {
        Self {
            title: format!("Scope: {signal_name}"),
            instance_num,
            state: ScopeState::Configured {
                key,
                msg_name,
                signal_name,
            },
        }
    }

    pub fn new_empty(instance_num: usize) -> Self {
        Self {
            title: format!("Scope #{instance_num}"),
            instance_num,
            state: ScopeState::default(),
        }
    }

    fn show_picker(
        &mut self,
        ui: &mut eframe::egui::Ui,
        database: Option<&app::DatabaseSelection>,
        formula_engine: &daqcore::analysis::FormulaEngine,
    ) -> bool {
        let mut picked_derived: Option<(String, String, String)> = None;
        if matches!(self.state, ScopeState::PickingMessage { .. }) {
            eframe::egui::CollapsingHeader::new(
                eframe::egui::RichText::new("Derived Signals (Formulas)").strong(),
            )
            .default_open(false)
            .show(ui, |ui| {
                for compiled in formula_engine.definitions() {
                    let def = &compiled.def;
                    let unit_suffix = if def.unit.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", def.unit)
                    };
                    let btn_label = format!("{} — {}{}", def.id, def.label, unit_suffix);
                    if ui.button(btn_label).clicked() {
                        picked_derived =
                            Some((def.id.clone(), def.label.clone(), def.unit.clone()));
                    }
                }
            });
            ui.separator();
        }

        if let Some((signal_id, label, unit)) = picked_derived {
            self.title = format!("Scope: {signal_id}");
            self.state = ScopeState::ConfiguredDerived {
                signal_id,
                label,
                unit,
            };
            return true;
        }

        let Some(database) = database else {
            ui::message_picker::no_database_placeholder(ui);
            return false;
        };

        let state = mem::take(&mut self.state);

        let (new_state, just_configured) = match state {
            ScopeState::PickingMessage { mut picker } => {
                let picked = picker.show(ui, database, true);
                match picked {
                    Some(msg) => (ScopeState::PickingSignal { selected_msg: msg }, false),
                    None => (ScopeState::PickingMessage { picker }, false),
                }
            }
            ScopeState::PickingSignal { selected_msg } => {
                let Some(message) = database.database.message_by_key(selected_msg) else {
                    self.state = ScopeState::default();
                    return false;
                };
                let bus_name = database.bus_name(selected_msg.bus_id);
                let identity = message.identity();

                ui.separator();
                ui.label(
                    eframe::egui::RichText::new(format!(
                        "Selected Message: [{bus_name}] {} (0x{identity:03X}) — pick a signal:",
                        message.name()
                    ))
                    .strong(),
                );

                let mut picked_signal = None;
                for sig in message.signals() {
                    if ui.button(sig.name()).clicked() {
                        picked_signal = Some(sig.name().to_owned());
                        break;
                    }
                }

                // Choosing a signal configures the query against shared history.
                if let Some(signal_name) = picked_signal {
                    (
                        ScopeState::Configured {
                            key: selected_msg,
                            msg_name: message.name().to_owned(),
                            signal_name,
                        },
                        true,
                    )
                } else if ui.button("← Back to message search").clicked() {
                    (
                        ScopeState::PickingMessage {
                            picker: ui::message_picker::MessagePickerState::default(),
                        },
                        false,
                    )
                } else {
                    (ScopeState::PickingSignal { selected_msg }, false)
                }
            }
            configured @ (ScopeState::Configured { .. } | ScopeState::ConfiguredDerived { .. }) => {
                (configured, false)
            }
        };

        self.state = new_state;

        if just_configured && let ScopeState::Configured { signal_name, .. } = &self.state {
            self.title = format!("Scope: {}", signal_name);
        }

        just_configured
    }

    pub fn reset_database(&mut self) {
        if !matches!(self.state, ScopeState::ConfiguredDerived { .. }) {
            self.state = ScopeState::default();
            self.title = format!("Scope #{}", self.instance_num);
        }
    }

    pub fn show(
        &mut self,
        ui: &mut eframe::egui::Ui,
        database: Option<&app::DatabaseSelection>,
        view: &telemetry::TelemetryView<'_>,
    ) -> egui_tiles::UiResponse {
        if !matches!(
            self.state,
            ScopeState::Configured { .. } | ScopeState::ConfiguredDerived { .. }
        ) {
            self.show_picker(ui, database, view.router.formula_engine());
            return egui_tiles::UiResponse::None;
        }

        let (signal_id, signal, heading_text, y_label) = match &self.state {
            ScopeState::Configured {
                key,
                msg_name,
                signal_name,
            } => {
                let target_key = *key;
                let identity = target_key.identity;
                let bus_name = database.map_or("Unknown", |db| db.bus_name(target_key.bus_id));
                let sig_id = daqcore::storage::format_signal_id(msg_name, signal_name);
                let heading = format!(
                    "{}: [{bus_name}] {msg_name} (0x{identity:03X}) - {signal_name}",
                    self.title
                );
                (sig_id, signal_name.clone(), heading, signal_name.clone())
            }
            ScopeState::ConfiguredDerived {
                signal_id,
                label,
                unit,
            } => {
                let y_axis = if unit.is_empty() {
                    signal_id.clone()
                } else {
                    format!("{signal_id} ({unit})")
                };
                let heading = format!("{}: [Derived] {label} ({signal_id})", self.title);
                (signal_id.clone(), signal_id.clone(), heading, y_axis)
            }
            _ => return egui_tiles::UiResponse::None,
        };

        let start = view.timeline.start();
        let start_ms = start.unix_millis();
        let duration = view.timeline.end().secs(start).max(0.001);
        let playhead = view.timeline.setpoint().secs(start);

        let width_px = ui.available_width().max(1.0);
        let resolution = view.timeline.resolution_for_width(width_px);
        let series = view.query_signal(&signal_id, resolution);

        let base_staleness_ms = view.router.formula_engine().max_staleness_ms();
        let max_gap_ms = resolution
            .bucket_width_ms()
            .map_or(base_staleness_ms, |bw| base_staleness_ms.max(bw * 3));

        let (points, envelope_min_max) = match &series {
            daqcore::router::SignalSeries::Exact {
                timestamps_ms,
                values,
            } => {
                let pts = telemetry::points_with_gap_breaks(
                    timestamps_ms.iter().copied().zip(values.iter().copied()),
                    start_ms,
                    max_gap_ms,
                );
                (pts, None)
            }
            daqcore::router::SignalSeries::Envelope(buckets) => {
                let mean_pts = telemetry::points_with_gap_breaks(
                    buckets.iter().map(|b| (b.utc_ms, b.mean)),
                    start_ms,
                    max_gap_ms,
                );
                let has_spread = buckets.iter().any(|b| (b.max - b.min).abs() > 1e-9);
                let min_max = has_spread.then(|| {
                    let min_pts = telemetry::points_with_gap_breaks(
                        buckets.iter().map(|b| (b.utc_ms, b.min)),
                        start_ms,
                        max_gap_ms,
                    );
                    let max_pts = telemetry::points_with_gap_breaks(
                        buckets.iter().map(|b| (b.utc_ms, b.max)),
                        start_ms,
                        max_gap_ms,
                    );
                    (min_pts, max_pts)
                });
                (mean_pts, min_max)
            }
        };

        let mut change = false;
        ui.horizontal(|ui| {
            ui.heading(&heading_text);
            change = ui.button("Change signal").clicked();
            if ui.button("Export CSV").clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .set_file_name(format!("{}_data.csv", signal))
                    .add_filter("CSV", &["csv"])
                    .save_file()
            {
                let mut text = String::from("Time_Seconds,Value\n");
                for p in &points {
                    text.push_str(&format!("{},{}\n", p[0], p[1]));
                }

                if let Err(e) = fs::write(path, text) {
                    log::error!("Export failed: {e}");
                }
            }
        });
        if change {
            self.state = ScopeState::default();
            self.title = format!("Scope #{}", self.instance_num);
            return egui_tiles::UiResponse::None;
        }

        if points.is_empty() {
            ui.label("No retained samples in the selected interval.");
        }

        let pixels = width_px as usize;
        let points = telemetry::decimate(&points, pixels);

        let trace_color = eframe::egui::Color32::from_rgb(80, 190, 255);
        let envelope_color = eframe::egui::Color32::from_rgba_unmultiplied(80, 190, 255, 90);

        egui_plot::Plot::new(&self.title)
            .legend(egui_plot::Legend::default())
            .view_aspect(2.0)
            .allow_zoom([false, true])
            .allow_drag([false, true])
            .allow_scroll([false, true])
            .x_axis_formatter(|mark, _| {
                let offset_ms = (mark.value * 1000.0).round() as i64;
                start.offset(offset_ms).label()
            })
            .x_axis_label("Time")
            .y_axis_label(&y_label)
            .show(ui, |plot| {
                plot.set_plot_bounds_x(0.0..=duration);
                if let Some((min_pts, max_pts)) = envelope_min_max {
                    let min_pts = telemetry::decimate(&min_pts, pixels);
                    let max_pts = telemetry::decimate(&max_pts, pixels);
                    plot.line(
                        egui_plot::Line::new(
                            format!("{signal} (bucket max)"),
                            egui_plot::PlotPoints::from(max_pts),
                        )
                        .color(envelope_color),
                    );
                    plot.line(
                        egui_plot::Line::new(
                            format!("{signal} (bucket min)"),
                            egui_plot::PlotPoints::from(min_pts),
                        )
                        .color(envelope_color),
                    );
                }
                plot.line(
                    egui_plot::Line::new(&signal, egui_plot::PlotPoints::from(points))
                        .color(trace_color),
                );
                plot.vline(
                    egui_plot::VLine::new("Playhead", playhead)
                        .color(eframe::egui::Color32::YELLOW),
                );
            });
        egui_tiles::UiResponse::None
    }
}
