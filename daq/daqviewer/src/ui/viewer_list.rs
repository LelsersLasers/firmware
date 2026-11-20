use crate::workspace_view;

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
        view: &workspace_view::WorkspaceDataView<'_>,
    ) -> egui_tiles::UiResponse {
        let live_frames = view.live_frames_to_setpoint();
        let active_signals = view.active_signals_to_setpoint();
        ui.heading(&self.title);
        if live_frames.is_empty() && active_signals.is_empty() {
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
                        if live_frames.is_empty() {
                            let mut rows = Vec::new();
                            for sig_meta in &active_signals {
                                let (msg_name, sig_name) = sig_meta.split_message_signal();
                                if let Some((utc_ms, val)) =
                                    view.latest_signal_value_in_setpoint_range(&sig_meta.signal_id)
                                {
                                    let bus_name = if sig_meta.bus_name.is_empty() {
                                        "CAN"
                                    } else {
                                        sig_meta.bus_name.as_str()
                                    };
                                    let formatted = if sig_meta.unit.is_empty() {
                                        format!("{val:.3}")
                                    } else {
                                        format!("{val:.3} {}", sig_meta.unit)
                                    };
                                    rows.push((
                                        utc_ms,
                                        bus_name,
                                        msg_name,
                                        sig_meta.can_id,
                                        sig_name,
                                        formatted,
                                    ));
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
                                        ui.label(daqcore::storage::format_signal_id(
                                            msg_name, sig_name,
                                        ));
                                    });
                                    row.col(|ui| {
                                        ui.label(value);
                                    });
                                });
                            }
                            return;
                        }
                        for frame in live_frames.iter().rev().take(200) {
                            let bus_name = frame
                                .decoded
                                .as_ref()
                                .map(|d| d.bus_name())
                                .filter(|b| !b.is_empty())
                                .unwrap_or("CAN");
                            if let Some(decoded) = &frame.decoded {
                                for (name, sig) in decoded.iter() {
                                    let unit_opt = (!sig.unit().is_empty()).then_some(sig.unit());
                                    let value = daqcore::formatter::try_format(
                                        formatter,
                                        decoded.name(),
                                        name,
                                        None,
                                        unit_opt,
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
                                            ui.label(daqcore::storage::format_signal_id(
                                                decoded.name(),
                                                name,
                                            ));
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
