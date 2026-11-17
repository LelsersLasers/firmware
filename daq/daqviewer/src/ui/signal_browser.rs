use crate::{action, ui, widget_constructor};
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
            updated_at_ms: daqcore::time::Time::now().unix_millis(),
            deleted: false,
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
    pub show_filters: bool,
    pub tree_mode: bool,
    pub hover_utc_ms: Option<i64>,
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

fn format_sidebar_value(v: f64) -> String {
    if !v.is_finite() {
        return String::new();
    }
    let rounded = v.round();
    if (v - rounded).abs() < 1e-6 && v.abs() < 1e9 {
        let int_val = if rounded == 0.0 { 0.0 } else { rounded };
        format!("{int_val:.0}")
    } else if v.abs() >= 1000.0 {
        format!("{v:.1}")
    } else if v.abs() >= 1.0 {
        format!("{v:.2}")
    } else {
        format!("{v:.3}")
    }
}

/// Render a single clean Nominal / AdvantageScope-style channel row:
/// `[icon]  channel_name                           [latest_val] [unit]`
/// No permanent `[Plot]` / `[Edit]` buttons cluttering the row.
#[allow(clippy::too_many_arguments)]
fn show_clean_channel_row(
    ui: &mut eframe::egui::Ui,
    dnd_id: eframe::egui::Id,
    icon: &str,
    icon_color: eframe::egui::Color32,
    canonical_id: &str,
    primary_text: &str,
    unit_text: &str,
    hover_tooltip: &str,
    drag_payload: DraggedSignal,
    edit_formula: Option<&daqcore::analysis::CompiledDerivedSignal>,
    cursor_utc_ms: i64,
    router: &mut daqcore::router::VirtualTimelineRouter,
    state: &mut SignalBrowserState,
    action_queue: &mut Vec<action::AppAction>,
    muted_color: eframe::egui::Color32,
) {
    let row_w = ui.available_width().max(80.0);
    let row_h = 20.0_f32;

    let dnd_resp = ui.dnd_drag_source(dnd_id, drag_payload.clone(), |ui| {
        let (rect, _) = ui.allocate_exact_size(
            eframe::egui::vec2(row_w, row_h),
            eframe::egui::Sense::hover(),
        );
        if !ui.is_rect_visible(rect) {
            return;
        }
        let is_hovered = ui.rect_contains_pointer(rect);
        let painter = ui.painter_at(rect.intersect(ui.clip_rect()));

        if is_hovered {
            painter.rect_filled(
                rect,
                3.0,
                ui.visuals()
                    .widgets
                    .hovered
                    .bg_fill
                    .gamma_multiply(0.65_f32),
            );
        }

        let latest_val_str = router
            .latest_signal_value_at(canonical_id, cursor_utc_ms)
            .map(|(_, v)| format_sidebar_value(v))
            .filter(|s| !s.is_empty());

        // Right-aligned live/cursor value + unit (AdvantageScope / Nominal style)
        let mut right_cursor = rect.right() - 4.0;
        if !unit_text.is_empty() {
            let unit_galley = painter.layout_no_wrap(
                unit_text.to_owned(),
                eframe::egui::FontId::monospace(10.5),
                muted_color,
            );
            let uw = unit_galley.size().x;
            painter.galley(
                eframe::egui::pos2(
                    right_cursor - uw,
                    rect.center().y - unit_galley.size().y * 0.5,
                ),
                unit_galley,
                muted_color,
            );
            right_cursor -= uw + 4.0;
        }

        if let Some(val_str) = &latest_val_str {
            let val_color = ui.visuals().strong_text_color();
            let val_galley = painter.layout_no_wrap(
                val_str.clone(),
                eframe::egui::FontId::monospace(11.0),
                val_color,
            );
            let vw = val_galley.size().x;
            painter.galley(
                eframe::egui::pos2(
                    right_cursor - vw,
                    rect.center().y - val_galley.size().y * 0.5,
                ),
                val_galley,
                val_color,
            );
            right_cursor -= vw + 6.0;
        } else {
            right_cursor -= 4.0;
        }

        // Left icon (`ƒ` or `•`)
        painter.text(
            eframe::egui::pos2(rect.left() + 5.0, rect.center().y),
            eframe::egui::Align2::LEFT_CENTER,
            icon,
            eframe::egui::FontId::monospace(11.0),
            icon_color,
        );

        // Primary channel name clipped cleanly before the value/unit column
        let text_left = rect.left() + 18.0;
        let text_right = right_cursor.max(text_left + 16.0);
        let text_clip = eframe::egui::Rect::from_min_max(
            eframe::egui::pos2(text_left, rect.top()),
            eframe::egui::pos2(text_right, rect.bottom()),
        )
        .intersect(ui.clip_rect());

        if text_clip.width() > 4.0 {
            ui.painter_at(text_clip).text(
                eframe::egui::pos2(text_left, rect.center().y),
                eframe::egui::Align2::LEFT_CENTER,
                primary_text,
                eframe::egui::FontId::monospace(11.5),
                ui.visuals().text_color(),
            );
        }
    });

    let row_resp = ui
        .interact(
            dnd_resp.response.rect,
            dnd_id,
            eframe::egui::Sense::click_and_drag(),
        )
        .on_hover_text(hover_tooltip);
    if row_resp.double_clicked() {
        action_queue.push(action::AppAction::SpawnWidget(
            drag_payload.to_widget_constructor(),
        ));
    }
    row_resp.context_menu(|ui| {
        if ui.button("Plot in new Scope").clicked() {
            action_queue.push(action::AppAction::SpawnWidget(
                drag_payload.to_widget_constructor(),
            ));
            ui.close();
        }
        if ui.button("Open in new State Lane").clicked() {
            action_queue.push(action::AppAction::SpawnWidget(
                widget_constructor::WidgetConstructor::StateLaneSingle {
                    signal_id: drag_payload.signal_id(),
                    label: drag_payload.display_label(),
                },
            ));
            ui.close();
        }
        if let Some(compiled) = edit_formula {
            if ui.button("Edit Formula…").clicked() {
                state.formula_draft = Some(FormulaDraft::from_compiled(compiled));
                ui.close();
            }
            if ui.button("Delete Formula").clicked() {
                let _ = router.delete_derived_signal(&compiled.def.id);
                ui.close();
            }
        }
    });
}

pub fn show_signal_browser(
    ui: &mut eframe::egui::Ui,
    state: &mut SignalBrowserState,
    router: &mut daqcore::router::VirtualTimelineRouter,
    action_queue: &mut Vec<action::AppAction>,
    diagnostic: &mut Option<String>,
) {
    let theme = ui::theme::get_theme(ui.ctx());
    let accent_color = theme.accent_color();
    let muted_color = theme.muted_text_color();

    ui.set_max_width(ui.available_width());

    // Nominal-style compact header: `CHANNELS` on left, view/filter/new buttons on right
    let header_w = ui.available_width();
    ui.allocate_ui_with_layout(
        eframe::egui::vec2(header_w, 22.0),
        eframe::egui::Layout::left_to_right(eframe::egui::Align::Center),
        |ui| {
            ui.set_max_width(header_w);
            ui.label(
                eframe::egui::RichText::new("CHANNELS")
                    .small()
                    .strong()
                    .color(muted_color),
            );
            ui.with_layout(
                eframe::egui::Layout::right_to_left(eframe::egui::Align::Center),
                |ui| {
                    if ui
                        .small_button("+")
                        .on_hover_text("New derived formula channel")
                        .clicked()
                    {
                        state.formula_draft = Some(FormulaDraft::default());
                    }
                    let has_active_filter =
                        state.bus_filter.is_some() || state.node_filter.is_some();
                    if ui
                        .selectable_label(state.show_filters || has_active_filter, "Filter")
                        .on_hover_text("Filter channels by CAN Bus or Transmitter Node")
                        .clicked()
                    {
                        state.show_filters = !state.show_filters;
                    }
                    if ui
                        .selectable_label(
                            state.tree_mode,
                            if state.tree_mode { "Tree" } else { "Flat" },
                        )
                        .on_hover_text(
                            "Toggle between Flat channel list (Nominal style) and Message Tree",
                        )
                        .clicked()
                    {
                        state.tree_mode = !state.tree_mode;
                    }
                },
            );
        },
    );

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
                    eframe::egui::Button::new("×").small(),
                )
                .on_hover_text("Clear search filter")
                .clicked()
            {
                state.search_query.clear();
            }
            let edit_width = (ui.available_width() - 14.0).max(40.0);
            let resp = ui.add(
                eframe::egui::TextEdit::singleline(&mut state.search_query)
                    .id(eframe::egui::Id::new("signal_browser_search_input"))
                    .desired_width(edit_width)
                    .hint_text("🔍 Search channel, unit, or glob… (/)"),
            );
            if resp.has_focus() && ui.input(|i| i.key_pressed(eframe::egui::Key::Escape)) {
                resp.surrender_focus();
            }
        },
    );

    // Build message groups strictly from signals active in the current timeline viewport
    let mut bus_names: BTreeSet<String> = BTreeSet::new();
    let mut node_names: BTreeSet<String> = BTreeSet::new();
    let mut groups_by_msg: BTreeMap<String, MessageGroup> = BTreeMap::new();

    for meta in router.active_signals_in_viewport() {
        let (msg_name, sig_name) = meta.split_message_signal();
        if msg_name.is_empty() {
            continue;
        }
        let bus_name = if meta.bus_name.is_empty() {
            "CAN".to_owned()
        } else {
            meta.bus_name.clone()
        };
        let transmitter = if meta.transmitter.is_empty() {
            "Unknown".to_owned()
        } else {
            meta.transmitter.clone()
        };
        bus_names.insert(bus_name.clone());
        node_names.insert(transmitter.clone());

        let key = (meta.can_id != 0 || !meta.bus_name.is_empty()).then_some(
            daqcore::superdbc::message::MessageKey {
                bus_id: meta.bus_id,
                identity: meta.can_id,
            },
        );

        let group = groups_by_msg
            .entry(msg_name.to_owned())
            .or_insert_with(|| MessageGroup {
                key,
                bus_name,
                transmitter,
                msg_name: msg_name.to_owned(),
                description: String::new(),
                signals: Vec::new(),
            });
        if !group.signals.iter().any(|s| s.signal_name == sig_name) {
            group.signals.push(SignalLeaf {
                key: group.key,
                msg_name: msg_name.to_owned(),
                signal_name: sig_name.to_owned(),
                unit: meta.unit,
                description: meta.description,
            });
        }
    }

    // Optional Bus & Node Filter tags (shown only when toggled or active)
    if (state.show_filters || state.bus_filter.is_some() || state.node_filter.is_some())
        && (!bus_names.is_empty() || !node_names.is_empty())
    {
        ui.horizontal_wrapped(|ui| {
            let bus_label = state.bus_filter.as_deref().unwrap_or("All Buses");
            eframe::egui::ComboBox::from_id_salt("signal_browser_bus_filter")
                .width(92.0)
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
                .width(104.0)
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

    ui.add_space(2.0);

    ui.scope(|ui| {
        ui.set_max_width(ui.available_width());
        ui.style_mut().wrap_mode = Some(eframe::egui::TextWrapMode::Truncate);
        ui.spacing_mut().item_spacing.y = 1.0;

        // 1. Derived Signals (Formulas)
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

        // 2. Filtered CAN Messages & Signals
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

        let cursor_utc_ms = state
            .hover_utc_ms
            .unwrap_or_else(|| router.timeline().setpoint_utc_ms());

        if !state.tree_mode {
            // -----------------------------------------------------------------
            // Clean Nominal-style Flat Channel List
            // -----------------------------------------------------------------
            for compiled in &matching_formulas {
                let def = &compiled.def;
                let drag_payload = DraggedSignal::Derived {
                    signal_id: def.id.clone(),
                    label: def.label.clone(),
                    unit: def.unit.clone(),
                };
                let tooltip = format!(
                    "Formula: {} ({})\nExpression: {}\nDependencies: {}\n\nDrag onto a plot, double-click to open in Scope, or right-click to edit",
                    def.id,
                    def.label,
                    def.expression,
                    if compiled.dependencies.is_empty() {
                        "(none)".to_owned()
                    } else {
                        compiled.dependencies.join(", ")
                    }
                );
                show_clean_channel_row(
                    ui,
                    eframe::egui::Id::new(("dnd_derived_flat", &def.id)),
                    "ƒ",
                    accent_color,
                    &def.id,
                    &def.id,
                    &def.unit,
                    &tooltip,
                    drag_payload,
                    Some(compiled),
                    cursor_utc_ms,
                    router,
                    state,
                    action_queue,
                    muted_color,
                );
            }

            for (group, sigs) in &filtered_groups {
                for sig in sigs {
                    let canonical_id =
                        daqcore::storage::format_signal_id(&sig.msg_name, &sig.signal_name);
                    let drag_payload = DraggedSignal::Raw {
                        key: sig.key,
                        msg_name: sig.msg_name.clone(),
                        signal_name: sig.signal_name.clone(),
                        unit: sig.unit.clone(),
                    };
                    let tooltip = format!(
                        "Channel: {}\nBus / Node: {} · {}\nUnit: {}\n{}\n\nDrag onto a plot, double-click to open in Scope, or right-click for options",
                        canonical_id,
                        group.bus_name,
                        group.transmitter,
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
                    );
                    show_clean_channel_row(
                        ui,
                        eframe::egui::Id::new(("dnd_raw_flat", &canonical_id)),
                        "•",
                        muted_color,
                        &canonical_id,
                        &canonical_id,
                        &sig.unit,
                        &tooltip,
                        drag_payload,
                        None,
                        cursor_utc_ms,
                        router,
                        state,
                        action_queue,
                        muted_color,
                    );
                }
            }

            if matching_formulas.is_empty() && filtered_groups.is_empty() {
                ui.label(
                    eframe::egui::RichText::new(
                        "No matching CAN messages or signals in the active timeline range.",
                    )
                    .weak()
                    .font(eframe::egui::FontId::monospace(11.5)),
                );
            }
        } else {
            // -----------------------------------------------------------------
            // Grouped Tree Mode (matching 11.5pt monospace typography of Flat view)
            // -----------------------------------------------------------------
            if !matching_formulas.is_empty() {
                eframe::egui::CollapsingHeader::new(
                    eframe::egui::RichText::new(format!(
                        "ƒ Derived Formulas ({})",
                        matching_formulas.len()
                    ))
                    .font(eframe::egui::FontId::monospace(11.5))
                    .color(accent_color),
                )
                .id_salt("signal_browser_derived_folder")
                .default_open(true)
                .show(ui, |ui| {
                    for compiled in &matching_formulas {
                        let def = &compiled.def;
                        let drag_payload = DraggedSignal::Derived {
                            signal_id: def.id.clone(),
                            label: def.label.clone(),
                            unit: def.unit.clone(),
                        };
                        let tooltip = format!(
                            "Formula: {} ({})\nExpression: {}\nRight-click to edit",
                            def.id, def.label, def.expression
                        );
                        show_clean_channel_row(
                            ui,
                            eframe::egui::Id::new(("dnd_derived_signal", &def.id)),
                            "ƒ",
                            accent_color,
                            &def.id,
                            &def.id,
                            &def.unit,
                            &tooltip,
                            drag_payload,
                            Some(compiled),
                            cursor_utc_ms,
                            router,
                            state,
                            action_queue,
                            muted_color,
                        );
                    }
                });
            }

            let query_active = !state.search_query.trim().is_empty();
            let _ = visible_sig_count;
            for (group, sigs) in filtered_groups {
                let header_label = format!("{} ({})", group.msg_name, sigs.len());
                let mut header = eframe::egui::CollapsingHeader::new(
                    eframe::egui::RichText::new(header_label)
                        .font(eframe::egui::FontId::monospace(11.5))
                        .color(ui.visuals().text_color()),
                )
                .id_salt(("sig_browser_msg", &group.msg_name));
                if query_active && visible_msg_count <= 25 {
                    header = header.open(Some(true));
                }

                header.show(ui, |ui| {
                    for sig in sigs {
                        let canonical_id = daqcore::storage::format_signal_id(
                            &sig.msg_name,
                            &sig.signal_name,
                        );
                        let drag_payload = DraggedSignal::Raw {
                            key: sig.key,
                            msg_name: sig.msg_name.clone(),
                            signal_name: sig.signal_name.clone(),
                            unit: sig.unit.clone(),
                        };
                        let tooltip = format!(
                            "Channel: {}\nBus: {} · Node: {}\n{}",
                            canonical_id, group.bus_name, group.transmitter, sig.description
                        );
                        show_clean_channel_row(
                            ui,
                            eframe::egui::Id::new(("dnd_raw_signal", &canonical_id)),
                            "•",
                            muted_color,
                            &canonical_id,
                            &sig.signal_name,
                            &sig.unit,
                            &tooltip,
                            drag_payload,
                            None,
                            cursor_utc_ms,
                            router,
                            state,
                            action_queue,
                            muted_color,
                        );
                    }
                });
            }
        }
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
    let mut delete_clicked = false;

    let validation = draft.validate(router.formula_engine());
    let title = if draft.is_editing_existing {
        "Edit Derived Formula Channel"
    } else {
        "New Derived Formula Channel"
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
                            "✔ Valid expression · Referenced signals ({}): {}",
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
                    .add_enabled(is_valid, eframe::egui::Button::new("Save Formula"))
                    .clicked()
                {
                    save_clicked = true;
                }
                if ui
                    .add_enabled(
                        is_valid,
                        eframe::egui::Button::new("Save & Open in Scope"),
                    )
                    .clicked()
                {
                    save_and_plot_clicked = true;
                }
                if draft.is_editing_existing && ui.button("Delete Formula").clicked() {
                    delete_clicked = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel_clicked = true;
                }
            });
        });

    if delete_clicked {
        let _ = router.delete_derived_signal(&draft.id);
        state.formula_draft = None;
    } else if save_clicked || save_and_plot_clicked {
        let def = draft.to_def();
        match router.save_derived_signal(def.clone()) {
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
        for tree_mode in [false, true] {
            state.tree_mode = tree_mode;
            let raw_input = eframe::egui::RawInput {
                screen_rect: Some(eframe::egui::Rect::from_min_size(
                    eframe::egui::Pos2::ZERO,
                    eframe::egui::vec2(420.0, 720.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(raw_input, |ctx| {
                eframe::egui::CentralPanel::default().show(ctx, |ui| {
                    show_signal_browser(ui, &mut state, &mut router, &mut action_queue, &mut diag);
                });
                show_formula_editor_modal(
                    ctx,
                    &mut state,
                    &mut router,
                    &mut action_queue,
                    &mut diag,
                );
            });
        }

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
        assert_eq!(format_sidebar_value(0.0), "0");
        assert_eq!(format_sidebar_value(-0.0), "0");
        assert_eq!(format_sidebar_value(504.59), "504.59");
        assert_eq!(format_sidebar_value(3633.05), "3633.1");
        assert_eq!(format_sidebar_value(0.352), "0.352");
        assert_eq!(format_sidebar_value(f64::NAN), "");
    }

    #[test]
    fn sidebar_width_remains_stable_across_frames_when_searching() {
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
                                show_signal_browser(
                                    ui,
                                    &mut state,
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

    #[test]
    fn right_click_formula_row_opens_context_menu_and_edit_modal() {
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

        let formula_id = router
            .formula_engine_mut()
            .definitions()
            .first()
            .expect("builtin formula")
            .def
            .id
            .clone();

        let mut formula_center = None;
        let mut edit_btn_center = None;

        for frame in 0..8 {
            let mut events = Vec::new();
            if frame == 2 {
                let pos = formula_center.expect("formula row rect from frame 1");
                events.push(eframe::egui::Event::PointerMoved(pos));
                events.push(eframe::egui::Event::PointerButton {
                    pos,
                    button: eframe::egui::PointerButton::Secondary,
                    pressed: true,
                    modifiers: eframe::egui::Modifiers::NONE,
                });
            } else if frame == 3 {
                let pos = formula_center.expect("formula row rect");
                events.push(eframe::egui::Event::PointerButton {
                    pos,
                    button: eframe::egui::PointerButton::Secondary,
                    pressed: false,
                    modifiers: eframe::egui::Modifiers::NONE,
                });
            } else if frame == 5 {
                let pos = edit_btn_center.expect("Edit Formula button rect from frame 4");
                events.push(eframe::egui::Event::PointerMoved(pos));
                events.push(eframe::egui::Event::PointerButton {
                    pos,
                    button: eframe::egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: eframe::egui::Modifiers::NONE,
                });
            } else if frame == 6 {
                let pos = edit_btn_center.expect("Edit Formula button rect");
                events.push(eframe::egui::Event::PointerButton {
                    pos,
                    button: eframe::egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: eframe::egui::Modifiers::NONE,
                });
            }

            let raw_input = eframe::egui::RawInput {
                screen_rect: Some(eframe::egui::Rect::from_min_size(
                    eframe::egui::Pos2::ZERO,
                    eframe::egui::vec2(1280.0, 720.0),
                )),
                time: Some(frame as f64 * 0.016),
                events,
                ..Default::default()
            };
            let output = ctx.run(raw_input, |ctx| {
                eframe::egui::SidePanel::left("left_sidebar")
                    .default_width(310.0)
                    .show(ctx, |ui| {
                        show_signal_browser(
                            ui,
                            &mut state,
                            &mut router,
                            &mut action_queue,
                            &mut diag,
                        );
                    });
            });

            if frame == 1 {
                for prim in &output.shapes {
                    if let eframe::egui::epaint::Shape::Text(text) = &prim.shape
                        && text.galley.text() == formula_id
                    {
                        formula_center = Some(prim.shape.visual_bounding_rect().center());
                    }
                }
            }
            if frame == 4 {
                for prim in &output.shapes {
                    if let eframe::egui::epaint::Shape::Text(text) = &prim.shape
                        && text.galley.text().contains("Edit Formula")
                    {
                        edit_btn_center = Some(prim.shape.visual_bounding_rect().center());
                    }
                }
            }
        }

        let draft = state
            .formula_draft
            .expect("Right-clicking formula row and clicking Edit Formula should open draft");
        assert_eq!(draft.id, formula_id);
        assert!(draft.is_editing_existing);
    }

    #[test]
    fn ui_symbols_have_real_font_glyphs_not_replacement_squares() {
        fn collect_rs_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        collect_rs_files(&path, out);
                    } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                        out.push(path);
                    }
                }
            }
        }

        let src_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        collect_rs_files(&src_dir, &mut files);

        let mut non_ascii_chars = std::collections::BTreeSet::new();
        for file in files {
            let content = std::fs::read_to_string(&file).expect("read source file");
            for line in content.lines() {
                let code_part = line.split("//").next().unwrap_or("");
                for ch in code_part.chars() {
                    if !ch.is_ascii() && ch != '\u{FFFF}' {
                        non_ascii_chars.insert(ch);
                    }
                }
            }
        }

        let ctx = eframe::egui::Context::default();
        let font_id = eframe::egui::FontId::proportional(14.0);
        let _ = ctx.run(eframe::egui::RawInput::default(), |ctx| {
            ctx.fonts_mut(|f| {
                let tofu_galley = f.layout_no_wrap(
                    "\u{FFFF}".to_owned(),
                    font_id.clone(),
                    eframe::egui::Color32::WHITE,
                );
                let tofu_uv = tofu_galley.rows[0].glyphs[0].uv_rect;

                for ch in &non_ascii_chars {
                    let g = f.layout_no_wrap(
                        ch.to_string(),
                        font_id.clone(),
                        eframe::egui::Color32::WHITE,
                    );
                    let uv = g.rows[0].glyphs[0].uv_rect;
                    assert_ne!(
                        uv,
                        tofu_uv,
                        "Character {:?} (U+{:04X}) has no glyph in egui's default font and renders as a tofu square",
                        ch,
                        *ch as u32
                    );
                }
            });
        });
    }

    #[test]
    fn historical_viewport_filtering_works_without_superdbc() {
        let t0 = daqcore::time::Time::from_unix_millis(30_000);
        let mut router =
            daqcore::router::VirtualTimelineRouter::live(t0, 5.0).expect("live router");

        let manifest_a = daqcore::storage::PartitionManifest {
            partition_id: "part_a".to_owned(),
            track_day_id: "2026-09-06".to_owned(),
            start_utc_ms: 1_500,
            end_utc_ms: 1_500,
            clock_mapping: daqcore::log_parse::correlate::ClockMapping {
                slope: 1.0,
                intercept_ms: 1_000.0,
                confidence: daqcore::log_parse::correlate::ClockConfidence::RtcEstimated,
            },
            raw_log_hashes: Vec::new(),
            superdbc_hash: "0123".repeat(16),
            signals: vec![daqcore::storage::SignalManifestEntry {
                signal_id: "msg_alpha.sig_a".to_owned(),
                unit: "V".to_owned(),
                is_discrete: false,
                bus_name: "Main".to_owned(),
                bus_id: 0,
                can_id: 257,
                transmitter: "VCU".to_owned(),
                nominal_period_ms: Some(10),
                description: "Alpha voltage".to_owned(),
                sample_count: 1,
                min_utc_ms: 1_500,
                max_utc_ms: 1_500,
                min_val: 42.5,
                max_val: 42.5,
            }],
            annotations: Vec::new(),
        };
        let manifest_b = daqcore::storage::PartitionManifest {
            partition_id: "part_b".to_owned(),
            track_day_id: "2026-09-06".to_owned(),
            start_utc_ms: 21_000,
            end_utc_ms: 21_000,
            clock_mapping: daqcore::log_parse::correlate::ClockMapping {
                slope: 1.0,
                intercept_ms: 20_000.0,
                confidence: daqcore::log_parse::correlate::ClockConfidence::RtcEstimated,
            },
            raw_log_hashes: Vec::new(),
            superdbc_hash: "0123".repeat(16),
            signals: vec![daqcore::storage::SignalManifestEntry {
                signal_id: "msg_beta.sig_b".to_owned(),
                unit: "C".to_owned(),
                is_discrete: false,
                bus_name: "Accum".to_owned(),
                bus_id: 1,
                can_id: 514,
                transmitter: "BMS".to_owned(),
                nominal_period_ms: Some(50),
                description: "Beta temperature".to_owned(),
                sample_count: 1,
                min_utc_ms: 21_000,
                max_utc_ms: 21_000,
                min_val: 88.0,
                max_val: 88.0,
            }],
            annotations: Vec::new(),
        };

        router
            .register_and_preload_manifest(&manifest_a, std::path::Path::new(""))
            .expect("register part_a");
        router
            .register_and_preload_manifest(&manifest_b, std::path::Path::new(""))
            .expect("register part_b");

        let mut ov_a = std::collections::HashMap::new();
        ov_a.insert(
            "msg_alpha.sig_a".to_owned(),
            std::sync::Arc::new(daqcore::router::SignalSeries::Envelope(vec![
                daqcore::router::EnvelopePoint {
                    utc_ms: 1_000,
                    min: 42.5,
                    max: 42.5,
                    mean: 42.5,
                },
            ])),
        );
        router
            .tile_cache_mut()
            .insert_preload_payload(daqcore::router::PartitionPreloadPayload {
                partition_id: "part_a".to_owned(),
                overview_1hz: ov_a,
                activity_1hz: vec![(1_000, 1)],
                states: std::collections::HashMap::new(),
                signals: manifest_a.signals.clone(),
            });

        let mut ov_b = std::collections::HashMap::new();
        ov_b.insert(
            "msg_beta.sig_b".to_owned(),
            std::sync::Arc::new(daqcore::router::SignalSeries::Envelope(vec![
                daqcore::router::EnvelopePoint {
                    utc_ms: 21_000,
                    min: 88.0,
                    max: 88.0,
                    mean: 88.0,
                },
            ])),
        );
        router
            .tile_cache_mut()
            .insert_preload_payload(daqcore::router::PartitionPreloadPayload {
                partition_id: "part_b".to_owned(),
                overview_1hz: ov_b,
                activity_1hz: vec![(21_000, 1)],
                states: std::collections::HashMap::new(),
                signals: manifest_b.signals.clone(),
            });

        // 1. Viewport on Partition A ([1_000..=3_000]): only msg_alpha.sig_a is active
        router.timeline_mut().snap_to_range_ms(1_000, 3_000);
        let active_a = router.active_signals_in_viewport();
        assert_eq!(active_a.len(), 1);
        let meta_a = active_a
            .iter()
            .find(|m| m.signal_id == "msg_alpha.sig_a")
            .expect("msg_alpha.sig_a active");
        assert_eq!(meta_a.bus_name, "Main");
        assert_eq!(meta_a.can_id, 257);
        assert_eq!(meta_a.transmitter, "VCU");
        assert_eq!(meta_a.nominal_period_ms, Some(10));
        assert_eq!(meta_a.unit, "V");
        assert_eq!(meta_a.description, "Alpha voltage");
        assert!(!active_a.iter().any(|m| m.signal_id == "msg_beta.sig_b"));

        // Render SignalBrowser, ViewerTable, ViewerList, Jitter without any SuperDBC
        let ctx = eframe::egui::Context::default();
        crate::ui::theme::store_theme(
            &ctx,
            crate::ui::theme::ThemeSelection::default().get_colors(),
        );
        let mut browser = SignalBrowserState::default();
        let mut viewer_table = crate::ui::viewer_table::ViewerTable::new(1);
        let viewer_list = crate::ui::viewer_list::ViewerList::new(1);
        let mut jitter = crate::ui::jitter::Jitter::new(1);
        let mut actions = Vec::new();
        let mut diag = None;
        let formatter = None;

        let raw_input = eframe::egui::RawInput {
            screen_rect: Some(eframe::egui::Rect::from_min_size(
                eframe::egui::Pos2::ZERO,
                eframe::egui::vec2(1000.0, 700.0),
            )),
            ..Default::default()
        };
        let _ = ctx.run(raw_input, |ctx| {
            eframe::egui::CentralPanel::default().show(ctx, |ui| {
                show_signal_browser(ui, &mut browser, &mut router, &mut actions, &mut diag);
                let view = crate::workspace_view::WorkspaceDataView::new(&router);
                let _ = viewer_table.show(ui, &mut actions, &formatter, &view);
                let _ = viewer_list.show(ui, &formatter, &view);
                let _ = jitter.show(ui, &view);
            });
        });

        // 2. Viewport on Partition B ([20_000..=23_000]): only msg_beta.sig_b is active
        router.timeline_mut().snap_to_range_ms(20_000, 23_000);
        let active_b = router.active_signals_in_viewport();
        assert_eq!(active_b.len(), 1);
        let meta_b = active_b
            .iter()
            .find(|m| m.signal_id == "msg_beta.sig_b")
            .expect("msg_beta.sig_b active");
        assert_eq!(meta_b.bus_name, "Accum");
        assert_eq!(meta_b.can_id, 514);
        assert_eq!(meta_b.transmitter, "BMS");
        assert_eq!(meta_b.nominal_period_ms, Some(50));
        assert_eq!(meta_b.unit, "C");
        assert!(!active_b.iter().any(|m| m.signal_id == "msg_alpha.sig_a"));

        // 3. Viewport on empty gap ([8_000..=12_000]): no signals active
        router.timeline_mut().snap_to_range_ms(8_000, 12_000);
        assert!(router.active_signals_in_viewport().is_empty());
    }
}
