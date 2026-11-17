use crate::{ui, workspace_view};

#[path = "scope_plot.rs"]
mod plot;

#[cfg(test)]
#[path = "scope_workflows.rs"]
mod workflows;

pub const CHANNEL_PALETTE: [eframe::egui::Color32; 8] = [
    eframe::egui::Color32::from_rgb(80, 190, 255), // Sky Blue
    eframe::egui::Color32::from_rgb(255, 160, 60), // Amber Orange
    eframe::egui::Color32::from_rgb(100, 230, 140), // Mint Green
    eframe::egui::Color32::from_rgb(235, 110, 245), // Orchid Purple
    eframe::egui::Color32::from_rgb(255, 95, 105), // Coral Red
    eframe::egui::Color32::from_rgb(70, 225, 220), // Teal Cyan
    eframe::egui::Color32::from_rgb(250, 220, 80), // Warm Yellow
    eframe::egui::Color32::from_rgb(165, 140, 255), // Periwinkle
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeXAxisMode {
    #[default]
    #[serde(alias = "utc", alias = "time")]
    UtcTime,
    #[serde(alias = "elapsed")]
    ElapsedTime,
    #[serde(alias = "distance")]
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
        let _ = self.key;
        &self.signal_id
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScopePickerTarget {
    AddChannel,
    SpeedSignal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ScopeCacheKey {
    start_ms: i64,
    end_ms: i64,
    pixels: usize,
    resolution: daqcore::router::Resolution,
    x_axis_mode: ScopeXAxisMode,
    speed_signal_id: String,
    comparison_ids: Vec<String>,
    channel_specs: Vec<(String, String, String, eframe::egui::Color32)>,
    data_revision: u64,
    configuration: String,
}

enum ChannelCursorLookup {
    Series(daqcore::router::SignalSeries),
    Distance(Vec<i64>, Vec<f64>),
    Fallback,
}

impl ChannelCursorLookup {
    fn value_at(&self, target_ms: i64) -> Option<f64> {
        match self {
            Self::Series(series) => series.latest_at_or_before(target_ms).map(|(_, v)| v),
            Self::Distance(ts, vals) => {
                if ts.is_empty() || vals.is_empty() {
                    return None;
                }
                let pos = ts.partition_point(|&t| t <= target_ms);
                if pos == 0 {
                    return None;
                }
                let idx = (pos - 1).min(vals.len() - 1);
                let v = vals[idx];
                v.is_finite().then_some(v)
            }
            Self::Fallback => None,
        }
    }
}

struct CachedScopeData {
    key: ScopeCacheKey,
    unit_groups: Vec<String>,
    unit_transforms: Vec<UnitAxisTransform>,
    primary_min: f64,
    primary_max: f64,
    max_x_bound: f64,
    primary_distance_lookup: Option<(i64, Vec<i64>, Vec<f64>)>,
    channel_minmax_mean: Vec<Option<(f64, f64, f64)>>,
    channel_cursor_lookups: Vec<ChannelCursorLookup>,
    mapped_traces: Vec<PreparedTrace>,
}

pub struct Scope {
    pub title: String,
    instance_num: usize,
    custom_title: bool,
    channels: Vec<ScopeChannel>,
    picker_target: Option<ScopePickerTarget>,
    search_query: String,
    pub x_axis_mode: ScopeXAxisMode,
    pub speed_signal_id: String,
    pub show_inline_stats: bool,
    pub show_legend: bool,
    pub config: ui::scope_settings::ScopeConfig,
    inspector: Option<(String, ui::scope_settings::InspectorTab)>,
    gesture: Option<ui::scope_interaction::Gesture>,
    measurements: ui::scope_interaction::Measurements,
    lane_y_overrides: std::collections::BTreeMap<usize, (f64, f64)>,
    plot_frames: Vec<eframe::egui::Rect>,
    hidden_channels: std::collections::HashSet<String>,
    y_bounds_override: Option<(f64, f64)>,
    cached_data: Option<CachedScopeData>,
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
            signal_id: signal_id.clone(),
            label: signal_id.clone(),
            unit: String::new(),
            color: CHANNEL_PALETTE[0],
            key: Some(key),
            is_derived: false,
        };
        Self {
            title: format!("Scope: {signal_id}"),
            instance_num,
            custom_title: false,
            channels: vec![channel],
            picker_target: None,
            search_query: String::new(),
            x_axis_mode: ScopeXAxisMode::UtcTime,
            speed_signal_id: "gps_speed_heading.gps_speed".to_owned(),
            show_inline_stats: false,
            show_legend: false,
            hidden_channels: std::collections::HashSet::new(),
            config: ui::scope_settings::ScopeConfig::default(),
            inspector: None,
            gesture: None,
            measurements: ui::scope_interaction::Measurements::default(),
            lane_y_overrides: std::collections::BTreeMap::new(),
            plot_frames: Vec::new(),
            y_bounds_override: None,
            cached_data: None,
        }
    }

    pub fn new_empty(instance_num: usize) -> Self {
        Self {
            title: format!("Scope #{instance_num}"),
            instance_num,
            custom_title: false,
            channels: Vec::new(),
            picker_target: Some(ScopePickerTarget::AddChannel),
            search_query: String::new(),
            x_axis_mode: ScopeXAxisMode::UtcTime,
            speed_signal_id: "gps_speed_heading.gps_speed".to_owned(),
            show_inline_stats: false,
            show_legend: false,
            hidden_channels: std::collections::HashSet::new(),
            config: ui::scope_settings::ScopeConfig::default(),
            inspector: None,
            gesture: None,
            measurements: ui::scope_interaction::Measurements::default(),
            lane_y_overrides: std::collections::BTreeMap::new(),
            plot_frames: Vec::new(),
            y_bounds_override: None,
            cached_data: None,
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
            picker_target: None,
            search_query: String::new(),
            x_axis_mode: ScopeXAxisMode::UtcTime,
            speed_signal_id: "gps_speed_heading.gps_speed".to_owned(),
            show_inline_stats: false,
            show_legend: false,
            hidden_channels: std::collections::HashSet::new(),
            config: ui::scope_settings::ScopeConfig::default(),
            inspector: None,
            gesture: None,
            measurements: ui::scope_interaction::Measurements::default(),
            lane_y_overrides: std::collections::BTreeMap::new(),
            plot_frames: Vec::new(),
            y_bounds_override: None,
            cached_data: None,
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
            picker_target: None,
            search_query: String::new(),
            x_axis_mode: ScopeXAxisMode::UtcTime,
            speed_signal_id: "gps_speed_heading.gps_speed".to_owned(),
            show_inline_stats: false,
            show_legend: false,
            hidden_channels: std::collections::HashSet::new(),
            config: ui::scope_settings::ScopeConfig::default(),
            inspector: None,
            gesture: None,
            measurements: ui::scope_interaction::Measurements::default(),
            lane_y_overrides: std::collections::BTreeMap::new(),
            plot_frames: Vec::new(),
            y_bounds_override: None,
            cached_data: None,
        }
    }

    pub fn new_configured(
        instance_num: usize,
        title: String,
        custom_title: bool,
        channels: Vec<ScopeChannel>,
        x_axis_mode: ScopeXAxisMode,
        speed_signal_id: String,
        y_bounds_override: Option<(f64, f64)>,
    ) -> Self {
        let mut s = Self {
            title,
            instance_num,
            custom_title,
            channels,
            picker_target: None,
            search_query: String::new(),
            x_axis_mode,
            speed_signal_id,
            show_inline_stats: false,
            show_legend: false,
            hidden_channels: std::collections::HashSet::new(),
            config: ui::scope_settings::ScopeConfig::default(),
            inspector: None,
            gesture: None,
            measurements: ui::scope_interaction::Measurements::default(),
            lane_y_overrides: std::collections::BTreeMap::new(),
            plot_frames: Vec::new(),
            y_bounds_override,
            cached_data: None,
        };
        if !custom_title {
            s.refresh_title();
        }
        s
    }

    pub fn channels(&self) -> &[ScopeChannel] {
        &self.channels
    }

    pub fn is_custom_title(&self) -> bool {
        self.custom_title
    }

    pub fn set_title(&mut self, new_title: String) {
        self.title = new_title;
        self.custom_title = !self.title.trim().is_empty();
    }

    pub fn reset_auto_title(&mut self) {
        self.custom_title = false;
        self.refresh_title();
    }

    pub fn y_bounds_override(&self) -> Option<(f64, f64)> {
        self.y_bounds_override
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
            self.picker_target = None;
            return;
        }
        let color = CHANNEL_PALETTE
            .iter()
            .copied()
            .find(|color| !self.channels.iter().any(|channel| channel.color == *color))
            .unwrap_or(CHANNEL_PALETTE[self.channels.len() % CHANNEL_PALETTE.len()]);
        self.channels.push(ScopeChannel {
            signal_id,
            label,
            unit,
            color,
            key,
            is_derived,
        });
        self.picker_target = None;
        self.refresh_title();
    }

    fn toggle_channel_visibility(&mut self, index: usize) {
        if let Some(channel) = self.channels.get(index)
            && !self.hidden_channels.remove(&channel.signal_id)
        {
            self.hidden_channels.insert(channel.signal_id.clone());
        }
    }

    pub fn remove_channel(&mut self, index: usize) {
        if index < self.channels.len() {
            let removed = self.channels.remove(index);
            self.hidden_channels.remove(&removed.signal_id);
            self.config.channels.remove(&removed.signal_id);
            if self.channels.is_empty() {
                self.custom_title = false;
                self.picker_target = Some(ScopePickerTarget::AddChannel);
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
                self.add_channel(signal_id.clone(), signal_id, unit, key, false);
            }
            ui::signal_browser::DraggedSignal::Derived {
                signal_id,
                label: _,
                unit,
            } => {
                self.add_channel(signal_id.clone(), signal_id, unit, None, true);
            }
        }
    }

    fn show_picker(
        &mut self,
        ui: &mut eframe::egui::Ui,
        target: ScopePickerTarget,
        view: &workspace_view::WorkspaceDataView<'_>,
    ) {
        let prompt = match target {
            ScopePickerTarget::AddChannel => "Select signal to plot:",
            ScopePickerTarget::SpeedSignal => "Select speed signal:",
        };
        let allow_cancel = !self.channels.is_empty();
        match ui::message_picker::show_signal_picker(
            ui,
            ("scope_signal_picker", self.instance_num),
            prompt,
            allow_cancel,
            false,
            &mut self.search_query,
            view,
        ) {
            ui::message_picker::SignalPickerAction::None => {}
            ui::message_picker::SignalPickerAction::Cancelled => {
                self.picker_target = None;
            }
            ui::message_picker::SignalPickerAction::Picked(picked) => match target {
                ScopePickerTarget::AddChannel => {
                    self.add_channel(
                        picked.signal_id.clone(),
                        picked.signal_id,
                        picked.unit,
                        picked.key,
                        picked.is_derived,
                    );
                }
                ScopePickerTarget::SpeedSignal => {
                    self.speed_signal_id = picked.signal_id;
                    self.picker_target = None;
                }
            },
        }
    }

    pub fn show(
        &mut self,
        ui: &mut eframe::egui::Ui,
        view: &workspace_view::WorkspaceDataView<'_>,
    ) -> egui_tiles::UiResponse {
        self.config.sanitize();
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
            if (!self.config.stacked || self.channels.is_empty())
                && ui.input(|i| i.pointer.any_released())
            {
                self.apply_dragged_signal((*payload).clone());
                let _ = eframe::egui::DragAndDrop::take_payload::<ui::signal_browser::DraggedSignal>(
                    ui.ctx(),
                );
            }
        }

        if self.channels.is_empty() || self.picker_target.is_some() {
            let target = self.picker_target.unwrap_or(ScopePickerTarget::AddChannel);
            self.show_picker(ui, target, view);
            return egui_tiles::UiResponse::None;
        }

        // Resolve engineering units directly from each channel's `signal_id`.
        for ch in &mut self.channels {
            let resolved = view.resolve_signal_unit(&ch.signal_id);
            if !resolved.is_empty() {
                ch.unit = resolved;
            }
        }

        let comparison_annotations: Vec<daqcore::storage::Annotation> =
            if view.timeline.is_comparison_mode() {
                let selected_ids = view.timeline.selected_annotation_ids();
                let all_anns = view
                    .router
                    .query_annotations_in_range(i64::MIN / 2..=i64::MAX / 2)
                    .unwrap_or_default();
                selected_ids
                    .iter()
                    .filter_map(|id| all_anns.iter().find(|ann| &ann.id == id).cloned())
                    .collect()
            } else {
                Vec::new()
            };
        let is_comparison = comparison_annotations.len() >= 2;
        ui.spacing_mut().item_spacing = eframe::egui::vec2(4.0, 2.0);

        let width_px = ui.available_width().max(1.0);
        let pixels = width_px as usize;
        let resolution = view.timeline.resolution_for_width(width_px);
        let base_staleness_ms = view.router.formula_engine().max_staleness_ms();
        let max_gap_ms = resolution
            .bucket_width_ms()
            .map_or(base_staleness_ms, |bw| base_staleness_ms.max(bw * 3));

        let start = view.timeline.start();
        let start_ms = start.unix_millis();
        let end_ms = view.timeline.end_utc_ms();
        let duration_s = view.timeline.end().secs(start).max(0.001);

        let comparison_ids: Vec<String> = comparison_annotations
            .iter()
            .map(|a| format!("{}:{}:{:?}", a.id, a.start_utc_ms, a.end_utc_ms))
            .collect();
        let channel_specs: Vec<(String, String, String, eframe::egui::Color32)> = self
            .channels
            .iter()
            .map(|c| {
                (
                    c.signal_id.clone(),
                    c.label.clone(),
                    c.unit.clone(),
                    c.color,
                )
            })
            .collect();
        let cache_key = ScopeCacheKey {
            start_ms,
            end_ms,
            pixels,
            resolution,
            x_axis_mode: self.x_axis_mode,
            speed_signal_id: self.speed_signal_id.clone(),
            comparison_ids,
            channel_specs,
            data_revision: view.data_revision(),
            configuration: serde_json::to_string(&self.config).unwrap_or_default(),
        };

        if self.cached_data.as_ref().is_none_or(|c| c.key != cache_key) {
            // Build unit groups in order of appearance so each unique unit gets its own Y-axis.
            let mut unit_groups: Vec<String> = Vec::new();
            let mut channel_unit_group: Vec<usize> = Vec::with_capacity(self.channels.len());
            for ch in &self.channels {
                let axis_key = self.config.axis_key(ch);
                let pos = unit_groups
                    .iter()
                    .position(|u| u == &axis_key)
                    .unwrap_or_else(|| {
                        unit_groups.push(axis_key.clone());
                        unit_groups.len() - 1
                    });
                channel_unit_group.push(pos);
            }

            // Query data and compute per-channel statistics & unit bounds.
            let mut channel_minmax_mean: Vec<Option<(f64, f64, f64)>> =
                vec![None; self.channels.len()];
            let mut channel_cursor_lookups: Vec<ChannelCursorLookup> =
                Vec::with_capacity(self.channels.len());
            let mut unit_bounds: Vec<(f64, f64)> =
                vec![(f64::INFINITY, f64::NEG_INFINITY); unit_groups.len()];

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
                        let trace_color = ch.color;

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
                            if primary_distance_lookup.is_none()
                                && !dist_series.distances_m.is_empty()
                            {
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
                                name: format!(
                                    "{} — {} ({:.0}m)",
                                    ch.signal_id, ann.name, lap_dist_m
                                ),
                                unit_idx,
                                color: trace_color,
                                style,
                                mean_points: decimated,
                                envelope_bands: Vec::new(),
                            });
                        } else {
                            let ann_dur_s =
                                ((ann_end_ms - ann_start_ms) as f64 / 1000.0).max(0.001);
                            max_x_bound = max_x_bound.max(ann_dur_s);
                            let ch_max_gap_ms =
                                channel_max_gap_ms(view.router, &ch.signal_id, max_gap_ms);
                            let series = view.query_signal_range(
                                &ch.signal_id,
                                ann_start_ms..=ann_end_ms,
                                ann_res,
                            );
                            if let Some((s_min, s_max, s_mean)) = series_window_stats(&series) {
                                ch_min = ch_min.min(s_min);
                                ch_max = ch_max.max(s_max);
                                ch_sum += s_mean;
                                ch_cnt += 1;
                            }
                            let (pts, env_bands) = series_to_points_and_bands(
                                &series,
                                ann_start_ms,
                                ch_max_gap_ms,
                                pixels,
                            );
                            raw_traces.push(PreparedTrace {
                                name: format!(
                                    "{} — {} ({:.1}s)",
                                    ch.signal_id, ann.name, ann_dur_s
                                ),
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
                        channel_minmax_mean[ch_idx] =
                            Some((ch_min, ch_max, ch_sum / (ch_cnt as f64)));
                    }
                    channel_cursor_lookups.push(ChannelCursorLookup::Fallback);
                }
            } else if self.x_axis_mode == ScopeXAxisMode::TrackDistance {
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
                        channel_minmax_mean[ch_idx] =
                            Some((ch_min, ch_max, ch_sum / (ch_cnt as f64)));
                    }
                    if primary_distance_lookup.is_none() && !dist_series.distances_m.is_empty() {
                        primary_distance_lookup = Some((
                            start_ms,
                            dist_series.timestamps_ms.clone(),
                            dist_series.distances_m.clone(),
                        ));
                    }
                    channel_cursor_lookups.push(ChannelCursorLookup::Distance(
                        dist_series.timestamps_ms.clone(),
                        dist_series.values.clone(),
                    ));
                    let pts: Vec<[f64; 2]> = dist_series
                        .distances_m
                        .into_iter()
                        .zip(dist_series.values)
                        .map(|(d, v)| [d, v])
                        .collect();
                    let decimated = workspace_view::decimate(&pts, pixels);
                    raw_traces.push(PreparedTrace {
                        name: ch.signal_id.clone(),
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
                    let ch_max_gap_ms = channel_max_gap_ms(view.router, &ch.signal_id, max_gap_ms);
                    let query_pad_ms = ch_max_gap_ms.max(3_000);
                    let padded_range =
                        start_ms.saturating_sub(query_pad_ms)..=end_ms.saturating_add(query_pad_ms);
                    let series = view.query_signal_range(&ch.signal_id, padded_range, resolution);
                    let in_window = series.slice_range(start_ms..=end_ms);
                    if let Some((s_min, s_max, s_mean)) =
                        series_window_stats(&in_window).or_else(|| series_window_stats(&series))
                    {
                        unit_bounds[unit_idx].0 = unit_bounds[unit_idx].0.min(s_min);
                        unit_bounds[unit_idx].1 = unit_bounds[unit_idx].1.max(s_max);
                        channel_minmax_mean[ch_idx] = Some((s_min, s_max, s_mean));
                    }
                    let (pts, env_bands) =
                        series_to_points_and_bands(&series, start_ms, ch_max_gap_ms, pixels);
                    channel_cursor_lookups.push(ChannelCursorLookup::Series(series));
                    raw_traces.push(PreparedTrace {
                        name: ch.signal_id.clone(),
                        unit_idx,
                        color: ch.color,
                        style: egui_plot::LineStyle::Solid,
                        mean_points: pts,
                        envelope_bands: env_bands,
                    });
                }
            }

            // Normalize unit bounds and build multi-unit Y transforms.
            let normalized_bounds: Vec<(f64, f64)> = unit_bounds
                .iter()
                .enumerate()
                .map(|(index, &(lo, hi))| {
                    if let Some(range) = self
                        .config
                        .axes
                        .get(&unit_groups[index])
                        .and_then(|axis| axis.valid_range())
                    {
                        range
                    } else if !lo.is_finite() || !hi.is_finite() {
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

            let (primary_min, primary_max) =
                normalized_bounds.first().copied().unwrap_or((-1.0, 1.0));
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

            let mapped_traces: Vec<PreparedTrace> = raw_traces
                .into_iter()
                .map(|trace| {
                    let tf = unit_transforms[trace.unit_idx];
                    let envelope_bands = trace
                        .envelope_bands
                        .into_iter()
                        .map(|quad| {
                            [
                                [quad[0][0], tf.phys_to_plot(quad[0][1])],
                                [quad[1][0], tf.phys_to_plot(quad[1][1])],
                                [quad[2][0], tf.phys_to_plot(quad[2][1])],
                                [quad[3][0], tf.phys_to_plot(quad[3][1])],
                            ]
                        })
                        .collect();
                    let mean_points = trace
                        .mean_points
                        .into_iter()
                        .map(|[x, y]| [x, tf.phys_to_plot(y)])
                        .collect();
                    PreparedTrace {
                        name: trace.name,
                        unit_idx: trace.unit_idx,
                        color: trace.color,
                        style: trace.style,
                        mean_points,
                        envelope_bands,
                    }
                })
                .collect();

            self.cached_data = Some(CachedScopeData {
                key: cache_key,
                unit_groups,
                unit_transforms,
                primary_min,
                primary_max,
                max_x_bound,
                primary_distance_lookup,
                channel_minmax_mean,
                channel_cursor_lookups,
                mapped_traces,
            });
        }

        // Compute fast cursor values from cached series (falling back to router only when needed).
        let cursor_ms = view.active_cursor_utc_ms();
        let mut channel_stats: Vec<Option<ChannelStats>> = vec![None; self.channels.len()];
        if let Some(cached) = &self.cached_data {
            for (idx, ch) in self.channels.iter().enumerate() {
                if let Some((min, max, mean)) =
                    cached.channel_minmax_mean.get(idx).copied().flatten()
                {
                    let cursor = cached
                        .channel_cursor_lookups
                        .get(idx)
                        .and_then(|lookup| lookup.value_at(cursor_ms))
                        .or_else(|| {
                            view.latest_signal_value_at_cursor(&ch.signal_id)
                                .map(|(_, v)| v)
                        });
                    channel_stats[idx] = Some(ChannelStats {
                        min,
                        max,
                        mean,
                        cursor,
                    });
                }
            }
        }

        // One small toolbar. Signal keys live inline in the toolbar by default (0% plot coverage),
        // or in the compact floating overlay when toggled on via the legend button.
        let entries = self.build_legend_entries(view, &comparison_annotations, &channel_stats);
        let mut open_picker: Option<ScopePickerTarget> = None;
        let mut clear_all = false;
        let mut export_csv = false;
        let mut toolbar_action: Option<ui::scope_legend::Action> = None;

        ui.horizontal(|ui| {
            if ui::scope_legend::toggle_button(ui, self.show_legend).clicked() {
                self.show_legend = !self.show_legend;
            }
            if ui::scope_icons::button(ui, true, self.config.stacked).clicked() {
                self.config.toggle_stack(&self.channels);
                self.cached_data = None;
                self.lane_y_overrides.clear();
                return;
            }
            if ui::scope_icons::button(ui, false, self.measurements.enabled).clicked() {
                self.measurements.enabled = !self.measurements.enabled;
                self.gesture = None;
            }
            if ui.small_button("+").on_hover_text("Add signal").clicked() {
                open_picker = Some(ScopePickerTarget::AddChannel);
            }
            if is_comparison {
                ui.label(
                    eframe::egui::RichText::new(format!(
                        "Comparing ({})",
                        comparison_annotations.len()
                    ))
                    .small(),
                );
            }
            if self.x_axis_mode != ScopeXAxisMode::UtcTime
                && ui
                    .small_button(self.x_axis_mode.label())
                    .on_hover_text("Use time axis")
                    .clicked()
            {
                self.x_axis_mode = ScopeXAxisMode::UtcTime;
            }

            eframe::egui::containers::menu::MenuButton::new("…")
                .config(
                    eframe::egui::containers::menu::MenuConfig::new()
                        .close_behavior(eframe::egui::PopupCloseBehavior::CloseOnClickOutside),
                )
                .ui(ui, |ui| {
                    ui.set_min_width(195.0);
                    ui.label(eframe::egui::RichText::new("Tab Title").weak().small());
                    ui.horizontal(|ui| {
                        let resp = ui.add(
                            eframe::egui::TextEdit::singleline(&mut self.title)
                                .desired_width(140.0)
                                .hint_text("Tab title…"),
                        );
                        if resp.changed() {
                            self.custom_title = !self.title.trim().is_empty();
                        }
                        if resp.lost_focus() {
                            if self.title.trim().is_empty() {
                                self.custom_title = false;
                                self.refresh_title();
                            }
                            if ui.input(|i| i.key_pressed(eframe::egui::Key::Enter)) {
                                ui.close();
                            }
                        }
                        if self.custom_title
                            && ui
                                .small_button("Auto")
                                .on_hover_text("Reset to automatic channel title")
                                .clicked()
                        {
                            self.custom_title = false;
                            self.refresh_title();
                        }
                    });
                    ui.separator();
                    ui.label(eframe::egui::RichText::new("X-Axis Mode").weak().small());
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
                            if mode == ScopeXAxisMode::TrackDistance
                                && self.speed_signal_id.is_empty()
                            {
                                self.speed_signal_id = view.resolve_default_speed_signal();
                            }
                            ui.close();
                        }
                    }
                    if self.x_axis_mode == ScopeXAxisMode::TrackDistance
                        && ui
                            .button(format!("Speed Signal: {}…", self.speed_signal_id))
                            .on_hover_text(&self.speed_signal_id)
                            .clicked()
                    {
                        open_picker = Some(ScopePickerTarget::SpeedSignal);
                        ui.close();
                    }
                    ui.separator();
                    ui.checkbox(&mut self.show_inline_stats, "Show statistics");
                    ui.menu_button("Plots", |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Count");
                            if ui
                                .add(
                                    eframe::egui::DragValue::new(&mut self.config.lanes)
                                        .range(1..=8),
                                )
                                .changed()
                            {
                                self.config.stacked = self.config.lanes > 1;
                                self.config.sanitize();
                                self.cached_data = None;
                            }
                        });
                        if ui.button("Split signals").clicked() {
                            self.config.stacked = true;
                            for (index, channel) in self.channels.iter().enumerate() {
                                self.config
                                    .channels
                                    .entry(channel.signal_id.clone())
                                    .or_default()
                                    .lane = index % self.config.lanes;
                            }
                            self.cached_data = None;
                            ui.close();
                        }
                        if ui.button("Combine signals").clicked() {
                            self.config.stacked = false;
                            self.cached_data = None;
                            ui.close();
                        }
                    });
                    ui.menu_button("Cursors", |ui| {
                        for (slot, label) in [(0, "Set A"), (1, "Set B")] {
                            if ui.button(label).clicked() {
                                self.measurements.enabled = true;
                                self.measurements.next = slot;
                                ui.close();
                            }
                        }
                        if ui.button("Clear cursors").clicked() {
                            self.measurements.pins = [None, None];
                            ui.close();
                        }
                    });
                    if self.y_bounds_override.is_some() && ui.button("Reset Y-Axis Zoom").clicked()
                    {
                        self.y_bounds_override = None;
                        ui.close();
                    }
                    if ui.button("Export CSV…").clicked() {
                        export_csv = true;
                        ui.close();
                    }
                    if self.channels.len() > 1 && ui.button("Clear All Channels").clicked() {
                        clear_all = true;
                        ui.close();
                    }
                });

            if !self.show_legend {
                toolbar_action = ui::scope_legend::show_toolbar_strip(
                    ui,
                    self.instance_num,
                    &entries,
                    self.show_inline_stats,
                );
            }
        });

        if let Some(action) = toolbar_action {
            self.apply_legend_action(action);
        }

        if export_csv {
            view.request_open_export_modal();
        }

        if let Some(target) = open_picker {
            self.picker_target = Some(target);
            return egui_tiles::UiResponse::None;
        }
        if clear_all {
            self.channels.clear();
            self.hidden_channels.clear();
            self.config.channels.clear();
            self.config.axes.clear();
            self.cached_data = None;
            self.custom_title = false;
            self.refresh_title();
            self.picker_target = Some(ScopePickerTarget::AddChannel);
            return egui_tiles::UiResponse::None;
        }

        self.render_plots(ui, view, &comparison_annotations, &entries)
    }
}

#[derive(Clone)]
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

/// Compute the gap-break threshold in milliseconds for `signal_id`, scaling up from
/// `base_max_gap_ms` for low-rate signals (e.g. 2 Hz -> `nominal_period_ms = 500` -> `1500 ms`)
/// so zooming into `Overview10Hz` or `FullRes` does not insert `NaN` breaks between every sample.
fn channel_max_gap_ms(
    router: &daqcore::router::VirtualTimelineRouter,
    signal_id: &str,
    base_max_gap_ms: i64,
) -> i64 {
    let clean_id = signal_id
        .trim()
        .strip_prefix('-')
        .unwrap_or(signal_id)
        .trim();
    let mut max_period_ms = router
        .signal_meta(clean_id)
        .and_then(|m| m.nominal_period_ms)
        .map(i64::from)
        .unwrap_or(0);

    if let Some(compiled) = router.formula_engine().get(clean_id) {
        for dep in &compiled.dependencies {
            if let Some(period) = router
                .signal_meta(dep)
                .and_then(|m| m.nominal_period_ms)
                .map(i64::from)
            {
                max_period_ms = max_period_ms.max(period);
            }
        }
    }

    base_max_gap_ms.max(max_period_ms.saturating_mul(3))
}

/// Infer a minimum gap-break threshold from the actual sample/bucket cadence of `series`
/// (up to 64 consecutive pairs with `0 < dt <= 5_000 ms`).
///
/// This ensures that:
/// 1. When `window <= 900 s` requests `Overview10Hz` or `FullRes` (`max_gap_ms = 500 ms`),
///    any partition that returns an `Overview1Hz` (`1,000 ms` bucket) fallback (e.g. while
///    offline or while `overview_10hz.parquet` / `full_res.parquet` is downloading) uses
///    `effective_max_gap_ms >= 3,000 ms` instead of breaking every 1-second bucket with `NaN`.
/// 2. Signals from older DBCs where `nominal_period_ms` is `None` (or logged at 1–5 Hz)
///    also adapt automatically to their observed cadence.
fn infer_series_gap_threshold_ms(
    series: &daqcore::router::SignalSeries,
    base_max_gap_ms: i64,
) -> i64 {
    let mut dts: Vec<i64> = Vec::with_capacity(64);
    match series {
        daqcore::router::SignalSeries::Exact { timestamps_ms, .. } => {
            for pair in timestamps_ms.windows(2) {
                let dt = pair[1] - pair[0];
                if dt > 0 && dt <= 5_000 {
                    dts.push(dt);
                    if dts.len() >= 64 {
                        break;
                    }
                }
            }
        }
        daqcore::router::SignalSeries::Envelope(buckets) => {
            for pair in buckets.windows(2) {
                let dt = pair[1].utc_ms - pair[0].utc_ms;
                if dt > 0 && dt <= 5_000 {
                    dts.push(dt);
                    if dts.len() >= 64 {
                        break;
                    }
                }
            }
        }
    }
    if dts.is_empty() {
        return base_max_gap_ms;
    }
    dts.sort_unstable();
    let median_dt = dts[dts.len() / 2];
    base_max_gap_ms.max(median_dt.saturating_mul(3))
}

/// Convert a [`daqcore::router::SignalSeries`] into decimated mean line points AND convex
/// trapezoid `[min, max]` polygon quads for shaded envelope rendering.
pub fn series_to_points_and_bands(
    series: &daqcore::router::SignalSeries,
    start_ms: i64,
    max_gap_ms: i64,
    pixels: usize,
) -> (Vec<[f64; 2]>, Vec<[[f64; 2]; 4]>) {
    let effective_gap_ms = infer_series_gap_threshold_ms(series, max_gap_ms);
    match series {
        daqcore::router::SignalSeries::Exact {
            timestamps_ms,
            values,
        } => {
            let pts = workspace_view::points_with_gap_breaks(
                timestamps_ms.iter().copied().zip(values.iter().copied()),
                start_ms,
                effective_gap_ms,
            );
            (workspace_view::decimate(&pts, pixels), Vec::new())
        }
        daqcore::router::SignalSeries::Envelope(buckets) => {
            let mean_pts = workspace_view::points_with_gap_breaks(
                buckets.iter().map(|b| (b.utc_ms, b.mean)),
                start_ms,
                effective_gap_ms,
            );
            let decimated_mean = workspace_view::decimate(&mean_pts, pixels);
            let quads = build_envelope_quads(buckets, start_ms, effective_gap_ms, pixels);
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

    let target_quads = (pixels / 3).clamp(64, 240);
    let step = buckets.len().div_ceil(target_quads).max(1);

    let mut quads = Vec::with_capacity(target_quads.min(buckets.len().saturating_sub(1)));
    let mut run_start = 0usize;
    while run_start < buckets.len() {
        if !buckets[run_start].min.is_finite() || !buckets[run_start].max.is_finite() {
            run_start += 1;
            continue;
        }
        let mut run_end = run_start + 1;
        while run_end < buckets.len() {
            let prev = &buckets[run_end - 1];
            let curr = &buckets[run_end];
            if !curr.min.is_finite()
                || !curr.max.is_finite()
                || curr.utc_ms <= prev.utc_ms
                || curr.utc_ms - prev.utc_ms > max_gap_ms
            {
                break;
            }
            run_end += 1;
        }

        let run = &buckets[run_start..run_end];
        if run.len() >= 2 {
            let mut aggregated: Vec<(i64, f64, f64)> =
                Vec::with_capacity(run.len().div_ceil(step) + 1);
            for chunk in run.chunks(step) {
                let mut c_min = f64::INFINITY;
                let mut c_max = f64::NEG_INFINITY;
                for b in chunk {
                    c_min = c_min.min(b.min);
                    c_max = c_max.max(b.max);
                }
                aggregated.push((chunk[0].utc_ms, c_min, c_max));
            }
            if let Some(last) = run.last()
                && aggregated
                    .last()
                    .is_none_or(|&(ts, _, _)| ts != last.utc_ms)
            {
                aggregated.push((last.utc_ms, last.min, last.max));
            }

            for pair in aggregated.windows(2) {
                let (ts0, min0, max0) = pair[0];
                let (ts1, min1, max1) = pair[1];
                if ts1 <= ts0 {
                    continue;
                }
                if (max0 - min0).abs() <= 1e-9 && (max1 - min1).abs() <= 1e-9 {
                    continue;
                }
                let t0 = ((ts0 - start_ms) as f64) / 1000.0;
                let t1 = ((ts1 - start_ms) as f64) / 1000.0;
                quads.push([[t0, min0], [t1, min1], [t1, max1], [t0, max0]]);
            }
        }

        run_start = run_end;
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
    fn floating_legend_toggle_and_interactions_stay_out_of_plot_navigation() {
        use eframe::egui::{self, Event, Modifiers, PointerButton};

        fn frame(
            ctx: &egui::Context,
            scope: &mut Scope,
            router: &daqcore::router::VirtualTimelineRouter,
            events: Vec<Event>,
        ) -> egui::FullOutput {
            let view = workspace_view::WorkspaceDataView::new(router);
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(900.0, 500.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let _ = scope.show(ui, &view);
                    });
                },
            );
            assert_eq!(
                view.requested_setpoint_utc_ms(),
                None,
                "Legend clicks cannot scrub"
            );
            assert_eq!(
                view.requested_pan_delta_ms(),
                None,
                "Legend drags cannot pan"
            );
            assert_eq!(view.requested_zoom(), None, "Legend scrolling cannot zoom");
            output
        }
        fn find(output: &egui::FullOutput, label: &str) -> Option<egui::Pos2> {
            output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == label => {
                    let center = text.pos + text.galley.size() * 0.5;
                    shape.clip_rect.contains(center).then_some(center)
                }
                _ => None,
            })
        }
        fn click(
            ctx: &egui::Context,
            scope: &mut Scope,
            router: &daqcore::router::VirtualTimelineRouter,
            pos: egui::Pos2,
        ) -> egui::FullOutput {
            frame(ctx, scope, router, vec![Event::PointerMoved(pos)]);
            for pressed in [true, false] {
                frame(
                    ctx,
                    scope,
                    router,
                    vec![Event::PointerButton {
                        pos,
                        button: PointerButton::Primary,
                        pressed,
                        modifiers: Modifiers::NONE,
                    }],
                );
            }
            frame(ctx, scope, router, vec![])
        }

        let ctx = egui::Context::default();
        ui::theme::store_theme(&ctx, ui::theme::ThemeSelection::default().get_colors());
        let router = daqcore::router::VirtualTimelineRouter::live(
            daqcore::time::Time::from_unix_millis(100_000),
            30.0,
        )
        .unwrap();
        let mut scope = Scope::new_multi(
            1,
            "Legend test",
            (0..20)
                .map(|index| {
                    (
                        format!("message.signal_{index}"),
                        format!("Channel {index}"),
                        "V".to_owned(),
                    )
                })
                .collect(),
        );
        assert!(
            !scope.show_legend,
            "Floating legend is off by default so keys live in the top toolbar strip"
        );
        frame(&ctx, &mut scope, &router, vec![]);
        let toggle = egui::pos2(20.0, 17.0);
        // Click toggle to open the floating legend overlay
        let output = click(&ctx, &mut scope, &router, toggle);
        assert!(scope.show_legend);
        assert!(find(&output, "Legend").is_none(), "The toggle is icon only");
        let entry = find(&output, "message.signal_0").expect("Floating legend is visible");
        assert!(
            entry.x < 450.0 && entry.y > 250.0,
            "Floating legend belongs at the bottom left"
        );
        let panel = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Rect(rect)
                    if rect.rect.width() >= 140.0
                        && rect.rect.width() <= 300.0
                        && rect.rect.height() <= 150.0 =>
                {
                    Some(rect.rect)
                }
                _ => None,
            })
            .expect("Legend panel");
        let eye = egui::pos2(panel.right() - 15.0, entry.y);
        let key = scope.cached_data.as_ref().unwrap().key.clone();
        let colors: Vec<_> = scope.channels.iter().map(|channel| channel.color).collect();
        click(&ctx, &mut scope, &router, eye);
        assert!(scope.hidden_channels.contains("message.signal_0"));
        assert_eq!(
            scope.channels.len(),
            20,
            "Hiding does not remove the signal"
        );
        assert!(
            !egui::Popup::is_any_open(&ctx),
            "The eye does not open channel details"
        );
        click(&ctx, &mut scope, &router, eye);
        assert!(!scope.hidden_channels.contains("message.signal_0"));
        assert_eq!(scope.cached_data.as_ref().unwrap().key, key);
        assert_eq!(
            scope
                .channels
                .iter()
                .map(|channel| channel.color)
                .collect::<Vec<_>>(),
            colors
        );
        assert!(
            find(&output, "message.signal_19").is_none(),
            "Long legends scroll instead of growing"
        );
        frame(
            &ctx,
            &mut scope,
            &router,
            vec![
                Event::PointerMoved(entry),
                Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, -20.0),
                    modifiers: Modifiers::NONE,
                },
            ],
        );
        frame(
            &ctx,
            &mut scope,
            &router,
            vec![Event::PointerButton {
                pos: entry,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            }],
        );
        let dragged = entry - egui::vec2(60.0, 0.0);
        frame(
            &ctx,
            &mut scope,
            &router,
            vec![Event::PointerMoved(dragged)],
        );
        frame(
            &ctx,
            &mut scope,
            &router,
            vec![Event::PointerButton {
                pos: dragged,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            }],
        );
        // Toggling the overlay switches back to the inline toolbar strip and back to floating without losing scroll position.
        let _output = click(&ctx, &mut scope, &router, toggle);
        assert!(!scope.show_legend);
        let output = click(&ctx, &mut scope, &router, toggle);
        assert!(scope.show_legend);
        let entry = find(&output, "message.signal_0")
            .or_else(|| find(&output, "message.signal_1"))
            .expect("Visible legend row");
        let output = click(&ctx, &mut scope, &router, entry);
        let remove = find(&output, "Remove signal").expect("Channel details popup");
        click(&ctx, &mut scope, &router, remove);
        assert_eq!(scope.channels.len(), 19);
    }

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
                    let _ = scope.show(ui, &view);
                });
            });
        }
    }

    #[test]
    fn low_rate_2hz_signal_does_not_break_when_zoomed_in() {
        use daqcore::frame::{FrameKind, ParsedFrame};
        use daqcore::superdbc::database::Database;

        let db_json = r#"{
          "content_hash": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
          "versions": { "schema_version": "1.1", "hash": "test_2hz" },
          "buses": {
            "VCAN": {
              "bus_id": 0,
              "baud_rate": 500000,
              "nodes": [],
              "messages": [
                {
                  "id": 513,
                  "is_extended_id": false,
                  "message_name": "INVA_INFO",
                  "transmitter": "INVERTER_A",
                  "receivers": ["MAIN_MODULE"],
                  "length_bytes": 4,
                  "nominal_period_ms": 500,
                  "priority": 2,
                  "description": "",
                  "signals": [
                    {
                      "signal_name": "AMK_DCBusVoltage",
                      "description": "",
                      "data_type": "unsigned",
                      "raw_type": "unsigned",
                      "start_bit": 0,
                      "bit_length": 16,
                      "byte_order": "little_endian",
                      "scale": 0.1,
                      "offset": 0.0,
                      "limits": null,
                      "unit": "V",
                      "choices": null,
                      "display_format": null
                    }
                  ]
                }
              ]
            }
          }
        }"#;
        let db = Database::from_json(db_json).expect("parse test db");

        let base_ms = 1_788_719_789_000;
        let mut router = daqcore::router::VirtualTimelineRouter::live(
            daqcore::time::Time::from_unix_millis(base_ms + 5_000),
            5.0,
        )
        .expect("live router");

        // Push a 2 Hz signal (500 ms nominal period) for 10 seconds
        for i in 0..=20 {
            let ts = base_ms + i * 500;
            let payload = db
                .encode(0, 513, &[("AMK_DCBusVoltage", 520.0 + (i as f64) * 0.5)])
                .expect("encode frame");
            let decoded = db.decode(0, 513, &payload).ok();
            router.ingest_frame(ParsedFrame {
                timestamp: daqcore::time::Time::from_unix_millis(ts),
                bus_id: 0,
                kind: FrameKind::Data,
                dlc: payload.len() as u8,
                identity: 513,
                raw_bytes: payload,
                decoded,
            });
        }

        let base_staleness = router.formula_engine().max_staleness_ms();
        let ch_gap = channel_max_gap_ms(&router, "INVA_INFO.AMK_DCBusVoltage", base_staleness);
        assert_eq!(ch_gap, 1_500);

        let full_res = router.query_signal(
            "INVA_INFO.AMK_DCBusVoltage",
            base_ms..=(base_ms + 5_000),
            daqcore::router::Resolution::FullRes,
        );
        let (pts, _) = series_to_points_and_bands(&full_res, base_ms, ch_gap, 800);
        // Must have NO NaN gap breaks between consecutive 500 ms samples!
        assert!(pts.len() >= 10);
        assert!(
            pts.iter().all(|p| p[0].is_finite() && p[1].is_finite()),
            "2 Hz signal must not have NaN breaks at FullRes: {pts:?}"
        );

        // Verify that an Overview1Hz (1,000 ms bucket) fallback series passed with a
        // `< 900s` window `max_gap_ms = 500 ms` ALSO adapts and does not break into NaNs
        let fallback_1hz = daqcore::router::SignalSeries::Envelope(
            (0..10)
                .map(|i| daqcore::router::EnvelopePoint {
                    utc_ms: base_ms + i * 1_000,
                    min: 500.0,
                    max: 540.0,
                    mean: 520.0 + i as f64,
                })
                .collect(),
        );
        let (fb_pts, fb_quads) = series_to_points_and_bands(&fallback_1hz, base_ms, 500, 800);
        assert_eq!(fb_pts.len(), 10);
        assert!(
            fb_pts.iter().all(|p| p[0].is_finite() && p[1].is_finite()),
            "1 Hz fallback envelope must not have NaN breaks when window < 900s: {fb_pts:?}"
        );
        assert_eq!(fb_quads.len(), 9);
    }
}
