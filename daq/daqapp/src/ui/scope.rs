use crate::{app, ui, workspace_view};
use std::{fs, mem};

pub const CHANNEL_PALETTE: [eframe::egui::Color32; 8] = [
    eframe::egui::Color32::from_rgb(80, 190, 255),  // Sky Blue
    eframe::egui::Color32::from_rgb(255, 160, 60),  // Amber Orange
    eframe::egui::Color32::from_rgb(100, 230, 140), // Mint Green
    eframe::egui::Color32::from_rgb(235, 110, 245), // Orchid Purple
    eframe::egui::Color32::from_rgb(255, 95, 105),  // Coral Red
    eframe::egui::Color32::from_rgb(70, 225, 220),  // Teal Cyan
    eframe::egui::Color32::from_rgb(250, 220, 80),  // Warm Yellow
    eframe::egui::Color32::from_rgb(165, 140, 255), // Periwinkle
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScopeXAxisMode {
    #[default]
    UtcTime,
    ElapsedTime,
    TrackDistance,
}

impl ScopeXAxisMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::UtcTime => "UTC Time",
            Self::ElapsedTime => "Elapsed (s)",
            Self::TrackDistance => "Distance (m)",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ScopeChannel {
    pub signal_id: String,
    pub label: String,
    pub unit: String,
    pub color: eframe::egui::Color32,
    pub key: Option<daqcore::superdbc::message::MessageKey>,
    pub is_derived: bool,
}

impl ScopeChannel {
    fn title_label(&self) -> &str {
        if self.is_derived || self.key.is_none() {
            &self.signal_id
        } else {
            &self.label
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ChannelStats {
    pub min: f64,
    pub max: f64,
    pub mean: f64,
    pub cursor: Option<f64>,
}

/// Affine mapping between a secondary unit group's physical Y values and the primary Y-axis plot space.
#[derive(Debug, Clone, Copy)]
pub struct UnitAxisTransform {
    pub scale: f64,
    pub offset: f64,
}

impl UnitAxisTransform {
    pub const IDENTITY: Self = Self {
        scale: 1.0,
        offset: 0.0,
    };

    pub fn from_ranges(primary_min: f64, primary_max: f64, sec_min: f64, sec_max: f64) -> Self {
        let p_span = (primary_max - primary_min).abs();
        let s_span = (sec_max - sec_min).abs();
        if p_span < 1e-9 || s_span < 1e-9 || !p_span.is_finite() || !s_span.is_finite() {
            return Self::IDENTITY;
        }
        let scale = p_span / s_span;
        let offset = primary_min - sec_min * scale;
        Self { scale, offset }
    }

    #[inline]
    pub fn phys_to_plot(self, y_phys: f64) -> f64 {
        if !y_phys.is_finite() {
            return f64::NAN;
        }
        self.offset + self.scale * y_phys
    }

    #[inline]
    pub fn plot_to_phys(self, y_plot: f64) -> f64 {
        if self.scale.abs() < 1e-12 {
            y_plot
        } else {
            (y_plot - self.offset) / self.scale
        }
    }
}

enum PickerMode {
    Closed,
    PickingMessage {
        picker: ui::message_picker::MessagePickerState,
    },
    PickingSignal {
        selected_msg: daqcore::superdbc::message::MessageKey,
    },
}

pub struct Scope {
    pub title: String,
    instance_num: usize,
    custom_title: bool,
    channels: Vec<ScopeChannel>,
    picker_mode: PickerMode,
    pub x_axis_mode: ScopeXAxisMode,
    pub speed_signal_id: String,
}

impl Scope {
    pub fn new(
        instance_num: usize,
        key: daqcore::superdbc::message::MessageKey,
        msg_name: String,
        signal_name: String,
    ) -> Self {
        let signal_id = daqcore::storage::format_signal_id(&msg_name, &signal_name);
        let channel = ScopeChannel {
            signal_id,
            label: signal_name.clone(),
            unit: String::new(),
            color: CHANNEL_PALETTE[0],
            key: Some(key),
            is_derived: false,
        };
        Self {
            title: format!("Scope: {signal_name}"),
            instance_num,
            custom_title: false,
            channels: vec![channel],
            picker_mode: PickerMode::Closed,
            x_axis_mode: ScopeXAxisMode::UtcTime,
            speed_signal_id: "gps_speed_heading.gps_speed".to_owned(),
        }
    }

    pub fn new_empty(instance_num: usize) -> Self {
        Self {
            title: format!("Scope #{instance_num}"),
            instance_num,
            custom_title: false,
            channels: Vec::new(),
            picker_mode: PickerMode::PickingMessage {
                picker: ui::message_picker::MessagePickerState::default(),
            },
            x_axis_mode: ScopeXAxisMode::UtcTime,
            speed_signal_id: "gps_speed_heading.gps_speed".to_owned(),
        }
    }

    pub fn new_channel(
        instance_num: usize,
        signal_id: String,
        label: String,
        unit: String,
    ) -> Self {
        let is_derived = !signal_id.contains('.');
        let channel = ScopeChannel {
            signal_id: signal_id.clone(),
            label,
            unit,
            color: CHANNEL_PALETTE[0],
            key: None,
            is_derived,
        };
        Self {
            title: format!("Scope: {signal_id}"),
            instance_num,
            custom_title: false,
            channels: vec![channel],
            picker_mode: PickerMode::Closed,
            x_axis_mode: ScopeXAxisMode::UtcTime,
            speed_signal_id: "gps_speed_heading.gps_speed".to_owned(),
        }
    }

    pub fn new_multi(
        instance_num: usize,
        title: impl Into<String>,
        signals: Vec<(String, String, String)>,
    ) -> Self {
        let mut channels = Vec::with_capacity(signals.len());
        for (idx, (signal_id, label, unit)) in signals.into_iter().enumerate() {
            channels.push(ScopeChannel {
                signal_id,
                label,
                unit,
                color: CHANNEL_PALETTE[idx % CHANNEL_PALETTE.len()],
                key: None,
                is_derived: false,
            });
        }
        Self {
            title: title.into(),
            instance_num,
            custom_title: true,
            channels,
            picker_mode: PickerMode::Closed,
            x_axis_mode: ScopeXAxisMode::UtcTime,
            speed_signal_id: "gps_speed_heading.gps_speed".to_owned(),
        }
    }

    pub fn channels(&self) -> &[ScopeChannel] {
        &self.channels
    }

    pub fn add_channel(
        &mut self,
        signal_id: String,
        label: String,
        unit: String,
        key: Option<daqcore::superdbc::message::MessageKey>,
        is_derived: bool,
    ) {
        if self.channels.iter().any(|c| c.signal_id == signal_id) {
            self.picker_mode = PickerMode::Closed;
            return;
        }
        let color = CHANNEL_PALETTE[self.channels.len() % CHANNEL_PALETTE.len()];
        self.channels.push(ScopeChannel {
            signal_id,
            label,
            unit,
            color,
            key,
            is_derived,
        });
        self.picker_mode = PickerMode::Closed;
        self.refresh_title();
    }

    pub fn remove_channel(&mut self, index: usize) {
        if index < self.channels.len() {
            self.channels.remove(index);
            for (i, ch) in self.channels.iter_mut().enumerate() {
                ch.color = CHANNEL_PALETTE[i % CHANNEL_PALETTE.len()];
            }
            if self.channels.is_empty() {
                self.custom_title = false;
                self.picker_mode = PickerMode::PickingMessage {
                    picker: ui::message_picker::MessagePickerState::default(),
                };
            }
            self.refresh_title();
        }
    }

    fn refresh_title(&mut self) {
        if self.custom_title && !self.channels.is_empty() {
            return;
        }
        match self.channels.as_slice() {
            [] => {
                self.title = format!("Scope #{}", self.instance_num);
            }
            [only] => {
                self.title = format!("Scope: {}", only.title_label());
            }
            [first, rest @ ..] => {
                self.title = format!("Scope: {} +{}", first.title_label(), rest.len());
            }
        }
    }

    pub fn apply_dragged_signal(&mut self, dragged: ui::signal_browser::DraggedSignal) {
        match dragged {
            ui::signal_browser::DraggedSignal::Raw {
                key,
                msg_name,
                signal_name,
                unit,
            } => {
                let signal_id = daqcore::storage::format_signal_id(&msg_name, &signal_name);
                self.add_channel(signal_id, signal_name, unit, key, false);
            }
            ui::signal_browser::DraggedSignal::Derived {
                signal_id,
                label,
                unit,
            } => {
                let display_label = if label.is_empty() {
                    signal_id.clone()
                } else {
                    label
                };
                self.add_channel(signal_id, display_label, unit, None, true);
            }
        }
    }

    pub fn reset_database(&mut self) {
        for ch in &mut self.channels {
            ch.key = None;
        }
    }

    fn show_picker(
        &mut self,
        ui: &mut eframe::egui::Ui,
        database: Option<&app::DatabaseSelection>,
        formula_engine: &daqcore::analysis::FormulaEngine,
    ) {
        eframe::egui::ScrollArea::vertical()
            .id_salt(("scope_picker", self.instance_num))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if !self.channels.is_empty() && ui.button("⬅ Back to plot").clicked() {
                    self.picker_mode = PickerMode::Closed;
                    return;
                }
                self.show_picker_inner(ui, database, formula_engine);
            });
    }

    fn show_picker_inner(
        &mut self,
        ui: &mut eframe::egui::Ui,
        database: Option<&app::DatabaseSelection>,
        formula_engine: &daqcore::analysis::FormulaEngine,
    ) {
        let mut picked_derived: Option<(String, String, String)> = None;
        if matches!(self.picker_mode, PickerMode::PickingMessage { .. }) {
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
                    if ui
                        .add(eframe::egui::Button::new(btn_label).wrap())
                        .clicked()
                    {
                        picked_derived =
                            Some((def.id.clone(), def.label.clone(), def.unit.clone()));
                    }
                }
            });
            ui.separator();
        }

        if let Some((signal_id, label, unit)) = picked_derived {
            let display = if label.is_empty() {
                signal_id.clone()
            } else {
                label
            };
            self.add_channel(signal_id, display, unit, None, true);
            return;
        }

        let Some(database) = database else {
            ui::message_picker::no_database_placeholder(ui);
            return;
        };

        let mode = mem::replace(&mut self.picker_mode, PickerMode::Closed);
        match mode {
            PickerMode::PickingMessage { mut picker } => {
                if let Some(msg) = picker.show(ui, database, true) {
                    self.picker_mode = PickerMode::PickingSignal { selected_msg: msg };
                } else {
                    self.picker_mode = PickerMode::PickingMessage { picker };
                }
            }
            PickerMode::PickingSignal { selected_msg } => {
                let Some(message) = database.database.message_by_key(selected_msg) else {
                    self.picker_mode = PickerMode::PickingMessage {
                        picker: ui::message_picker::MessagePickerState::default(),
                    };
                    return;
                };
                let bus_name = database.bus_name(selected_msg.bus_id);
                let identity = message.identity();

                ui.separator();
                ui.add(
                    eframe::egui::Label::new(
                        eframe::egui::RichText::new(format!(
                            "Selected Message: [{bus_name}] {} (0x{identity:03X}) — pick a signal:",
                            message.name()
                        ))
                        .strong(),
                    )
                    .wrap(),
                );

                let mut picked_signal: Option<(String, String)> = None;
                for sig in message.signals() {
                    let sig_label = if sig.unit().is_empty() {
                        sig.name().to_owned()
                    } else {
                        format!("{} ({})", sig.name(), sig.unit())
                    };
                    if ui
                        .add(eframe::egui::Button::new(sig_label).wrap())
                        .clicked()
                    {
                        picked_signal = Some((sig.name().to_owned(), sig.unit().to_owned()));
                        break;
                    }
                }

                if let Some((signal_name, unit)) = picked_signal {
                    let sig_id = daqcore::storage::format_signal_id(message.name(), &signal_name);
                    self.add_channel(sig_id, signal_name, unit, Some(selected_msg), false);
                } else if ui.button("⬅ Back to message search").clicked() {
                    self.picker_mode = PickerMode::PickingMessage {
                        picker: ui::message_picker::MessagePickerState::default(),
                    };
                } else {
                    self.picker_mode = PickerMode::PickingSignal { selected_msg };
                }
            }
            PickerMode::Closed => {}
        }
    }

    pub fn show(
        &mut self,
        ui: &mut eframe::egui::Ui,
        database: Option<&app::DatabaseSelection>,
        view: &workspace_view::WorkspaceDataView<'_>,
    ) -> egui_tiles::UiResponse {
        let pane_rect = ui.max_rect();
        if let Some(payload) =
            eframe::egui::DragAndDrop::payload::<ui::signal_browser::DraggedSignal>(ui.ctx())
            && ui.rect_contains_pointer(pane_rect)
        {
            ui.painter().rect_stroke(
                pane_rect.shrink(1.0),
                4.0,
                eframe::egui::Stroke::new(2.0_f32, eframe::egui::Color32::from_rgb(80, 190, 255)),
                eframe::egui::StrokeKind::Inside,
            );
            if ui.input(|i| i.pointer.any_released()) {
                self.apply_dragged_signal((*payload).clone());
                let _ = eframe::egui::DragAndDrop::take_payload::<ui::signal_browser::DraggedSignal>(
                    ui.ctx(),
                );
            }
        }

        if self.channels.is_empty() || !matches!(self.picker_mode, PickerMode::Closed) {
            self.show_picker(ui, database, view.router.formula_engine());
            return egui_tiles::UiResponse::None;
        }

        // Resolve units for any channel whose unit was not populated yet.
        for ch in &mut self.channels {
            if ch.unit.is_empty() {
                ch.unit = view.resolve_signal_unit(&ch.signal_id, database);
            }
        }

        let comparison_annotations: Vec<daqcore::storage::Annotation> =
            if view.timeline.is_comparison_mode() {
                let selected_ids = view.timeline.selected_annotation_ids();
                view.router
                    .query_annotations_in_range(i64::MIN / 2..=i64::MAX / 2)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|ann| selected_ids.iter().any(|id| id == &ann.id))
                    .collect()
            } else {
                Vec::new()
            };
        let is_comparison = comparison_annotations.len() >= 2;

        // Top toolbar: title, X-axis mode selector, add/clear channel buttons, CSV export.
        let mut open_picker = false;
        let mut clear_all = false;
        let mut export_csv = false;
        ui.horizontal_wrapped(|ui| {
            ui.strong(&self.title);
            if is_comparison {
                ui.colored_label(
                    eframe::egui::Color32::from_rgb(255, 190, 60),
                    format!("[Overlay: {} laps/events]", comparison_annotations.len()),
                );
            }
            ui.separator();
            ui.label("X:");
            for mode in [
                ScopeXAxisMode::UtcTime,
                ScopeXAxisMode::ElapsedTime,
                ScopeXAxisMode::TrackDistance,
            ] {
                if ui
                    .selectable_label(self.x_axis_mode == mode, mode.label())
                    .clicked()
                {
                    self.x_axis_mode = mode;
                    if mode == ScopeXAxisMode::TrackDistance && self.speed_signal_id.is_empty() {
                        self.speed_signal_id = view.resolve_default_speed_signal(database);
                    }
                }
            }
            if self.x_axis_mode == ScopeXAxisMode::TrackDistance {
                eframe::egui::ComboBox::from_id_salt(("scope_spd_sig", self.instance_num))
                    .selected_text(&self.speed_signal_id)
                    .width(155.0)
                    .show_ui(ui, |ui| {
                        for cand in [
                            "gps_speed_heading.gps_speed",
                            "wheel_speeds.front_left",
                            "wheel_speeds.front_right",
                        ] {
                            ui.selectable_value(&mut self.speed_signal_id, cand.to_owned(), cand);
                        }
                    });
            }
            ui.separator();
            if ui
                .small_button("+ Signal")
                .on_hover_text("Add another signal or derived formula to this Scope")
                .clicked()
            {
                open_picker = true;
            }
            if self.channels.len() > 1 && ui.small_button("Clear").clicked() {
                clear_all = true;
            }
            if ui.small_button("Export CSV").clicked() {
                export_csv = true;
            }
        });

        if open_picker {
            self.picker_mode = PickerMode::PickingMessage {
                picker: ui::message_picker::MessagePickerState::default(),
            };
            return egui_tiles::UiResponse::None;
        }
        if clear_all {
            self.channels.clear();
            self.custom_title = false;
            self.refresh_title();
            self.picker_mode = PickerMode::PickingMessage {
                picker: ui::message_picker::MessagePickerState::default(),
            };
            return egui_tiles::UiResponse::None;
        }

        let width_px = ui.available_width().max(1.0);
        let pixels = width_px as usize;
        let resolution = view.timeline.resolution_for_width(width_px);
        let base_staleness_ms = view.router.formula_engine().max_staleness_ms();
        let max_gap_ms = resolution
            .bucket_width_ms()
            .map_or(base_staleness_ms, |bw| base_staleness_ms.max(bw * 3));

        // Build unit groups in order of appearance so each unique unit gets its own Y-axis.
        let mut unit_groups: Vec<String> = Vec::new();
        let mut channel_unit_group: Vec<usize> = Vec::with_capacity(self.channels.len());
        for ch in &self.channels {
            let pos = unit_groups
                .iter()
                .position(|u| u == &ch.unit)
                .unwrap_or_else(|| {
                    unit_groups.push(ch.unit.clone());
                    unit_groups.len() - 1
                });
            channel_unit_group.push(pos);
        }

        let start = view.timeline.start();
        let start_ms = start.unix_millis();
        let end_ms = view.timeline.end_utc_ms();
        let duration_s = view.timeline.end().secs(start).max(0.001);
        let playhead_s = view.timeline.setpoint().secs(start);

        // Query data and compute per-channel statistics & unit bounds.
        let mut channel_stats: Vec<Option<ChannelStats>> = vec![None; self.channels.len()];
        let mut unit_bounds: Vec<(f64, f64)> = vec![(f64::INFINITY, f64::NEG_INFINITY); unit_groups.len()];

        // Prepared traces to plot (before unit affine normalization):
        let mut raw_traces: Vec<PreparedTrace> = Vec::new();
        let mut max_x_bound = duration_s;
        let mut primary_distance_lookup: Option<(i64, Vec<i64>, Vec<f64>)> = None;

        if is_comparison {
            max_x_bound = 0.001;
            let line_styles = [
                egui_plot::LineStyle::Solid,
                egui_plot::LineStyle::dashed_dense(),
                egui_plot::LineStyle::dotted_dense(),
                egui_plot::LineStyle::dashed_loose(),
            ];

            for (ch_idx, ch) in self.channels.iter().enumerate() {
                let unit_idx = channel_unit_group[ch_idx];
                let mut ch_min = f64::INFINITY;
                let mut ch_max = f64::NEG_INFINITY;
                let mut ch_sum = 0.0;
                let mut ch_cnt = 0usize;

                for (lap_idx, ann) in comparison_annotations.iter().enumerate() {
                    let ann_start_ms = ann.start_utc_ms;
                    let ann_end_ms = ann
                        .end_utc_ms
                        .unwrap_or(ann.start_utc_ms + 30_000)
                        .max(ann.start_utc_ms + 1_000);
                    let ann_res = daqcore::router::Resolution::for_window_ms(
                        (ann_end_ms - ann_start_ms).max(1),
                        width_px,
                    );
                    let style = line_styles[lap_idx % line_styles.len()];
                    let trace_color = if self.channels.len() == 1 {
                        CHANNEL_PALETTE[lap_idx % CHANNEL_PALETTE.len()]
                    } else {
                        ch.color
                    };

                    if self.x_axis_mode == ScopeXAxisMode::TrackDistance {
                        let dist_series = view.query_signal_vs_distance(
                            &ch.signal_id,
                            &self.speed_signal_id,
                            ann_start_ms..=ann_end_ms,
                            ann_res,
                        );
                        let lap_dist_m = dist_series.total_distance_m().max(1.0);
                        max_x_bound = max_x_bound.max(lap_dist_m);
                        for &v in &dist_series.values {
                            if v.is_finite() {
                                ch_min = ch_min.min(v);
                                ch_max = ch_max.max(v);
                                ch_sum += v;
                                ch_cnt += 1;
                            }
                        }
                        if primary_distance_lookup.is_none() && !dist_series.distances_m.is_empty() {
                            primary_distance_lookup = Some((
                                ann_start_ms,
                                dist_series.timestamps_ms.clone(),
                                dist_series.distances_m.clone(),
                            ));
                        }
                        let pts: Vec<[f64; 2]> = dist_series
                            .distances_m
                            .into_iter()
                            .zip(dist_series.values)
                            .map(|(d, v)| [d, v])
                            .collect();
                        let decimated = workspace_view::decimate(&pts, pixels);
                        raw_traces.push(PreparedTrace {
                            name: format!("{} — {} ({:.0}m)", ch.label, ann.name, lap_dist_m),
                            unit_idx,
                            color: trace_color,
                            style,
                            mean_points: decimated,
                            envelope_bands: Vec::new(),
                        });
                    } else {
                        let ann_dur_s = ((ann_end_ms - ann_start_ms) as f64 / 1000.0).max(0.001);
                        max_x_bound = max_x_bound.max(ann_dur_s);
                        let series =
                            view.query_signal_range(&ch.signal_id, ann_start_ms..=ann_end_ms, ann_res);
                        if let Some((s_min, s_max, s_mean)) = series_window_stats(&series) {
                            ch_min = ch_min.min(s_min);
                            ch_max = ch_max.max(s_max);
                            ch_sum += s_mean;
                            ch_cnt += 1;
                        }
                        let (pts, env_bands) =
                            series_to_points_and_bands(&series, ann_start_ms, max_gap_ms, pixels);
                        raw_traces.push(PreparedTrace {
                            name: format!("{} — {} ({:.1}s)", ch.label, ann.name, ann_dur_s),
                            unit_idx,
                            color: trace_color,
                            style,
                            mean_points: pts,
                            envelope_bands: env_bands,
                        });
                    }
                }

                if ch_cnt > 0 && ch_min.is_finite() && ch_max.is_finite() {
                    unit_bounds[unit_idx].0 = unit_bounds[unit_idx].0.min(ch_min);
                    unit_bounds[unit_idx].1 = unit_bounds[unit_idx].1.max(ch_max);
                    let cursor = view
                        .latest_signal_value_at_cursor(&ch.signal_id)
                        .map(|(_, v)| v);
                    channel_stats[ch_idx] = Some(ChannelStats {
                        min: ch_min,
                        max: ch_max,
                        mean: ch_sum / (ch_cnt as f64),
                        cursor,
                    });
                }
            }
        } else {
            // Single-window mode (UTC Time, Elapsed Time, or Track Distance)
            if self.x_axis_mode == ScopeXAxisMode::TrackDistance {
                max_x_bound = 1.0;
                for (ch_idx, ch) in self.channels.iter().enumerate() {
                    let unit_idx = channel_unit_group[ch_idx];
                    let dist_series = view.query_signal_vs_distance(
                        &ch.signal_id,
                        &self.speed_signal_id,
                        start_ms..=end_ms,
                        resolution,
                    );
                    max_x_bound = max_x_bound.max(dist_series.total_distance_m().max(1.0));
                    let mut ch_min = f64::INFINITY;
                    let mut ch_max = f64::NEG_INFINITY;
                    let mut ch_sum = 0.0;
                    let mut ch_cnt = 0usize;
                    for &v in &dist_series.values {
                        if v.is_finite() {
                            ch_min = ch_min.min(v);
                            ch_max = ch_max.max(v);
                            ch_sum += v;
                            ch_cnt += 1;
                        }
                    }
                    if ch_cnt > 0 {
                        unit_bounds[unit_idx].0 = unit_bounds[unit_idx].0.min(ch_min);
                        unit_bounds[unit_idx].1 = unit_bounds[unit_idx].1.max(ch_max);
                        let cursor = view
                            .latest_signal_value_at_cursor(&ch.signal_id)
                            .map(|(_, v)| v);
                        channel_stats[ch_idx] = Some(ChannelStats {
                            min: ch_min,
                            max: ch_max,
                            mean: ch_sum / (ch_cnt as f64),
                            cursor,
                        });
                    }
                    if primary_distance_lookup.is_none() && !dist_series.distances_m.is_empty() {
                        primary_distance_lookup = Some((
                            start_ms,
                            dist_series.timestamps_ms.clone(),
                            dist_series.distances_m.clone(),
                        ));
                    }
                    let pts: Vec<[f64; 2]> = dist_series
                        .distances_m
                        .into_iter()
                        .zip(dist_series.values)
                        .map(|(d, v)| [d, v])
                        .collect();
                    let decimated = workspace_view::decimate(&pts, pixels);
                    raw_traces.push(PreparedTrace {
                        name: ch.label.clone(),
                        unit_idx,
                        color: ch.color,
                        style: egui_plot::LineStyle::Solid,
                        mean_points: decimated,
                        envelope_bands: Vec::new(),
                    });
                }
            } else {
                for (ch_idx, ch) in self.channels.iter().enumerate() {
                    let unit_idx = channel_unit_group[ch_idx];
                    let series = view.query_signal(&ch.signal_id, resolution);
                    if let Some((s_min, s_max, s_mean)) = series_window_stats(&series) {
                        unit_bounds[unit_idx].0 = unit_bounds[unit_idx].0.min(s_min);
                        unit_bounds[unit_idx].1 = unit_bounds[unit_idx].1.max(s_max);
                        let cursor = view
                            .latest_signal_value_at_cursor(&ch.signal_id)
                            .map(|(_, v)| v);
                        channel_stats[ch_idx] = Some(ChannelStats {
                            min: s_min,
                            max: s_max,
                            mean: s_mean,
                            cursor,
                        });
                    }
                    let (pts, env_bands) =
                        series_to_points_and_bands(&series, start_ms, max_gap_ms, pixels);
                    raw_traces.push(PreparedTrace {
                        name: ch.label.clone(),
                        unit_idx,
                        color: ch.color,
                        style: egui_plot::LineStyle::Solid,
                        mean_points: pts,
                        envelope_bands: env_bands,
                    });
                }
            }
        }

        if export_csv
            && let Some(first_trace) = raw_traces.first()
            && let Some(path) = rfd::FileDialog::new()
                .set_file_name(format!("{}_data.csv", self.title.replace([' ', ':'], "_")))
                .add_filter("CSV", &["csv"])
                .save_file()
        {
            let mut text = String::from("X,Value\n");
            for p in &first_trace.mean_points {
                text.push_str(&format!("{},{}\n", p[0], p[1]));
            }
            if let Err(e) = fs::write(path, text) {
                log::error!("Export failed: {e}");
            }
        }

        // Render compact per-channel legend + cursor/min/max/mean statistics strip.
        let mut remove_idx: Option<usize> = None;
        ui.horizontal_wrapped(|ui| {
            for (idx, ch) in self.channels.iter().enumerate() {
                let unit_str = if ch.unit.is_empty() {
                    String::new()
                } else {
                    format!(" {}", ch.unit)
                };
                let stats_text = if let Some(st) = channel_stats[idx] {
                    let cur_str = st
                        .cursor
                        .map_or_else(|| "—".to_owned(), |v| format!("{v:.2}{unit_str}"));
                    let kind_prefix = if ch.is_derived { "ƒ " } else { "" };
                    format!(
                        "■ {}{} = {}  [min {:.2}, max {:.2}, avg {:.2}]",
                        kind_prefix, ch.label, cur_str, st.min, st.max, st.mean
                    )
                } else {
                    format!("■ {}{unit_str} (no samples)", ch.label)
                };
                ui.colored_label(ch.color, stats_text)
                    .on_hover_text(&ch.signal_id);
                if ui
                    .small_button("×")
                    .on_hover_text(format!("Remove {}", ch.signal_id))
                    .clicked()
                {
                    remove_idx = Some(idx);
                }
                ui.add_space(6.0);
            }
        });
        if let Some(idx) = remove_idx {
            self.remove_channel(idx);
            return egui_tiles::UiResponse::None;
        }

        if raw_traces.iter().all(|t| t.mean_points.is_empty()) {
            ui.label("No retained samples in the selected interval.");
        }

        // Normalize unit bounds and build multi-unit Y axes.
        let normalized_bounds: Vec<(f64, f64)> = unit_bounds
            .iter()
            .map(|&(lo, hi)| {
                if !lo.is_finite() || !hi.is_finite() {
                    (-1.0, 1.0)
                } else if (hi - lo).abs() < 1e-6 {
                    let pad = (lo.abs() * 0.05).max(1.0);
                    (lo - pad, hi + pad)
                } else {
                    let pad = (hi - lo) * 0.05;
                    (lo - pad, hi + pad)
                }
            })
            .collect();

        let (primary_min, primary_max) = normalized_bounds.first().copied().unwrap_or((-1.0, 1.0));
        let unit_transforms: Vec<UnitAxisTransform> = normalized_bounds
            .iter()
            .enumerate()
            .map(|(idx, &(lo, hi))| {
                if idx == 0 {
                    UnitAxisTransform::IDENTITY
                } else {
                    UnitAxisTransform::from_ranges(primary_min, primary_max, lo, hi)
                }
            })
            .collect();

        let y_axes: Vec<egui_plot::AxisHints<'static>> = unit_groups
            .iter()
            .enumerate()
            .map(|(u_idx, unit_name)| {
                let placement = if u_idx % 2 == 0 {
                    egui_plot::HPlacement::Left
                } else {
                    egui_plot::HPlacement::Right
                };
                let axis_label = if unit_name.is_empty() {
                    if self.channels.len() == 1 {
                        self.channels[0].label.clone()
                    } else {
                        "Value".to_owned()
                    }
                } else {
                    unit_name.clone()
                };
                let tf = unit_transforms[u_idx];
                let unit_clone = unit_name.clone();
                egui_plot::AxisHints::new_y()
                    .label(axis_label)
                    .placement(placement)
                    .formatter(move |mark, _range| {
                        let phys = tf.plot_to_phys(mark.value);
                        if unit_clone.is_empty() {
                            format!("{phys:.2}")
                        } else {
                            format!("{phys:.2} {unit_clone}")
                        }
                    })
            })
            .collect();

        let x_axis_mode = self.x_axis_mode;
        let x_label = match (is_comparison, x_axis_mode) {
            (_, ScopeXAxisMode::TrackDistance) => "Track Distance (m)",
            (true, _) => "Elapsed Event Time (s)",
            (false, ScopeXAxisMode::ElapsedTime) => "Elapsed Time (s)",
            (false, ScopeXAxisMode::UtcTime) => "UTC Time",
        };

        // Compute playhead X and linked hover X in the current X-axis coordinate system.
        let playhead_x: Option<f64> = if is_comparison {
            None
        } else {
            match x_axis_mode {
                ScopeXAxisMode::UtcTime | ScopeXAxisMode::ElapsedTime => Some(playhead_s),
                ScopeXAxisMode::TrackDistance => primary_distance_lookup
                    .as_ref()
                    .and_then(|(_, ts, ds)| interpolate_distance_at_ms(ts, ds, view.timeline.setpoint_utc_ms())),
            }
        };

        let hover_x: Option<f64> = match x_axis_mode {
            ScopeXAxisMode::TrackDistance => view.hover_distance_m().or_else(|| {
                let hover_ms = view.hover_utc_ms()?;
                let (_, ts, ds) = primary_distance_lookup.as_ref()?;
                interpolate_distance_at_ms(ts, ds, hover_ms)
            }),
            ScopeXAxisMode::UtcTime | ScopeXAxisMode::ElapsedTime => {
                view.hover_utc_ms().map(|hover_ms| {
                    let ref_start_ms = if is_comparison {
                        comparison_annotations
                            .first()
                            .map_or(start_ms, |a| a.start_utc_ms)
                    } else {
                        start_ms
                    };
                    ((hover_ms - ref_start_ms) as f64) / 1000.0
                })
            }
        };

        let plot_width = ui.available_width().max(60.0);
        let plot_height = ui.available_height().max(60.0);

        let plot_id = (
            "scope_plot_v2",
            self.instance_num,
            is_comparison,
            x_axis_mode as u8,
        );
        egui_plot::Plot::new(plot_id)
            .legend(egui_plot::Legend::default())
            .custom_y_axes(y_axes)
            .width(plot_width)
            .height(plot_height)
            .allow_zoom([false, true])
            .allow_drag([false, true])
            .allow_scroll([false, true])
            .x_axis_formatter(move |mark, _| match (is_comparison, x_axis_mode) {
                (_, ScopeXAxisMode::TrackDistance) => format!("{:.0} m", mark.value),
                (true, _) | (false, ScopeXAxisMode::ElapsedTime) => format!("{:.1}s", mark.value),
                (false, ScopeXAxisMode::UtcTime) => {
                    let offset_ms = (mark.value * 1000.0).round() as i64;
                    start.offset(offset_ms).label()
                }
            })
            .x_axis_label(x_label)
            .show(ui, |plot_ui| {
                plot_ui.set_plot_bounds_x(0.0..=max_x_bound);

                for trace in raw_traces {
                    let tf = unit_transforms[trace.unit_idx];
                    let fill_color = trace.color.gamma_multiply(0.22);
                    let band_stroke =
                        eframe::egui::Stroke::new(0.5_f32, trace.color.gamma_multiply(0.35));

                    for quad in trace.envelope_bands {
                        let mapped_quad = vec![
                            [quad[0][0], tf.phys_to_plot(quad[0][1])],
                            [quad[1][0], tf.phys_to_plot(quad[1][1])],
                            [quad[2][0], tf.phys_to_plot(quad[2][1])],
                            [quad[3][0], tf.phys_to_plot(quad[3][1])],
                        ];
                        plot_ui.polygon(
                            egui_plot::Polygon::new("", mapped_quad)
                                .fill_color(fill_color)
                                .stroke(band_stroke)
                                .allow_hover(false),
                        );
                    }

                    let mapped_pts: Vec<[f64; 2]> = trace
                        .mean_points
                        .into_iter()
                        .map(|[x, y]| [x, tf.phys_to_plot(y)])
                        .collect();

                    plot_ui.line(
                        egui_plot::Line::new(trace.name, egui_plot::PlotPoints::from(mapped_pts))
                            .color(trace.color)
                            .style(trace.style),
                    );
                }

                if let Some(px) = playhead_x {
                    plot_ui.vline(
                        egui_plot::VLine::new("", px)
                            .color(eframe::egui::Color32::YELLOW)
                            .width(1.5_f32),
                    );
                }

                if let Some(hx) = hover_x
                    && hx >= 0.0
                    && hx <= max_x_bound
                {
                    plot_ui.vline(
                        egui_plot::VLine::new("", hx)
                            .color(eframe::egui::Color32::from_rgba_unmultiplied(
                                180, 235, 255, 210,
                            ))
                            .style(egui_plot::LineStyle::dashed_dense()),
                    );
                }

                if plot_ui.response().hovered()
                    && let Some(coord) = plot_ui.pointer_coordinate()
                {
                    let clamped_x = coord.x.clamp(0.0, max_x_bound);
                    match x_axis_mode {
                        ScopeXAxisMode::TrackDistance => {
                            view.set_hover_distance_m(Some(clamped_x));
                            if let Some((_, ts, ds)) = &primary_distance_lookup
                                && let Some(utc_ms) = interpolate_ms_at_distance(ts, ds, clamped_x)
                            {
                                view.set_hover_utc_ms(Some(utc_ms));
                                if plot_ui.response().clicked() {
                                    view.request_setpoint_utc_ms(utc_ms);
                                }
                            }
                        }
                        ScopeXAxisMode::UtcTime | ScopeXAxisMode::ElapsedTime => {
                            let ref_start_ms = if is_comparison {
                                comparison_annotations
                                    .first()
                                    .map_or(start_ms, |a| a.start_utc_ms)
                            } else {
                                start_ms
                            };
                            let utc_ms = ref_start_ms + (clamped_x * 1000.0).round() as i64;
                            view.set_hover_utc_ms(Some(utc_ms));
                            if plot_ui.response().clicked() {
                                view.request_setpoint_utc_ms(utc_ms);
                            }
                        }
                    }
                }
            });

        egui_tiles::UiResponse::None
    }
}

struct PreparedTrace {
    name: String,
    unit_idx: usize,
    color: eframe::egui::Color32,
    style: egui_plot::LineStyle,
    mean_points: Vec<[f64; 2]>,
    envelope_bands: Vec<[[f64; 2]; 4]>,
}

fn series_window_stats(series: &daqcore::router::SignalSeries) -> Option<(f64, f64, f64)> {
    match series {
        daqcore::router::SignalSeries::Exact { values, .. } => {
            let mut min_v = f64::INFINITY;
            let mut max_v = f64::NEG_INFINITY;
            let mut sum_v = 0.0;
            let mut count = 0usize;
            for &v in values {
                if v.is_finite() {
                    min_v = min_v.min(v);
                    max_v = max_v.max(v);
                    sum_v += v;
                    count += 1;
                }
            }
            (count > 0).then(|| (min_v, max_v, sum_v / (count as f64)))
        }
        daqcore::router::SignalSeries::Envelope(buckets) => {
            let mut min_v = f64::INFINITY;
            let mut max_v = f64::NEG_INFINITY;
            let mut sum_v = 0.0;
            let mut count = 0usize;
            for b in buckets {
                if b.min.is_finite() && b.max.is_finite() && b.mean.is_finite() {
                    min_v = min_v.min(b.min);
                    max_v = max_v.max(b.max);
                    sum_v += b.mean;
                    count += 1;
                }
            }
            (count > 0).then(|| (min_v, max_v, sum_v / (count as f64)))
        }
    }
}

/// Convert a [`daqcore::router::SignalSeries`] into decimated mean line points AND convex
/// trapezoid `[min, max]` polygon quads for shaded envelope rendering.
pub fn series_to_points_and_bands(
    series: &daqcore::router::SignalSeries,
    start_ms: i64,
    max_gap_ms: i64,
    pixels: usize,
) -> (Vec<[f64; 2]>, Vec<[[f64; 2]; 4]>) {
    match series {
        daqcore::router::SignalSeries::Exact {
            timestamps_ms,
            values,
        } => {
            let pts = workspace_view::points_with_gap_breaks(
                timestamps_ms.iter().copied().zip(values.iter().copied()),
                start_ms,
                max_gap_ms,
            );
            (workspace_view::decimate(&pts, pixels), Vec::new())
        }
        daqcore::router::SignalSeries::Envelope(buckets) => {
            let mean_pts = workspace_view::points_with_gap_breaks(
                buckets.iter().map(|b| (b.utc_ms, b.mean)),
                start_ms,
                max_gap_ms,
            );
            let decimated_mean = workspace_view::decimate(&mean_pts, pixels);
            let quads = build_envelope_quads(buckets, start_ms, max_gap_ms, pixels);
            (decimated_mean, quads)
        }
    }
}

/// Build convex trapezoid quads `[[t0, min0], [t1, min1], [t1, max1], [t0, max0]]` from
/// overview `EnvelopePoint` buckets, breaking across unlogged gaps `> max_gap_ms`.
pub fn build_envelope_quads(
    buckets: &[daqcore::router::EnvelopePoint],
    start_ms: i64,
    max_gap_ms: i64,
    pixels: usize,
) -> Vec<[[f64; 2]; 4]> {
    if buckets.len() < 2 {
        return Vec::new();
    }
    let has_spread = buckets.iter().any(|b| (b.max - b.min).abs() > 1e-9);
    if !has_spread {
        return Vec::new();
    }

    let step = buckets.len().div_ceil(pixels.max(64)).max(1);
    let mut sampled: Vec<&daqcore::router::EnvelopePoint> =
        buckets.iter().step_by(step).collect();
    if let Some(last) = buckets.last()
        && sampled.last().is_none_or(|b| b.utc_ms != last.utc_ms)
    {
        sampled.push(last);
    }

    let mut quads = Vec::with_capacity(sampled.len().saturating_sub(1));
    for pair in sampled.windows(2) {
        let b0 = pair[0];
        let b1 = pair[1];
        if b1.utc_ms <= b0.utc_ms || b1.utc_ms - b0.utc_ms > max_gap_ms {
            continue;
        }
        if !b0.min.is_finite()
            || !b0.max.is_finite()
            || !b1.min.is_finite()
            || !b1.max.is_finite()
        {
            continue;
        }
        let t0 = ((b0.utc_ms - start_ms) as f64) / 1000.0;
        let t1 = ((b1.utc_ms - start_ms) as f64) / 1000.0;
        quads.push([
            [t0, b0.min],
            [t1, b1.min],
            [t1, b1.max],
            [t0, b0.max],
        ]);
    }
    quads
}

fn interpolate_distance_at_ms(
    timestamps_ms: &[i64],
    distances_m: &[f64],
    target_ms: i64,
) -> Option<f64> {
    if timestamps_ms.is_empty() || timestamps_ms.len() != distances_m.len() {
        return None;
    }
    let idx = timestamps_ms.partition_point(|&t| t <= target_ms);
    if idx == 0 {
        return distances_m.first().copied();
    }
    if idx >= timestamps_ms.len() {
        return distances_m.last().copied();
    }
    let t0 = timestamps_ms[idx - 1];
    let t1 = timestamps_ms[idx];
    let d0 = distances_m[idx - 1];
    let d1 = distances_m[idx];
    if t1 <= t0 {
        return Some(d1);
    }
    let alpha = ((target_ms - t0) as f64) / ((t1 - t0) as f64);
    Some(d0 + alpha.clamp(0.0, 1.0) * (d1 - d0))
}

fn interpolate_ms_at_distance(
    timestamps_ms: &[i64],
    distances_m: &[f64],
    target_d: f64,
) -> Option<i64> {
    if timestamps_ms.is_empty() || timestamps_ms.len() != distances_m.len() {
        return None;
    }
    let idx = distances_m.partition_point(|&d| d <= target_d);
    if idx == 0 {
        return timestamps_ms.first().copied();
    }
    if idx >= distances_m.len() {
        return timestamps_ms.last().copied();
    }
    let d0 = distances_m[idx - 1];
    let d1 = distances_m[idx];
    let t0 = timestamps_ms[idx - 1];
    let t1 = timestamps_ms[idx];
    if (d1 - d0).abs() < 1e-9 {
        return Some(t1);
    }
    let alpha = ((target_d - d0) / (d1 - d0)).clamp(0.0, 1.0);
    Some(t0 + (((t1 - t0) as f64) * alpha).round() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_axis_transform_roundtrips_physical_values() {
        let tf = UnitAxisTransform::from_ranges(200.0, 400.0, -50.0, 150.0);
        assert!((tf.phys_to_plot(-50.0) - 200.0).abs() < 1e-9);
        assert!((tf.phys_to_plot(150.0) - 400.0).abs() < 1e-9);
        assert!((tf.plot_to_phys(300.0) - 50.0).abs() < 1e-9);
    }

    #[test]
    fn build_envelope_quads_splits_across_gaps_and_preserves_convex_bounds() {
        let buckets = vec![
            daqcore::router::EnvelopePoint {
                utc_ms: 1_000,
                min: 10.0,
                max: 14.0,
                mean: 12.0,
            },
            daqcore::router::EnvelopePoint {
                utc_ms: 1_100,
                min: 11.0,
                max: 15.0,
                mean: 13.0,
            },
            // Large gap > 500 ms
            daqcore::router::EnvelopePoint {
                utc_ms: 5_000,
                min: 8.0,
                max: 12.0,
                mean: 10.0,
            },
            daqcore::router::EnvelopePoint {
                utc_ms: 5_100,
                min: 9.0,
                max: 13.0,
                mean: 11.0,
            },
        ];
        let quads = build_envelope_quads(&buckets, 1_000, 500, 800);
        assert_eq!(quads.len(), 2);
        assert_eq!(quads[0][0], [0.0, 10.0]);
        assert_eq!(quads[0][2], [0.1, 15.0]);
        assert_eq!(quads[1][0], [4.0, 8.0]);
        assert_eq!(quads[1][2], [4.1, 13.0]);
    }

    #[test]
    fn multi_channel_scope_renders_multi_unit_axes_and_distance_modes() {
        let t0 = daqcore::time::Time::from_unix_millis(1_794_500_000_000);
        let router = daqcore::router::VirtualTimelineRouter::live(t0, 30.0).expect("live router");
        let mut scope = Scope::new_multi(
            1,
            "Scope: Battery Overview",
            vec![
                (
                    "pack_stats.pack_voltage".to_owned(),
                    "Pack Voltage".to_owned(),
                    "V".to_owned(),
                ),
                (
                    "pack_stats.pack_current".to_owned(),
                    "Pack Current".to_owned(),
                    "A".to_owned(),
                ),
                (
                    "battery.power_kw".to_owned(),
                    "Pack Power".to_owned(),
                    "kW".to_owned(),
                ),
            ],
        );
        assert_eq!(scope.channels().len(), 3);

        let ctx = eframe::egui::Context::default();
        ui::theme::store_theme(&ctx, ui::theme::ThemeSelection::default().get_colors());

        for mode in [
            ScopeXAxisMode::UtcTime,
            ScopeXAxisMode::ElapsedTime,
            ScopeXAxisMode::TrackDistance,
        ] {
            scope.x_axis_mode = mode;
            let view = workspace_view::WorkspaceDataView::with_hover(
                &router,
                Some(t0.unix_millis() - 5_000),
                Some(50.0),
            );
            let raw_input = eframe::egui::RawInput {
                screen_rect: Some(eframe::egui::Rect::from_min_size(
                    eframe::egui::Pos2::ZERO,
                    eframe::egui::vec2(900.0, 500.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(raw_input, |ctx| {
                eframe::egui::CentralPanel::default().show(ctx, |ui| {
                    let _ = scope.show(ui, None, &view);
                });
            });
        }
    }
}
