use crate::{action, ui, widget_constructor, workspace_view};
use std::collections::BTreeMap;

const STALE_BACKGROUND_ALPHA: u8 = 25;
const STALE_BORDER_WIDTH: f32 = 1.0;
const STALE_BORDER_ALPHA_FACTOR: f32 = 0.5;
const UNMATCHED_STALE_GAMMA: f32 = 0.6;

pub struct ViewerTable {
    pub title: String,
    search: String,
    tx_node: String,
}

impl ViewerTable {
    pub fn new(instance: usize) -> Self {
        Self {
            title: format!("CAN Viewer Table #{instance}"),
            search: String::new(),
            tx_node: "Any".into(),
        }
    }

    pub fn show(
        &mut self,
        ui: &mut eframe::egui::Ui,
        actions: &mut Vec<action::AppAction>,
        formatter: &Option<daqcore::formatter::Formatter>,
        view: &workspace_view::WorkspaceDataView<'_>,
    ) -> egui_tiles::UiResponse {
        let live_frames = view.live_frames_to_setpoint();
        let latest_live_frames = view.latest_live_frames_to_setpoint();
        let active_signals = view.active_signals_to_setpoint();
        ui.horizontal_wrapped(|ui| {
            let search_width = (ui.available_width() - 120.0).clamp(80.0, 260.0);
            ui.add(
                eframe::egui::TextEdit::singleline(&mut self.search)
                    .desired_width(search_width)
                    .hint_text("🔍 Search message, signal, or hex ID…"),
            );
            let mut nodes: Vec<_> = latest_live_frames
                .values()
                .filter_map(|f| f.decoded.as_ref().map(|d| d.tx_node().to_owned()))
                .chain(
                    active_signals
                        .iter()
                        .filter(|s| !s.transmitter.is_empty())
                        .map(|s| s.transmitter.clone()),
                )
                .collect();
            nodes.sort();
            nodes.dedup();
            eframe::egui::ComboBox::from_id_salt(("tx_node", &self.title))
                .selected_text(&self.tx_node)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.tx_node, "Any".into(), "Any");
                    ui.selectable_value(&mut self.tx_node, "Unparsed".into(), "Unparsed");
                    for node in nodes {
                        ui.selectable_value(&mut self.tx_node, node.clone(), node);
                    }
                });
        });
        if live_frames.is_empty() && active_signals.is_empty() {
            ui.label("No retained CAN messages in the selected interval.");
        }

        let search = self.search.to_lowercase();
        eframe::egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if latest_live_frames.is_empty() {
                    let mut grouped: BTreeMap<String, Vec<&daqcore::router::ActiveSignalMeta>> =
                        BTreeMap::new();
                    for sig in &active_signals {
                        let (msg_name, _) = sig.split_message_signal();
                        if !msg_name.is_empty() {
                            grouped.entry(msg_name.to_owned()).or_default().push(sig);
                        }
                    }

                    for (msg_name, msg_sigs) in grouped {
                        let first = msg_sigs[0];
                        let key = daqcore::superdbc::message::MessageKey {
                            bus_id: first.bus_id,
                            identity: first.can_id,
                        };
                        let node = if first.transmitter.is_empty() {
                            "Unknown"
                        } else {
                            first.transmitter.as_str()
                        };
                        let bus_name = if first.bus_name.is_empty() {
                            "CAN"
                        } else {
                            first.bus_name.as_str()
                        };
                        if self.tx_node != "Any" && self.tx_node != node {
                            continue;
                        }
                        if !search.is_empty()
                            && !msg_name.to_lowercase().contains(&search)
                            && !node.to_lowercase().contains(&search)
                            && !bus_name.to_lowercase().contains(&search)
                            && !format!("{:03X}", key.identity)
                                .to_lowercase()
                                .contains(&search)
                            && !msg_sigs.iter().any(|s| {
                                let (_, sig_name) = s.split_message_signal();
                                sig_name.to_lowercase().contains(&search)
                            })
                        {
                            continue;
                        }

                        let mut latest_msg_ms: Option<i64> = None;
                        let mut signals = Vec::with_capacity(msg_sigs.len());
                        for sig_meta in &msg_sigs {
                            let (_, sig_name) = sig_meta.split_message_signal();
                            let formatted = match view
                                .latest_signal_value_in_setpoint_range(&sig_meta.signal_id)
                            {
                                Some((utc_ms, val)) => {
                                    latest_msg_ms =
                                        Some(latest_msg_ms.map_or(utc_ms, |m| m.max(utc_ms)));
                                    let enum_label = if sig_meta.is_discrete {
                                        view.query_states(&sig_meta.signal_id, utc_ms..=utc_ms)
                                            .into_iter()
                                            .last()
                                            .map(|iv| iv.label)
                                    } else {
                                        None
                                    };
                                    format_active_signal_value(
                                        &sig_meta.unit,
                                        enum_label.as_deref(),
                                        val,
                                    )
                                }
                                None => continue,
                            };
                            signals.push((sig_name, formatted));
                        }

                        let Some(msg_ms) = latest_msg_ms else {
                            continue;
                        };
                        let timestamp_label = daqcore::time::Time::from_unix_millis(msg_ms).label();
                        // Resident 1Hz overview buckets are quantized to 1,000 ms boundaries,
                        // so include a 1,000 ms bucket window before marking an SD message stale.
                        let stale = first.nominal_period_ms.is_some_and(|period_ms| {
                            let timeout_ms = (i64::from(period_ms) * 5) / 2;
                            let elapsed_ms = view.view_time().unix_millis().saturating_sub(msg_ms);
                            elapsed_ms > timeout_ms.saturating_add(1_000)
                        });

                        actions.extend(
                            MessageCard {
                                msg_name: &msg_name,
                                bus_name,
                                key,
                                tx_node: node,
                                raw_bytes: "",
                                timestamp: &timestamp_label,
                                stale,
                                signals,
                                search: &self.search,
                            }
                            .ui(ui),
                        );
                        ui.add_space(8.0);
                    }
                    return;
                }

                for frame in latest_live_frames.values() {
                    let decoded = frame.decoded.as_ref();
                    let name = decoded.map_or("Error: Unknown", |d| d.name());
                    let node = decoded.map_or("Unparsed", |d| d.tx_node());
                    let bus_name = decoded
                        .map(|d| d.bus_name())
                        .filter(|b| !b.is_empty())
                        .unwrap_or("CAN");
                    if self.tx_node != "Any" && self.tx_node != node {
                        continue;
                    }

                    let stale =
                        decoded
                            .and_then(|d| d.nominal_period_ms())
                            .is_some_and(|period_ms| {
                                let timeout_ms = (i64::from(period_ms) * 5) / 2;
                                let elapsed_ms = view
                                    .view_time()
                                    .unix_millis()
                                    .saturating_sub(frame.timestamp.unix_millis());
                                elapsed_ms > timeout_ms
                            });

                    if !search.is_empty()
                        && !name.to_lowercase().contains(&search)
                        && !node.to_lowercase().contains(&search)
                        && !bus_name.to_lowercase().contains(&search)
                        && !(stale && "stale".contains(&search))
                        && !format!("{:03X}", frame.identity)
                            .to_lowercase()
                            .contains(&search)
                        && !decoded.is_some_and(|d| {
                            d.iter()
                                .any(|(name, _)| name.to_lowercase().contains(&search))
                        })
                    {
                        continue;
                    }

                    let signals = decoded
                        .map(|d| {
                            d.iter()
                                .map(|(name, sig)| {
                                    let unit_opt = (!sig.unit().is_empty()).then_some(sig.unit());
                                    (
                                        name,
                                        daqcore::formatter::try_format(
                                            formatter,
                                            d.name(),
                                            name,
                                            None,
                                            unit_opt,
                                            sig,
                                        ),
                                    )
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let bytes = frame
                        .raw_bytes
                        .iter()
                        .map(|b| format!("{b:02X}"))
                        .collect::<Vec<_>>()
                        .join(" ");
                    actions.extend(
                        MessageCard {
                            msg_name: name,
                            bus_name,
                            key: frame.key(),
                            tx_node: node,
                            raw_bytes: &bytes,
                            timestamp: &frame.timestamp.label(),
                            stale,
                            signals,
                            search: &self.search,
                        }
                        .ui(ui),
                    );
                    ui.add_space(8.0);
                }
            });
        egui_tiles::UiResponse::None
    }
}

struct MessageCard<'a> {
    msg_name: &'a str,
    bus_name: &'a str,
    key: daqcore::superdbc::message::MessageKey,
    tx_node: &'a str,
    raw_bytes: &'a str,
    timestamp: &'a str,
    stale: bool,
    signals: Vec<(&'a str, String)>,
    search: &'a str,
}

impl MessageCard<'_> {
    fn ui(&self, ui: &mut eframe::egui::Ui) -> Vec<action::AppAction> {
        let mut action_queue = Vec::new();
        let theme = ui::theme::get_theme(ui.ctx());
        let error_color = theme.error_color();

        // Header (outside card)
        ui.horizontal_wrapped(|ui| {
            let search = self.search.to_lowercase();
            let matches_name = search.is_empty()
                || self.msg_name.to_lowercase().contains(&search)
                || self.bus_name.to_lowercase().contains(&search);
            let name_color = if self.stale {
                if matches_name {
                    error_color
                } else {
                    error_color.gamma_multiply(UNMATCHED_STALE_GAMMA)
                }
            } else if matches_name {
                ui.visuals().text_color()
            } else {
                ui.visuals().weak_text_color()
            };

            ui.label(
                eframe::egui::RichText::new(format!(
                    "{}: {}  [0x{:X} | 0x{:03X}]",
                    self.bus_name, self.msg_name, self.key.bus_id, self.key.identity
                ))
                .strong()
                .size(16.0)
                .color(name_color),
            );

            let matches_node = search.is_empty() || self.tx_node.to_lowercase().contains(&search);
            let node_color = if matches_node {
                ui.visuals().text_color()
            } else {
                ui.visuals().weak_text_color()
            };

            ui.label(
                eframe::egui::RichText::new(format!("from {}", self.tx_node)).color(node_color),
            );
            ui.label(
                eframe::egui::RichText::new(self.timestamp)
                    .italics()
                    .color(ui.visuals().weak_text_color()),
            );
            if self.stale {
                ui.label(
                    eframe::egui::RichText::new("STALE")
                        .strong()
                        .color(error_color),
                );
            }
            if !self.raw_bytes.is_empty() {
                ui.label(
                    eframe::egui::RichText::new(self.raw_bytes)
                        .monospace()
                        .color(ui.visuals().text_color()),
                );
            }
        });

        ui.add_space(4.0);

        // Card container
        if self.signals.is_empty() {
            return action_queue;
        }

        let (card_fill, card_stroke) = if self.stale {
            (
                eframe::egui::Color32::from_rgba_unmultiplied(
                    error_color.r(),
                    error_color.g(),
                    error_color.b(),
                    STALE_BACKGROUND_ALPHA,
                ),
                eframe::egui::Stroke::new(
                    STALE_BORDER_WIDTH,
                    error_color.linear_multiply(STALE_BORDER_ALPHA_FACTOR),
                ),
            )
        } else {
            (
                ui.visuals().faint_bg_color,
                ui.visuals().widgets.noninteractive.bg_stroke,
            )
        };

        eframe::egui::Frame::group(ui.style())
            .fill(card_fill)
            .stroke(card_stroke)
            .corner_radius(eframe::egui::CornerRadius::same(8))
            .inner_margin(eframe::egui::Margin::symmetric(8, 6))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.vertical(|ui| {
                    for (i, (sig_name, value)) in self.signals.iter().enumerate() {
                        let search = self.search.to_lowercase();
                        let matches_signal =
                            search.is_empty() || sig_name.to_lowercase().contains(&search);
                        let signal_color = if matches_signal {
                            ui.visuals().text_color()
                        } else {
                            ui.visuals().weak_text_color()
                        };
                        let full_sig_name = if self.msg_name == "Error: Unknown" {
                            (*sig_name).to_owned()
                        } else {
                            daqcore::storage::format_signal_id(self.msg_name, sig_name)
                        };

                        let sig_galley = eframe::egui::WidgetText::from(
                            eframe::egui::RichText::new(&full_sig_name)
                                .monospace()
                                .color(signal_color),
                        )
                        .into_galley(
                            ui,
                            Some(eframe::egui::TextWrapMode::Extend),
                            f32::INFINITY,
                            eframe::egui::TextStyle::Monospace,
                        );
                        let val_galley = eframe::egui::WidgetText::from(
                            eframe::egui::RichText::new(value).monospace(),
                        )
                        .into_galley(
                            ui,
                            Some(eframe::egui::TextWrapMode::Extend),
                            f32::INFINITY,
                            eframe::egui::TextStyle::Monospace,
                        );
                        let fits_single_line = sig_galley.size().x + val_galley.size().x + 48.0
                            <= ui.available_width();

                        let mut open_scope = false;
                        if fits_single_line {
                            ui.horizontal(|ui| {
                                if ui
                                    .small_button("📊")
                                    .on_hover_text("Open in Scope")
                                    .clicked()
                                {
                                    open_scope = true;
                                }
                                ui.label(
                                    eframe::egui::RichText::new(&full_sig_name)
                                        .monospace()
                                        .color(signal_color),
                                );
                                ui.with_layout(
                                    eframe::egui::Layout::right_to_left(
                                        eframe::egui::Align::Center,
                                    ),
                                    |ui| {
                                        ui.label(eframe::egui::RichText::new(value).monospace());
                                    },
                                );
                            });
                        } else {
                            ui.horizontal_wrapped(|ui| {
                                if ui
                                    .small_button("📊")
                                    .on_hover_text("Open in Scope")
                                    .clicked()
                                {
                                    open_scope = true;
                                }
                                ui.label(
                                    eframe::egui::RichText::new(&full_sig_name)
                                        .monospace()
                                        .color(signal_color),
                                );
                                ui.label(
                                    eframe::egui::RichText::new("=")
                                        .monospace()
                                        .color(ui.visuals().weak_text_color()),
                                );
                                ui.label(eframe::egui::RichText::new(value).monospace());
                            });
                        }

                        if open_scope {
                            action_queue.push(action::AppAction::SpawnWidget(
                                widget_constructor::WidgetConstructor::Scope {
                                    key: self.key,
                                    msg_name: self.msg_name.to_string(),
                                    signal_name: sig_name.to_string(),
                                },
                            ));
                        }
                        if i < self.signals.len() - 1 {
                            ui.separator();
                        }
                    }
                });
            });

        action_queue
    }
}

fn format_active_signal_value(unit: &str, enum_label: Option<&str>, val: f64) -> String {
    if let Some(label) = enum_label
        && !label.is_empty()
    {
        return format!("{label} ({val:.0})");
    }
    let formatted_num = if (val - val.round()).abs() < 1e-6 {
        format!("{val:.0}")
    } else {
        format!("{val:.3}")
    };
    if unit.is_empty() {
        formatted_num
    } else {
        format!("{formatted_num} {unit}")
    }
}
