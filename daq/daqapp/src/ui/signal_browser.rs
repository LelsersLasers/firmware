use crate::{action, app, widget_constructor};
use std::collections::{BTreeMap, BTreeSet};

/// Drag-and-drop payload emitted when a user drags a signal or derived formula from
/// the Signal Browser onto a `Scope`, `GpsPlot`, or the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DraggedSignal {
    Raw {
        key: Option<daqcore::superdbc::message::MessageKey>,
        msg_name: String,
        signal_name: String,
        unit: String,
    },
    Derived {
        signal_id: String,
        label: String,
        unit: String,
    },
}

impl DraggedSignal {
    pub fn signal_id(&self) -> String {
        match self {
            Self::Raw {
                msg_name,
                signal_name,
                ..
            } => daqcore::storage::format_signal_id(msg_name, signal_name),
            Self::Derived { signal_id, .. } => signal_id.clone(),
        }
    }

    #[cfg(test)]
    pub fn display_label(&self) -> String {
        match self {
            Self::Raw {
                msg_name,
                signal_name,
                ..
            } => format!("{msg_name}.{signal_name}"),
            Self::Derived { label, .. } => label.clone(),
        }
    }

    pub fn unit(&self) -> &str {
        match self {
            Self::Raw { unit, .. } | Self::Derived { unit, .. } => unit.as_str(),
        }
    }

    pub fn to_widget_constructor(&self) -> widget_constructor::WidgetConstructor {
        match self {
            Self::Raw {
                key: Some(key),
                msg_name,
                signal_name,
                ..
            } => widget_constructor::WidgetConstructor::Scope {
                key: *key,
                msg_name: msg_name.clone(),
                signal_name: signal_name.clone(),
            },
            Self::Raw {
                key: None,
                msg_name,
                signal_name,
                unit,
            } => widget_constructor::WidgetConstructor::ScopeChannel {
                signal_id: daqcore::storage::format_signal_id(msg_name, signal_name),
                label: format!("{msg_name}.{signal_name}"),
                unit: unit.clone(),
            },
            Self::Derived {
                signal_id,
                label,
                unit,
            } => widget_constructor::WidgetConstructor::ScopeChannel {
                signal_id: signal_id.clone(),
                label: label.clone(),
                unit: unit.clone(),
            },
        }
    }
}

/// Draft state for the `"+ New Formula"` modal / inline editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormulaDraft {
    pub id: String,
    pub label: String,
    pub expression: String,
    pub unit: String,
    pub is_editing_existing: bool,
}

impl Default for FormulaDraft {
    fn default() -> Self {
        Self {
            id: "custom_power_kw".to_owned(),
            label: "Custom Power".to_owned(),
            expression: "(pack_stats.vbatt_adc * pack_stats.ibatt_adc) / 1000.0".to_owned(),
            unit: "kW".to_owned(),
            is_editing_existing: false,
        }
    }
}

impl FormulaDraft {
    pub fn from_compiled(compiled: &daqcore::analysis::CompiledDerivedSignal) -> Self {
        Self {
            id: compiled.def.id.clone(),
            label: compiled.def.label.clone(),
            expression: compiled.def.expression.clone(),
            unit: compiled.def.unit.clone(),
            is_editing_existing: true,
        }
    }

    pub fn to_def(&self) -> daqcore::analysis::DerivedSignalDef {
        let id = self.id.trim().to_owned();
        let label = if self.label.trim().is_empty() {
            id.clone()
        } else {
            self.label.trim().to_owned()
        };
        daqcore::analysis::DerivedSignalDef {
            id,
            label,
            expression: self.expression.trim().to_owned(),
            unit: self.unit.trim().to_owned(),
        }
    }

    /// Validate syntax and topological dependencies against `engine` without mutating it.
    /// Returns `Ok(referenced_signal_ids)` if valid, or `Err(error_message)`.
    pub fn validate(
        &self,
        engine: &daqcore::analysis::FormulaEngine,
    ) -> Result<Vec<String>, String> {
        let id = self.id.trim();
        if id.is_empty() {
            return Err("Formula ID must not be empty".to_owned());
        }
        if id.contains(char::is_whitespace) {
            return Err("Formula ID must not contain spaces".to_owned());
        }
        let ast = daqcore::analysis::Expr::parse(&self.expression).map_err(|e| e.to_string())?;
        let refs = ast.referenced_signals();
        let mut preview_engine = engine.clone();
        preview_engine
            .upsert_formula(self.to_def())
            .map_err(|e| e.to_string())?;
        Ok(refs)
    }
}

/// Persistent UI state for the Unified Signal & Formula Browser.
#[derive(Debug, Default, Clone)]
pub struct SignalBrowserState {
    pub search_query: String,
    pub bus_filter: Option<String>,
    pub node_filter: Option<String>,
    pub formula_draft: Option<FormulaDraft>,
}

/// Test whether a candidate text set matches a user search query (supporting glob wildcards
/// `*`, `?`, `[` via `globset` as well as multi-token case-insensitive substring matching).
pub fn matches_search_query(query: &str, haystacks: &[&str]) -> bool {
    let q = query.trim();
    if q.is_empty() || q == "*" {
        return true;
    }

    if (q.contains('*') || q.contains('?') || q.contains('['))
        && let Ok(glob) = globset::GlobBuilder::new(&format!("*{q}*"))
            .case_insensitive(true)
            .build()
    {
        let matcher = glob.compile_matcher();
        return haystacks.iter().any(|h| matcher.is_match(h));
    }

    let tokens: Vec<String> = q
        .split_whitespace()
        .map(|t| t.to_ascii_lowercase())
        .collect();
    let combined = haystacks.join(" ").to_ascii_lowercase();
    tokens.iter().all(|tok| combined.contains(tok))
}

#[derive(Clone)]
struct SignalLeaf {
    key: Option<daqcore::superdbc::message::MessageKey>,
    msg_name: String,
    signal_name: String,
    unit: String,
    description: String,
}

#[derive(Clone)]
struct MessageGroup {
    key: Option<daqcore::superdbc::message::MessageKey>,
    bus_name: String,
    transmitter: String,
    msg_name: String,
    description: String,
    signals: Vec<SignalLeaf>,
}

pub fn show_signal_browser(
    ui: &mut eframe::egui::Ui,
    state: &mut SignalBrowserState,
    database: Option<&app::DatabaseSelection>,
    router: &mut daqcore::router::VirtualTimelineRouter,
    action_queue: &mut Vec<action::AppAction>,
    diagnostic: &mut Option<String>,
) {
    ui.set_max_width(ui.available_width());

    ui.horizontal_wrapped(|ui| {
        ui.strong("🔎 Signal & Formula Browser");
        if ui
            .small_button("➕ New Formula")
            .on_hover_text("Define a new vectorized derived formula channel")
            .clicked()
        {
            state.formula_draft = Some(FormulaDraft::default());
        }
    });

    // Search bar (right-to-left so the clear button deducts from available_width first
    // and never causes a frame-over-frame SidePanel width feedback loop; explicit id_salt
    // ensures focus is never dropped when search_query transitions from empty to non-empty)
    let row_width = ui.available_width();
    ui.allocate_ui_with_layout(
        eframe::egui::vec2(row_width, ui.spacing().interact_size.y),
        eframe::egui::Layout::right_to_left(eframe::egui::Align::Center),
        |ui| {
            ui.set_max_width(row_width);
            if ui
                .add_enabled(
                    !state.search_query.is_empty(),
                    eframe::egui::Button::new("✕").small(),
                )
                .on_hover_text("Clear search filter")
                .clicked()
            {
                state.search_query.clear();
            }
            let edit_width = (ui.available_width() - 14.0).max(40.0);
            ui.add(
                eframe::egui::TextEdit::singleline(&mut state.search_query)
                    .id(eframe::egui::Id::new("signal_browser_search_input"))
                    .desired_width(edit_width)
                    .hint_text("Search signal, message, unit, or glob…"),
            );
        },
    );

    // Build message groups from SuperDBC + fallback signals from partition manifests / live buffer
    let mut bus_names: BTreeSet<String> = BTreeSet::new();
    let mut node_names: BTreeSet<String> = BTreeSet::new();
    let mut groups_by_msg: BTreeMap<String, MessageGroup> = BTreeMap::new();

    if let Some(db_sel) = database {
        for bus in db_sel.database.buses() {
            bus_names.insert(bus.name().to_owned());
            for msg in bus.messages() {
                node_names.insert(msg.transmitter().to_owned());
                let signals = msg
                    .signals()
                    .iter()
                    .map(|sig| SignalLeaf {
                        key: Some(msg.key()),
                        msg_name: msg.name().to_owned(),
                        signal_name: sig.name().to_owned(),
                        unit: sig.unit().to_owned(),
                        description: sig.description().to_owned(),
                    })
                    .collect();
                groups_by_msg.insert(
                    msg.name().to_owned(),
                    MessageGroup {
                        key: Some(msg.key()),
                        bus_name: bus.name().to_owned(),
                        transmitter: msg.transmitter().to_owned(),
                        msg_name: msg.name().to_owned(),
                        description: msg.description().to_owned(),
                        signals,
                    },
                );
            }
        }
    }

    // Merge any signals from loaded Parquet partition manifests or live buffer not in SuperDBC
    for part in router.partitions() {
        for sig_entry in &part.manifest.signals {
            if let Some((msg_name, sig_name)) = sig_entry.signal_id.split_once('.') {
                let group =
                    groups_by_msg
                        .entry(msg_name.to_owned())
                        .or_insert_with(|| MessageGroup {
                            key: None,
                            bus_name: "Store".to_owned(),
                            transmitter: "Parquet".to_owned(),
                            msg_name: msg_name.to_owned(),
                            description: String::new(),
                            signals: Vec::new(),
                        });
                if !group.signals.iter().any(|s| s.signal_name == sig_name) {
                    group.signals.push(SignalLeaf {
                        key: group.key,
                        msg_name: msg_name.to_owned(),
                        signal_name: sig_name.to_owned(),
                        unit: sig_entry.unit.clone(),
                        description: String::new(),
                    });
                }
            }
        }
    }
    for sig_id in router.live_buffer().signal_ids() {
        if let Some((msg_name, sig_name)) = sig_id.split_once('.') {
            let group = groups_by_msg
                .entry(msg_name.to_owned())
                .or_insert_with(|| MessageGroup {
                    key: None,
                    bus_name: "Live".to_owned(),
                    transmitter: "Live".to_owned(),
                    msg_name: msg_name.to_owned(),
                    description: String::new(),
                    signals: Vec::new(),
                });
            if !group.signals.iter().any(|s| s.signal_name == sig_name) {
                group.signals.push(SignalLeaf {
                    key: group.key,
                    msg_name: msg_name.to_owned(),
                    signal_name: sig_name.to_owned(),
                    unit: String::new(),
                    description: String::new(),
                });
            }
        }
    }

    // Optional Bus & Node Filter tags
    if !bus_names.is_empty() || !node_names.is_empty() {
        ui.horizontal_wrapped(|ui| {
            let bus_label = state.bus_filter.as_deref().unwrap_or("All Buses");
            eframe::egui::ComboBox::from_id_salt("signal_browser_bus_filter")
                .width(96.0)
                .selected_text(format!("Bus: {bus_label}"))
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(state.bus_filter.is_none(), "All Buses")
                        .clicked()
                    {
                        state.bus_filter = None;
                    }
                    for b in &bus_names {
                        if ui
                            .selectable_label(state.bus_filter.as_deref() == Some(b.as_str()), b)
                            .clicked()
                        {
                            state.bus_filter = Some(b.clone());
                        }
                    }
                });

            let node_label = state.node_filter.as_deref().unwrap_or("All Nodes");
            eframe::egui::ComboBox::from_id_salt("signal_browser_node_filter")
                .width(110.0)
                .selected_text(format!("Node: {node_label}"))
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(state.node_filter.is_none(), "All Nodes")
                        .clicked()
                    {
                        state.node_filter = None;
                    }
                    for n in &node_names {
                        if ui
                            .selectable_label(state.node_filter.as_deref() == Some(n.as_str()), n)
                            .clicked()
                        {
                            state.node_filter = Some(n.clone());
                        }
                    }
                });
        });
    }

    ui.add(
        eframe::egui::Label::new(
            eframe::egui::RichText::new(
                "💡 Drag any signal/formula onto a Scope or GPS Plot, or click 📈",
            )
            .small()
            .weak(),
        )
        .wrap(),
    );

    ui.scope(|ui| {
        ui.set_max_width(ui.available_width());
        ui.style_mut().wrap_mode = Some(eframe::egui::TextWrapMode::Truncate);

        // -------------------------------------------------------------------------
        // 1. Top-level "Derived Signals (Formulas)" Folder
        // -------------------------------------------------------------------------
        let compiled_formulas: Vec<daqcore::analysis::CompiledDerivedSignal> =
            router.formula_engine().definitions().to_vec();
        let matching_formulas: Vec<&daqcore::analysis::CompiledDerivedSignal> = compiled_formulas
            .iter()
            .filter(|c| {
                matches_search_query(
                    &state.search_query,
                    &[
                        &c.def.id,
                        &c.def.label,
                        &c.def.unit,
                        &c.def.expression,
                        "derived formula",
                    ],
                )
            })
            .collect();

        eframe::egui::CollapsingHeader::new(
            eframe::egui::RichText::new(format!(
                "📐 Derived Signals (Formulas) ({})",
                matching_formulas.len()
            ))
            .strong(),
        )
        .id_salt("signal_browser_derived_folder")
        .default_open(true)
        .show(ui, |ui| {
            if matching_formulas.is_empty() {
                ui.label(eframe::egui::RichText::new("No matching formulas.").weak().small());
            }
            for compiled in matching_formulas {
                let def = &compiled.def;
                let unit_suffix = if def.unit.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", def.unit)
                };
                let drag_payload = DraggedSignal::Derived {
                    signal_id: def.id.clone(),
                    label: def.label.clone(),
                    unit: def.unit.clone(),
                };

                ui.horizontal(|ui| {
                    if ui
                        .small_button("📈")
                        .on_hover_text("Open formula in a new Scope")
                        .clicked()
                    {
                        action_queue.push(action::AppAction::SpawnWidget(
                            drag_payload.to_widget_constructor(),
                        ));
                    }
                    if ui
                        .small_button("✏")
                        .on_hover_text("Edit formula expression")
                        .clicked()
                    {
                        state.formula_draft = Some(FormulaDraft::from_compiled(compiled));
                    }
                    let dnd_resp = ui.dnd_drag_source(
                        eframe::egui::Id::new(("dnd_derived_signal", &def.id)),
                        drag_payload.clone(),
                        |ui| {
                            ui.add(
                                eframe::egui::Label::new(
                                    eframe::egui::RichText::new(format!(
                                        "ƒ {} — {}{}",
                                        def.id, def.label, unit_suffix
                                    ))
                                    .small(),
                                )
                                .truncate(),
                            );
                        },
                    );
                    let row_resp = dnd_resp.response.on_hover_text(format!(
                        "Formula: {}\nExpression: {}\nDependencies: {}\nDrag onto a Scope/GpsPlot or click 📈",
                        def.id,
                        def.expression,
                        if compiled.dependencies.is_empty() {
                            "(none)".to_owned()
                        } else {
                            compiled.dependencies.join(", ")
                        }
                    ));
                    if row_resp.double_clicked() {
                        action_queue.push(action::AppAction::SpawnWidget(
                            drag_payload.to_widget_constructor(),
                        ));
                    }
                });
            }
        });

        // -------------------------------------------------------------------------
        // 2. Hierarchical Tree: Message -> Signal (with Unit)
        // -------------------------------------------------------------------------
        let query_active = !state.search_query.trim().is_empty();
        let mut visible_msg_count = 0usize;
        let mut visible_sig_count = 0usize;

        let filtered_groups: Vec<(MessageGroup, Vec<SignalLeaf>)> = groups_by_msg
            .into_values()
            .filter(|g| {
                state.bus_filter.as_deref().is_none_or(|b| g.bus_name == b)
                    && state
                        .node_filter
                        .as_deref()
                        .is_none_or(|n| g.transmitter == n)
            })
            .filter_map(|g| {
                let msg_matches = matches_search_query(
                    &state.search_query,
                    &[&g.msg_name, &g.bus_name, &g.transmitter, &g.description],
                );
                let matching_sigs: Vec<SignalLeaf> = g
                    .signals
                    .iter()
                    .filter(|s| {
                        let canonical_id = format!("{}.{}", s.msg_name, s.signal_name);
                        msg_matches
                            || matches_search_query(
                                &state.search_query,
                                &[
                                    &canonical_id,
                                    &s.signal_name,
                                    &s.unit,
                                    &s.description,
                                    &g.bus_name,
                                    &g.transmitter,
                                ],
                            )
                    })
                    .cloned()
                    .collect();
                if matching_sigs.is_empty() {
                    None
                } else {
                    visible_msg_count += 1;
                    visible_sig_count += matching_sigs.len();
                    Some((g, matching_sigs))
                }
            })
            .collect();

        eframe::egui::CollapsingHeader::new(
            eframe::egui::RichText::new(format!(
                "📡 CAN Messages & Signals ({visible_msg_count} msgs, {visible_sig_count} sigs)"
            ))
            .strong(),
        )
        .id_salt("signal_browser_can_tree")
        .default_open(true)
        .show(ui, |ui| {
            if filtered_groups.is_empty() {
                ui.label(
                    eframe::egui::RichText::new("No matching CAN messages or signals.")
                        .weak()
                        .small(),
                );
            }
            for (group, sigs) in filtered_groups {
                let header_label = if let Some(k) = group.key {
                    format!(
                        "{} [{} · {} · 0x{:03X}] ({})",
                        group.msg_name,
                        group.bus_name,
                        group.transmitter,
                        k.identity,
                        sigs.len()
                    )
                } else {
                    format!("{} [{}] ({})", group.msg_name, group.bus_name, sigs.len())
                };

                let mut header = eframe::egui::CollapsingHeader::new(
                    eframe::egui::RichText::new(header_label).small().strong(),
                )
                .id_salt(("sig_browser_msg", &group.msg_name));
                if query_active && visible_msg_count <= 25 {
                    header = header.open(Some(true));
                }

                header.show(ui, |ui| {
                    for sig in sigs {
                        let canonical_id =
                            daqcore::storage::format_signal_id(&sig.msg_name, &sig.signal_name);
                        let unit_badge = if sig.unit.is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", sig.unit)
                        };
                        let drag_payload = DraggedSignal::Raw {
                            key: sig.key,
                            msg_name: sig.msg_name.clone(),
                            signal_name: sig.signal_name.clone(),
                            unit: sig.unit.clone(),
                        };

                        ui.horizontal(|ui| {
                            if ui
                                .small_button("📈")
                                .on_hover_text(format!("Open {canonical_id} in a new Scope"))
                                .clicked()
                            {
                                action_queue.push(action::AppAction::SpawnWidget(
                                    drag_payload.to_widget_constructor(),
                                ));
                            }
                            let dnd_resp = ui.dnd_drag_source(
                                eframe::egui::Id::new(("dnd_raw_signal", &canonical_id)),
                                drag_payload.clone(),
                                |ui| {
                                    ui.add(
                                        eframe::egui::Label::new(
                                            eframe::egui::RichText::new(format!(
                                                "• {}{}",
                                                sig.signal_name, unit_badge
                                            ))
                                            .small(),
                                        )
                                        .truncate(),
                                    );
                                },
                            );
                            let row_resp = dnd_resp.response.on_hover_text(format!(
                                "Signal ID: {}\nUnit: {}\n{}\nDrag onto a Scope/GpsPlot or click 📈",
                                canonical_id,
                                if sig.unit.is_empty() {
                                    "(unitless)"
                                } else {
                                    sig.unit.as_str()
                                },
                                if sig.description.is_empty() {
                                    "(no description)"
                                } else {
                                    sig.description.as_str()
                                }
                            ));
                            if row_resp.double_clicked() {
                                action_queue.push(action::AppAction::SpawnWidget(
                                    drag_payload.to_widget_constructor(),
                                ));
                            }
                        });
                    }
                });
            }
        });
    });

    // Render the "+ New Formula" modal if open
    show_formula_editor_modal(ui.ctx(), state, router, action_queue, diagnostic);
}

pub fn show_formula_editor_modal(
    ctx: &eframe::egui::Context,
    state: &mut SignalBrowserState,
    router: &mut daqcore::router::VirtualTimelineRouter,
    action_queue: &mut Vec<action::AppAction>,
    diagnostic: &mut Option<String>,
) {
    let Some(mut draft) = state.formula_draft.clone() else {
        return;
    };

    let mut keep_open = true;
    let mut cancel_clicked = false;
    let mut save_clicked = false;
    let mut save_and_plot_clicked = false;

    let validation = draft.validate(router.formula_engine());
    let title = if draft.is_editing_existing {
        "✏ Edit Derived Formula Channel"
    } else {
        "➕ New Derived Formula Channel"
    };

    eframe::egui::Window::new(title)
        .id(eframe::egui::Id::new("formula_editor_modal"))
        .collapsible(false)
        .resizable(true)
        .open(&mut keep_open)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Channel ID:");
                ui.text_edit_singleline(&mut draft.id);
            });
            ui.horizontal(|ui| {
                ui.label("Label:     ");
                ui.text_edit_singleline(&mut draft.label);
            });
            ui.horizontal(|ui| {
                ui.label("Unit:      ");
                ui.text_edit_singleline(&mut draft.unit);
            });

            ui.label("Expression (e.g. `(pack_stats.vbatt_adc * pack_stats.ibatt_adc) / 1000.0` or `lowpass(imu.accel_y_g, 15.0)`):");
            ui.add(
                eframe::egui::TextEdit::multiline(&mut draft.expression)
                    .desired_rows(3)
                    .code_editor(),
            );

            match &validation {
                Ok(deps) => {
                    ui.colored_label(
                        eframe::egui::Color32::from_rgb(34, 197, 94),
                        format!(
                            "✓ Valid expression · Referenced signals ({}): {}",
                            deps.len(),
                            if deps.is_empty() {
                                "(constant)".to_owned()
                            } else {
                                deps.join(", ")
                            }
                        ),
                    );
                }
                Err(err) => {
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        format!("⚠ Validation error: {err}"),
                    );
                }
            }

            ui.separator();
            ui.horizontal(|ui| {
                let is_valid = validation.is_ok();
                if ui
                    .add_enabled(is_valid, eframe::egui::Button::new("💾 Save Formula"))
                    .clicked()
                {
                    save_clicked = true;
                }
                if ui
                    .add_enabled(
                        is_valid,
                        eframe::egui::Button::new("📈 Save & Open in Scope"),
                    )
                    .clicked()
                {
                    save_and_plot_clicked = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel_clicked = true;
                }
            });
        });

    if save_clicked || save_and_plot_clicked {
        let def = draft.to_def();
        match router.formula_engine_mut().upsert_formula(def.clone()) {
            Ok(()) => {
                if save_and_plot_clicked {
                    action_queue.push(action::AppAction::SpawnWidget(
                        widget_constructor::WidgetConstructor::ScopeChannel {
                            signal_id: def.id,
                            label: def.label,
                            unit: def.unit,
                        },
                    ));
                }
                state.formula_draft = None;
            }
            Err(err) => {
                *diagnostic = Some(format!("Failed to save formula: {err}"));
                state.formula_draft = Some(draft);
            }
        }
    } else if !keep_open || cancel_clicked {
        state.formula_draft = None;
    } else {
        state.formula_draft = Some(draft);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_and_fuzzy_search_matches_signals_units_and_descriptions() {
        let haystacks = [
            "pack_stats.vbatt_adc",
            "vbatt_adc",
            "V",
            "Pack battery voltage",
            "VCAN",
            "A_BOX",
        ];
        assert!(matches_search_query("", &haystacks));
        assert!(matches_search_query("*", &haystacks));
        assert!(matches_search_query("*vbatt*", &haystacks));
        assert!(matches_search_query("pack_stats.*", &haystacks));
        assert!(matches_search_query("battery V", &haystacks));
        assert!(!matches_search_query("motor_rpm", &haystacks));
    }

    #[test]
    fn formula_draft_validates_syntax_dependencies_and_cycles() {
        let engine = daqcore::analysis::FormulaEngine::default();
        let draft = FormulaDraft {
            id: "mech_power_kw".to_owned(),
            label: "Mechanical Power".to_owned(),
            expression: "(motor_stats.torque * motor_stats.rpm) / 9549.0".to_owned(),
            unit: "kW".to_owned(),
            is_editing_existing: false,
        };
        let deps = draft.validate(&engine).expect("valid expression");
        assert_eq!(
            deps,
            vec![
                "motor_stats.torque".to_owned(),
                "motor_stats.rpm".to_owned()
            ]
        );

        let bad_syntax = FormulaDraft {
            id: "bad_expr".to_owned(),
            label: "Bad".to_owned(),
            expression: "pack_stats.vbatt_adc * (".to_owned(),
            unit: "V".to_owned(),
            is_editing_existing: false,
        };
        assert!(bad_syntax.validate(&engine).is_err());

        let self_cycle = FormulaDraft {
            id: "self_loop".to_owned(),
            label: "Cycle".to_owned(),
            expression: "self_loop + 1.0".to_owned(),
            unit: "".to_owned(),
            is_editing_existing: false,
        };
        assert!(self_cycle.validate(&engine).is_err());
    }

    #[test]
    fn signal_browser_and_drag_targets_render_and_configure_cleanly() {
        let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let dbc_path = manifest_dir.join("../../superdbc/superdbc_per26.json");
        let database = daqcore::superdbc::database::Database::load(&dbc_path).expect("load PER26");
        let selection = daqcore::can_thread::BusSelection::default_single(&database)
            .unwrap_or(daqcore::can_thread::BusSelection::Single(0));
        let db_sel = app::DatabaseSelection {
            database,
            selection,
        };

        let t0 = daqcore::time::Time::from_unix_millis(1_788_719_789_000);
        let mut router =
            daqcore::router::VirtualTimelineRouter::live(t0, 30.0).expect("live router");
        let mut state = SignalBrowserState {
            formula_draft: Some(FormulaDraft::default()),
            ..Default::default()
        };
        let mut action_queue = Vec::new();
        let mut diag = None;

        let ctx = eframe::egui::Context::default();
        crate::ui::theme::store_theme(
            &ctx,
            crate::ui::theme::ThemeSelection::default().get_colors(),
        );
        let raw_input = eframe::egui::RawInput {
            screen_rect: Some(eframe::egui::Rect::from_min_size(
                eframe::egui::Pos2::ZERO,
                eframe::egui::vec2(420.0, 720.0),
            )),
            ..Default::default()
        };
        let _ = ctx.run(raw_input, |ctx| {
            eframe::egui::CentralPanel::default().show(ctx, |ui| {
                show_signal_browser(
                    ui,
                    &mut state,
                    Some(&db_sel),
                    &mut router,
                    &mut action_queue,
                    &mut diag,
                );
            });
            show_formula_editor_modal(ctx, &mut state, &mut router, &mut action_queue, &mut diag);
        });

        let dragged = DraggedSignal::Derived {
            signal_id: "dc_bus_power_kw".to_owned(),
            label: "DC Bus Power".to_owned(),
            unit: "kW".to_owned(),
        };
        assert_eq!(dragged.display_label(), "DC Bus Power");
        assert_eq!(dragged.signal_id(), "dc_bus_power_kw");
        assert_eq!(dragged.unit(), "kW");

        let mut scope = crate::ui::scope::Scope::new_empty(1);
        scope.apply_dragged_signal(dragged);
        assert_eq!(scope.title, "Scope: dc_bus_power_kw");

        let mut gps = crate::ui::gps_plot::GpsPlot::new(1);
        gps.set_overlay_signal("dc_bus_power_kw".to_owned(), "kW".to_owned());
        assert_eq!(
            gps.overlay_signal(),
            Some(&("dc_bus_power_kw".to_owned(), "kW".to_owned()))
        );
    }

    #[test]
    fn sidebar_width_remains_stable_across_frames_when_searching() {
        let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let dbc_path = manifest_dir.join("../../superdbc/superdbc_per26.json");
        let database = daqcore::superdbc::database::Database::load(&dbc_path).expect("load PER26");
        let selection = daqcore::can_thread::BusSelection::default_single(&database)
            .unwrap_or(daqcore::can_thread::BusSelection::Single(0));
        let db_sel = app::DatabaseSelection {
            database,
            selection,
        };

        let t0 = daqcore::time::Time::from_unix_millis(1_788_719_789_000);
        let mut router =
            daqcore::router::VirtualTimelineRouter::live(t0, 30.0).expect("live router");
        let mut state = SignalBrowserState::default();
        let mut action_queue = Vec::new();
        let mut diag = None;

        let ctx = eframe::egui::Context::default();
        egui_extras::install_image_loaders(&ctx);
        crate::ui::theme::store_theme(
            &ctx,
            crate::ui::theme::ThemeSelection::default().get_colors(),
        );

        let search_id = eframe::egui::Id::new("signal_browser_search_input");
        let mut widths = Vec::new();
        for frame in 0..40 {
            let mut events = Vec::new();
            if frame == 2 {
                events.push(eframe::egui::Event::Text("t".to_owned()));
            } else if frame == 3 {
                events.push(eframe::egui::Event::Text("e".to_owned()));
            } else if frame == 4 {
                events.push(eframe::egui::Event::Text("m".to_owned()));
            } else if frame == 5 {
                events.push(eframe::egui::Event::Text("p".to_owned()));
            }
            let raw_input = eframe::egui::RawInput {
                screen_rect: Some(eframe::egui::Rect::from_min_size(
                    eframe::egui::Pos2::ZERO,
                    eframe::egui::vec2(1280.0, 600.0),
                )),
                time: Some(frame as f64 * 0.016),
                events,
                ..Default::default()
            };
            let _ = ctx.run(raw_input, |ctx| {
                if frame == 1 {
                    ctx.memory_mut(|m| m.request_focus(search_id));
                }
                eframe::egui::SidePanel::left("left_sidebar")
                    .resizable(true)
                    .default_width(310.0)
                    .width_range(220.0..=520.0)
                    .show_animated(ctx, true, |ui| {
                        let panel_width = ui.available_width();
                        ui.set_max_width(panel_width);
                        eframe::egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.set_max_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    ui.add(
                                        eframe::egui::Image::from_bytes(
                                            crate::assets::PER_LOGO_PATH,
                                            crate::assets::PER_LOGO_BYTES,
                                        )
                                        .show_loading_spinner(true)
                                        .corner_radius(5),
                                    );
                                    ui.heading("Side bar");
                                });
                                show_signal_browser(
                                    ui,
                                    &mut state,
                                    Some(&db_sel),
                                    &mut router,
                                    &mut action_queue,
                                    &mut diag,
                                );
                            });
                    });
                if frame >= 2 {
                    assert!(
                        ctx.memory(|m| m.has_focus(search_id)),
                        "Search input lost focus on frame {frame}"
                    );
                }
                let left_after = ctx.available_rect().left();
                widths.push(left_after);
                eframe::egui::CentralPanel::default().show(ctx, |ui| {
                    ui.label("workspace");
                });
            });
        }
        assert_eq!(state.search_query, "temp");
        let first_w = widths[1];
        let last_w = *widths.last().unwrap();
        assert!(
            (last_w - first_w).abs() < 1.0,
            "Sidebar width crept from {first_w} to {last_w} across 40 frames: {widths:?}"
        );
    }
}
