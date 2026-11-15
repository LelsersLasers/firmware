use crate::{app, ui, workspace_view};

/// Metadata returned when the user selects a signal from the flat `msg.signal` picker.
#[derive(Clone, Debug)]
pub struct PickedSignal {
    pub signal_id: String,
    pub signal_name: String,
    pub unit: String,
    pub key: Option<daqcore::superdbc::message::MessageKey>,
    pub is_derived: bool,
}

impl PickedSignal {
    /// Formats the picker entry as `"msg.signal [unit]"` (or `"msg.signal"` when unitless).
    pub fn display_label(&self) -> String {
        if self.unit.is_empty() {
            self.signal_id.clone()
        } else {
            format!("{} [{}]", self.signal_id, self.unit)
        }
    }
}

pub enum SignalPickerAction {
    None,
    Cancelled,
    Picked(PickedSignal),
}

/// Shared flat `msg.signal` (and derived formula) picker used across `Scope`, `XYScatter`,
/// `StateLane`, and `GpsPlot`. Lists only signals and formulas active in the current timeline viewport.
pub fn show_signal_picker(
    ui: &mut eframe::egui::Ui,
    id_salt: impl std::hash::Hash,
    prompt: &str,
    allow_cancel: bool,
    prefer_discrete_when_empty: bool,
    search_query: &mut String,
    view: &workspace_view::WorkspaceDataView<'_>,
) -> SignalPickerAction {
    let mut cancelled = false;
    ui.horizontal(|ui| {
        if allow_cancel && ui.button("⬅ Cancel").clicked() {
            cancelled = true;
        }
        ui.label(prompt);
        ui.add(
            eframe::egui::TextEdit::singleline(search_query)
                .desired_width(ui.available_width().max(60.0))
                .hint_text("Search msg.signal or formula…"),
        );
    });
    if cancelled {
        return SignalPickerAction::Cancelled;
    }

    let query = search_query.trim().to_lowercase();
    let tokens: Vec<&str> = query.split_whitespace().filter(|tok| *tok != "*").collect();
    let matches_query = |text: &str| -> bool {
        if tokens.is_empty() {
            return true;
        }
        let lower = text.to_lowercase();
        tokens.iter().all(|tok| lower.contains(tok))
    };

    let active_signals = view.active_signals_in_viewport();
    let active_id_set: std::collections::HashSet<&str> = active_signals
        .iter()
        .map(|s| s.signal_id.as_str())
        .collect();

    let mut candidates: Vec<PickedSignal> = Vec::new();

    if !(prefer_discrete_when_empty && tokens.is_empty()) {
        for compiled in view.router.formula_engine().definitions() {
            let deps_active = active_id_set.is_empty()
                || view
                    .router
                    .formula_engine()
                    .execution_plan_for_expr(&compiled.ast)
                    .is_ok_and(|(_, leaves)| {
                        leaves.is_empty()
                            || leaves.iter().all(|leaf| active_id_set.contains(leaf.as_str()))
                    });
            if !deps_active {
                continue;
            }
            let def = &compiled.def;
            let searchable = format!("{} {} {}", def.id, def.label, def.unit);
            if matches_query(&searchable) {
                let signal_name = if def.label.is_empty() {
                    def.id.clone()
                } else {
                    def.label.clone()
                };
                candidates.push(PickedSignal {
                    signal_id: def.id.clone(),
                    signal_name,
                    unit: def.unit.clone(),
                    key: None,
                    is_derived: true,
                });
            }
        }
    }

    for meta in &active_signals {
        let is_discrete =
            meta.is_discrete || ui::state_lane::is_likely_fault_signal(&meta.signal_id);
        if prefer_discrete_when_empty && tokens.is_empty() && !is_discrete {
            continue;
        }
        let searchable = format!(
            "{} {} {} {}",
            meta.signal_id, meta.unit, meta.bus_name, meta.transmitter
        );
        if matches_query(&searchable) {
            let (_, sig_name) = meta.split_message_signal();
            let key = (meta.can_id != 0 || !meta.bus_name.is_empty()).then_some(
                daqcore::superdbc::message::MessageKey {
                    bus_id: meta.bus_id,
                    identity: meta.can_id,
                },
            );
            candidates.push(PickedSignal {
                signal_id: meta.signal_id.clone(),
                signal_name: sig_name.to_owned(),
                unit: meta.unit.clone(),
                key,
                is_derived: false,
            });
        }
    }

    if prefer_discrete_when_empty {
        candidates.sort_by(|a, b| a.signal_id.cmp(&b.signal_id));
    }

    let mut picked: Option<PickedSignal> = None;
    eframe::egui::ScrollArea::vertical()
        .id_salt(id_salt)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if candidates.is_empty() {
                let msg = if active_signals.is_empty() {
                    "No signals have samples in the active timeline range."
                } else {
                    "No matching signals found in the active timeline range."
                };
                ui.label(eframe::egui::RichText::new(msg).italics().weak());
                return;
            }
            for cand in candidates.into_iter().take(200) {
                let button_label = cand.display_label();
                let hover_text = if cand.is_derived && cand.signal_name != cand.signal_id {
                    format!("{} — {}", button_label, cand.signal_name)
                } else {
                    button_label.clone()
                };
                if ui
                    .add(eframe::egui::Button::new(button_label).wrap())
                    .on_hover_text(hover_text)
                    .clicked()
                {
                    picked = Some(cand);
                    break;
                }
            }
        });

    match picked {
        Some(sig) => SignalPickerAction::Picked(sig),
        None => SignalPickerAction::None,
    }
}

/// Metadata for a CAN message that has active signal samples in the current timeline viewport.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveMessageOption {
    pub message_name: String,
    pub bus_name: String,
    pub bus_id: u8,
    pub can_id: u32,
    pub nominal_period_ms: Option<u32>,
    pub first_signal_id: String,
}

impl ActiveMessageOption {
    pub fn key(&self) -> daqcore::superdbc::message::MessageKey {
        daqcore::superdbc::message::MessageKey {
            bus_id: self.bus_id,
            identity: self.can_id,
        }
    }
}

/// Range-aware CAN message picker backed by active signals in the current timeline viewport (used by `Jitter`).
#[derive(Default)]
pub struct ActiveMessagePickerState {
    search_text: String,
}

impl ActiveMessagePickerState {
    pub fn collect_active_messages(
        view: &workspace_view::WorkspaceDataView<'_>,
    ) -> Vec<ActiveMessageOption> {
        let mut by_msg: std::collections::BTreeMap<String, ActiveMessageOption> =
            std::collections::BTreeMap::new();
        for meta in view.active_signals_in_viewport() {
            let (msg_name, _) = meta.split_message_signal();
            if msg_name.is_empty() {
                continue;
            }
            by_msg
                .entry(msg_name.to_owned())
                .or_insert_with(|| ActiveMessageOption {
                    message_name: msg_name.to_owned(),
                    bus_name: if meta.bus_name.is_empty() {
                        "CAN".to_owned()
                    } else {
                        meta.bus_name.clone()
                    },
                    bus_id: meta.bus_id,
                    can_id: meta.can_id,
                    nominal_period_ms: meta.nominal_period_ms,
                    first_signal_id: meta.signal_id.clone(),
                });
        }
        by_msg.into_values().collect()
    }

    pub fn show(
        &mut self,
        ui: &mut eframe::egui::Ui,
        view: &workspace_view::WorkspaceDataView<'_>,
        selected_msg_is_none: bool,
    ) -> Option<ActiveMessageOption> {
        ui.horizontal(|ui| {
            ui.label("Search:");
            ui.add(
                eframe::egui::TextEdit::singleline(&mut self.search_text)
                    .desired_width(ui.available_width().max(60.0))
                    .hint_text("Message name, CAN ID, or bus…"),
            );
        });
        ui.add_space(4.0);
        if self.search_text.is_empty() && !selected_msg_is_none {
            return None;
        }
        let search = self.search_text.trim().to_lowercase();
        let messages = Self::collect_active_messages(view);
        let mut found = false;
        let mut picked = None;
        let scroll_max_h = if selected_msg_is_none {
            ui.available_height().max(80.0)
        } else {
            ui.available_height().clamp(80.0, 220.0)
        };
        eframe::egui::ScrollArea::vertical()
            .id_salt("active_message_picker_results")
            .max_height(scroll_max_h)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for msg in messages.into_iter().filter(|m| {
                    search.is_empty()
                        || search == "*"
                        || m.message_name.to_lowercase().contains(&search)
                        || format!("0x{:03x}", m.can_id).contains(&search)
                        || m.bus_name.to_lowercase().contains(&search)
                }) {
                    found = true;
                    let label = format!(
                        "[{}] {} (0x{:03X})",
                        msg.bus_name, msg.message_name, msg.can_id
                    );
                    if ui.add(eframe::egui::Button::new(label).wrap()).clicked() {
                        picked = Some(msg);
                    }
                }
                if !found {
                    ui.label(
                        eframe::egui::RichText::new(
                            "No messages have samples in the active timeline range.",
                        )
                        .italics()
                        .weak(),
                    );
                }
            });
        if picked.is_some() {
            self.search_text.clear();
        }
        picked
    }
}

/// Shared search state for widgets that operate on whole CAN messages (`SendUi`, `Jitter`).
#[derive(Default)]
pub struct MessagePickerState {
    search_text: String,
}

impl MessagePickerState {
    pub fn show(
        &mut self,
        ui: &mut eframe::egui::Ui,
        database: &app::DatabaseSelection,
        selected_msg_is_none: bool,
    ) -> Option<daqcore::superdbc::message::MessageKey> {
        ui.horizontal(|ui| {
            ui.label("Search:");
            ui.add(
                eframe::egui::TextEdit::singleline(&mut self.search_text)
                    .desired_width(ui.available_width().max(60.0))
                    .hint_text("Message name, signal, CAN ID, or bus…"),
            );
        });
        ui.add_space(4.0);
        if self.search_text.is_empty() && !selected_msg_is_none {
            return None;
        }
        let search = self.search_text.trim().to_lowercase();
        let mut found = false;
        let mut picked = None;
        let active_buses = database.active_buses();
        let scroll_max_h = if selected_msg_is_none {
            ui.available_height().max(80.0)
        } else {
            ui.available_height().clamp(80.0, 220.0)
        };
        eframe::egui::ScrollArea::vertical()
            .id_salt("message_picker_results")
            .max_height(scroll_max_h)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for bus in active_buses {
                    let bus_name_lower = bus.name().to_lowercase();
                    for message in bus.messages().iter().filter(|message| {
                        search.is_empty()
                            || search == "*"
                            || message.name().to_lowercase().contains(&search)
                            || format!("0x{:03x}", message.identity()).contains(&search)
                            || bus_name_lower.contains(&search)
                            || message.signals().iter().any(|s| {
                                daqcore::storage::format_signal_id(message.name(), s.name())
                                    .to_lowercase()
                                    .contains(&search)
                            })
                    }) {
                        found = true;
                        let label = format!(
                            "[{}] {} (0x{:03X})",
                            bus.name(),
                            message.name(),
                            message.identity()
                        );
                        if ui.add(eframe::egui::Button::new(label).wrap()).clicked() {
                            picked = Some(message.key());
                        }
                    }
                }
                if !found {
                    ui.label(
                        eframe::egui::RichText::new("No messages found.")
                            .italics()
                            .weak(),
                    );
                }
            });
        if picked.is_some() {
            self.search_text.clear();
        }
        picked
    }
}

pub fn no_database_placeholder(ui: &mut eframe::egui::Ui) {
    ui.vertical_centered(|ui| {
        ui.label("No active SuperDBC selection.");
        ui.label("CMD+S to toggle the sidebar.");
        ui.label("Use the sidebar to select a SuperDBC file and CAN bus.");
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picked_signal_display_label_formats_msg_signal_and_unit() {
        let with_unit = PickedSignal {
            signal_id: "pedals.throttle".to_owned(),
            signal_name: "throttle".to_owned(),
            unit: "%".to_owned(),
            key: None,
            is_derived: false,
        };
        assert_eq!(with_unit.display_label(), "pedals.throttle [%]");

        let unitless = PickedSignal {
            signal_id: "VCU_state.state".to_owned(),
            signal_name: "state".to_owned(),
            unit: String::new(),
            key: None,
            is_derived: false,
        };
        assert_eq!(unitless.display_label(), "VCU_state.state");
    }
}
