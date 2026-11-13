use crate::{app, telemetry};

pub struct ViewerList {
    pub title: String,
}

impl ViewerList {
    pub fn new(instance: usize) -> Self {
        Self {
            title: format!("CAN Viewer List #{instance}"),
        }
    }

    pub fn show(
        &self,
        ui: &mut eframe::egui::Ui,
        formatter: &Option<daqcore::formatter::Formatter>,
        database: Option<&app::DatabaseSelection>,
        view: &telemetry::TelemetryView<'_>,
    ) -> egui_tiles::UiResponse {
        ui.heading(&self.title);
        if view.setpoint_frames.is_empty() && view.router.partitions().is_empty() {
            ui.label("No retained CAN messages in the selected interval.");
        }
        eframe::egui::ScrollArea::horizontal()
            .id_salt((&self.title, "viewer_list_hscroll"))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui_extras::TableBuilder::new(ui)
                    .striped(true)
                    .auto_shrink([false, false])
                    .column(
                        egui_extras::Column::initial(95.0)
                            .at_least(65.0)
                            .resizable(true)
                            .clip(true),
                    )
                    .column(
                        egui_extras::Column::initial(55.0)
                            .at_least(40.0)
                            .resizable(true)
                            .clip(true),
                    )
                    .column(
                        egui_extras::Column::initial(165.0)
                            .at_least(85.0)
                            .resizable(true)
                            .clip(true),
                    )
                    .column(
                        egui_extras::Column::initial(145.0)
                            .at_least(80.0)
                            .resizable(true)
                            .clip(true),
                    )
                    .column(
                        egui_extras::Column::remainder()
                            .at_least(80.0)
                            .resizable(true)
                            .clip(true),
                    )
                    .header(20.0, |mut header| {
                        for text in ["Timestamp", "Bus", "Message (ID)", "Signal", "Value"] {
                            header.col(|ui| {
                                ui.strong(text);
                            });
                        }
                    })
                    .body(|mut body| {
                        if view.setpoint_frames.is_empty()
                            && let Some(db) = database
                        {
                            let mut rows = Vec::new();
                            for message in db.database.messages() {
                                let key = message.key();
                                let bus_name = db.bus_name(key.bus_id);
                                for sig_def in message.signals() {
                                    let signal_id = daqcore::storage::format_signal_id(
                                        message.name(),
                                        sig_def.name(),
                                    );
                                    if let Some((utc_ms, val)) =
                                        view.latest_signal_value_at_setpoint(&signal_id)
                                    {
                                        let unit = sig_def.unit();
                                        let formatted = if unit.is_empty() {
                                            format!("{val:.3}")
                                        } else {
                                            format!("{val:.3} {unit}")
                                        };
                                        rows.push((
                                            utc_ms,
                                            bus_name,
                                            message.name(),
                                            key.identity,
                                            sig_def.name(),
                                            formatted,
                                        ));
                                    }
                                }
                            }
                            rows.sort_by_key(|b| std::cmp::Reverse(b.0));
                            for (utc_ms, bus_name, msg_name, identity, sig_name, value) in
                                rows.into_iter().take(200)
                            {
                                body.row(18.0, |mut row| {
                                    row.col(|ui| {
                                        ui.label(
                                            daqcore::time::Time::from_unix_millis(utc_ms).label(),
                                        );
                                    });
                                    row.col(|ui| {
                                        ui.label(bus_name);
                                    });
                                    row.col(|ui| {
                                        ui.label(format!("{msg_name} (0x{identity:X})"));
                                    });
                                    row.col(|ui| {
                                        ui.label(sig_name);
                                    });
                                    row.col(|ui| {
                                        ui.label(value);
                                    });
                                });
                            }
                            return;
                        }
                        for frame in view.setpoint_frames.iter().rev().take(200) {
                            let bus_name =
                                database.map_or("Unknown", |db| db.bus_name(frame.bus_id));
                            if let Some(decoded) = &frame.decoded {
                                let key = decoded.key();
                                let def = database.and_then(|selection| {
                                    selection.database.message_by_key(key)
                                });
                                for (name, sig) in decoded.iter() {
                                    let signal_definition =
                                        def.and_then(|message| message.signal(name));

                                    let value = daqcore::formatter::try_format(
                                        formatter,
                                        decoded.name(),
                                        name,
                                        signal_definition,
                                        None,
                                        sig,
                                    );
                                    body.row(18.0, |mut row| {
                                        row.col(|ui| {
                                            ui.label(frame.timestamp.label());
                                        });
                                        row.col(|ui| {
                                            ui.label(bus_name);
                                        });
                                        row.col(|ui| {
                                            ui.label(format!(
                                                "{} (0x{:X})",
                                                decoded.name(),
                                                frame.identity
                                            ));
                                        });
                                        row.col(|ui| {
                                            ui.label(name);
                                        });
                                        row.col(|ui| {
                                            ui.label(value);
                                        });
                                    });
                                }
                            } else {
                                body.row(18.0, |mut row| {
                                    row.col(|ui| {
                                        ui.label(frame.timestamp.label());
                                    });
                                    row.col(|ui| {
                                        ui.label(bus_name);
                                    });
                                    row.col(|ui| {
                                        ui.label(format!("0x{:X}", frame.identity));
                                    });
                                    row.col(|ui| {
                                        ui.label("Unknown");
                                    });
                                    row.col(|ui| {
                                        ui.monospace(format!("{:02X?}", frame.raw_bytes));
                                    });
                                });
                            }
                        }
                    });
            });
        egui_tiles::UiResponse::None
    }
}
