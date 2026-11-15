use crate::{ui, workspace_view};
use std::fs;

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
                let idx = pos.saturating_sub(1).min(vals.len() - 1);
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
    first_raw_mean_points: Option<Vec<[f64; 2]>>,
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
            picker_target: None,
            search_query: String::new(),
            x_axis_mode: ScopeXAxisMode::UtcTime,
            speed_signal_id: "gps_speed_heading.gps_speed".to_owned(),
            show_inline_stats: false,
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
        let color = CHANNEL_PALETTE[self.channels.len() % CHANNEL_PALETTE.len()];
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

    pub fn remove_channel(&mut self, index: usize) {
        if index < self.channels.len() {
            self.channels.remove(index);
            for (i, ch) in self.channels.iter_mut().enumerate() {
                ch.color = CHANNEL_PALETTE[i % CHANNEL_PALETTE.len()];
            }
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
                        picked.signal_id,
                        picked.signal_name,
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

        if self.channels.is_empty() || self.picker_target.is_some() {
            let target = self.picker_target.unwrap_or(ScopePickerTarget::AddChannel);
            self.show_picker(ui, target, view);
            return egui_tiles::UiResponse::None;
        }

        // Resolve units for any channel whose unit was not populated yet.
        for ch in &mut self.channels {
            if ch.unit.is_empty() {
                ch.unit = view.resolve_signal_unit(&ch.signal_id);
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
        let playhead_s = view.timeline.setpoint().secs(start);

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
        };

        if self.cached_data.as_ref().is_none_or(|c| c.key != cache_key) {
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
                                name: format!("{} — {} ({:.0}m)", ch.label, ann.name, lap_dist_m),
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
                                max_gap_ms,
                                pixels,
                            );
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
                        channel_minmax_mean[ch_idx] = Some((s_min, s_max, s_mean));
                    }
                    let (pts, env_bands) =
                        series_to_points_and_bands(&series, start_ms, max_gap_ms, pixels);
                    channel_cursor_lookups.push(ChannelCursorLookup::Series(series));
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

            let first_raw_mean_points = raw_traces.first().map(|t| t.mean_points.clone());

            // Normalize unit bounds and build multi-unit Y transforms.
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
                first_raw_mean_points,
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

        // Single compact Nominal-style header strip:
        // Left: colored channel pills (`■ Label = 3633.1 W`, with `[min, max, avg]` in tooltip or optional inline toggle)
        // Right: `+` button (add channel) and `…` menu (X-axis mode, speed signal, inline stats toggle, reset Y, CSV export, clear)
        let mut remove_idx: Option<usize> = None;
        let mut open_picker: Option<ScopePickerTarget> = None;
        let mut clear_all = false;
        let mut export_csv = false;

        ui.horizontal_wrapped(|ui| {
            if is_comparison {
                ui.colored_label(
                    eframe::egui::Color32::from_rgb(255, 190, 60),
                    eframe::egui::RichText::new(format!("[Overlay: {}]", comparison_annotations.len()))
                        .small()
                        .strong(),
                );
            }

            for (idx, ch) in self.channels.iter().enumerate() {
                let unit_str = if ch.unit.is_empty() {
                    String::new()
                } else {
                    format!(" {}", ch.unit)
                };
                let kind_prefix = if ch.is_derived { "ƒ " } else { "" };
                let (pill_text, tooltip_text) = if let Some(st) = channel_stats[idx] {
                    let cur_str = st
                        .cursor
                        .map_or_else(|| "—".to_owned(), |v| format!("{v:.2}{unit_str}"));
                    let pill = if self.show_inline_stats {
                        format!(
                            "■ {}{} = {} [min {:.2}, max {:.2}, avg {:.2}]",
                            kind_prefix, ch.label, cur_str, st.min, st.max, st.mean
                        )
                    } else {
                        format!("■ {}{} = {}", kind_prefix, ch.label, cur_str)
                    };
                    let tip = format!(
                        "{}\nCursor: {}\nWindow Min: {:.3}{}\nWindow Max: {:.3}{}\nWindow Avg: {:.3}{}",
                        ch.signal_id, cur_str, st.min, unit_str, st.max, unit_str, st.mean, unit_str
                    );
                    (pill, tip)
                } else {
                    (
                        format!("■ {}{}", ch.label, unit_str),
                        format!("{} (no samples in active window)", ch.signal_id),
                    )
                };

                ui.colored_label(
                    ch.color,
                    eframe::egui::RichText::new(pill_text).size(11.5),
                )
                .on_hover_text(tooltip_text);
                if ui
                    .small_button("×")
                    .on_hover_text(format!("Remove {}", ch.signal_id))
                    .clicked()
                {
                    remove_idx = Some(idx);
                }
                ui.add_space(4.0);
            }

            if ui
                .small_button("+")
                .on_hover_text("Add another signal or derived formula to this Scope")
                .clicked()
            {
                open_picker = Some(ScopePickerTarget::AddChannel);
            }

            if self.x_axis_mode != ScopeXAxisMode::UtcTime
                && ui
                    .small_button(self.x_axis_mode.label())
                    .on_hover_text("Click to return to UTC Time X-axis")
                    .clicked()
            {
                self.x_axis_mode = ScopeXAxisMode::UtcTime;
            }

            ui.menu_button("…", |ui| {
                ui.set_min_width(190.0);
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
                        if mode == ScopeXAxisMode::TrackDistance && self.speed_signal_id.is_empty()
                        {
                            self.speed_signal_id = view.resolve_default_speed_signal();
                        }
                        ui.close();
                    }
                }
                if self.x_axis_mode == ScopeXAxisMode::TrackDistance {
                    let short_spd = self
                        .speed_signal_id
                        .rsplit('.')
                        .next()
                        .unwrap_or(&self.speed_signal_id);
                    if ui
                        .button(format!("Speed Signal: {short_spd}…"))
                        .on_hover_text(&self.speed_signal_id)
                        .clicked()
                    {
                        open_picker = Some(ScopePickerTarget::SpeedSignal);
                        ui.close();
                    }
                }
                ui.separator();
                ui.checkbox(
                    &mut self.show_inline_stats,
                    "Show [min, max, avg] inline",
                );
                if self.y_bounds_override.is_some() && ui.button("Reset Y-Axis Zoom").clicked() {
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
        });

        if export_csv
            && let Some(first_points) = self
                .cached_data
                .as_ref()
                .and_then(|c| c.first_raw_mean_points.as_ref())
            && let Some(path) = rfd::FileDialog::new()
                .set_file_name(format!("{}_data.csv", self.title.replace([' ', ':'], "_")))
                .add_filter("CSV", &["csv"])
                .save_file()
        {
            let mut text = String::from("X,Value\n");
            for p in first_points {
                text.push_str(&format!("{},{}\n", p[0], p[1]));
            }
            if let Err(e) = fs::write(path, text) {
                log::error!("Export failed: {e}");
            }
        }

        if let Some(idx) = remove_idx {
            self.remove_channel(idx);
            return egui_tiles::UiResponse::None;
        }
        if let Some(target) = open_picker {
            self.picker_target = Some(target);
            return egui_tiles::UiResponse::None;
        }
        if clear_all {
            self.channels.clear();
            self.cached_data = None;
            self.custom_title = false;
            self.refresh_title();
            self.picker_target = Some(ScopePickerTarget::AddChannel);
            return egui_tiles::UiResponse::None;
        }

        let Some(cached) = self.cached_data.as_ref() else {
            return egui_tiles::UiResponse::None;
        };

        if cached
            .mapped_traces
            .iter()
            .all(|t| t.mean_points.is_empty())
        {
            ui.label("No retained samples in the selected interval.");
        }

        let primary_min = cached.primary_min;
        let primary_max = cached.primary_max;
        let max_x_bound = cached.max_x_bound;
        let multi_unit = cached.unit_groups.len() > 1;
        let y_axes: Vec<egui_plot::AxisHints<'static>> = cached
            .unit_groups
            .iter()
            .enumerate()
            .map(|(u_idx, unit_name)| {
                let placement = if u_idx % 2 == 0 {
                    egui_plot::HPlacement::Left
                } else {
                    egui_plot::HPlacement::Right
                };
                let tf = cached.unit_transforms[u_idx];
                let mut hints = egui_plot::AxisHints::new_y()
                    .placement(placement)
                    .formatter(move |mark, _range| {
                        let phys = tf.plot_to_phys(mark.value);
                        let phys_step = (mark.step_size / tf.scale.abs().max(1e-9)).max(1e-6);
                        let decimals = (-phys_step.log10().floor() as isize).clamp(0, 3) as usize;
                        eframe::egui::emath::format_with_decimals_in_range(
                            phys,
                            decimals..=decimals,
                        )
                    });
                if multi_unit && !unit_name.is_empty() {
                    hints = hints.label(unit_name.clone());
                }
                hints
            })
            .collect();

        let x_axis_mode = self.x_axis_mode;

        // Compute playhead X and linked hover X in the current X-axis coordinate system.
        let playhead_x: Option<f64> =
            if is_comparison {
                None
            } else {
                match x_axis_mode {
                    ScopeXAxisMode::UtcTime | ScopeXAxisMode::ElapsedTime => Some(playhead_s),
                    ScopeXAxisMode::TrackDistance => cached
                        .primary_distance_lookup
                        .as_ref()
                        .and_then(|(_, ts, ds)| {
                            interpolate_distance_at_ms(ts, ds, view.timeline.setpoint_utc_ms())
                        }),
                }
            };

        let hover_x: Option<f64> = match x_axis_mode {
            ScopeXAxisMode::TrackDistance => view.hover_distance_m().or_else(|| {
                let hover_ms = view.hover_utc_ms()?;
                let (_, ts, ds) = cached.primary_distance_lookup.as_ref()?;
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
        let (active_y_min, active_y_max) =
            self.y_bounds_override.unwrap_or((primary_min, primary_max));
        let mut next_y_override = self.y_bounds_override;

        let mut plot = egui_plot::Plot::new(plot_id)
            .custom_y_axes(y_axes)
            .width(plot_width)
            .height(plot_height)
            .allow_zoom(false)
            .allow_drag(false)
            .allow_scroll(false)
            .allow_boxed_zoom(false)
            .allow_double_click_reset(false)
            .x_axis_formatter(move |mark, _| match (is_comparison, x_axis_mode) {
                (_, ScopeXAxisMode::TrackDistance) => format!("{:.0} m", mark.value),
                (true, _) | (false, ScopeXAxisMode::ElapsedTime) => format!("{:.1}s", mark.value),
                (false, ScopeXAxisMode::UtcTime) => {
                    let offset_ms = (mark.value * 1000.0).round() as i64;
                    start.offset(offset_ms).label()
                }
            });
        // Only show the in-plot legend box when comparing multiple laps/stints (to distinguish solid vs dashed laps);
        // in standard mode, the compact channel pills in the header strip already serve as the color-coded legend + live readout.
        if is_comparison {
            plot = plot.legend(egui_plot::Legend::default());
        }
        plot.show(ui, |plot_ui| {
            plot_ui.set_plot_bounds_x(0.0..=max_x_bound);
            plot_ui.set_plot_bounds_y(active_y_min..=active_y_max);

            for trace in &cached.mapped_traces {
                let fill_color = trace.color.gamma_multiply(0.22);
                let band_stroke =
                    eframe::egui::Stroke::new(0.5_f32, trace.color.gamma_multiply(0.35));

                for quad in &trace.envelope_bands {
                    plot_ui.polygon(
                        egui_plot::Polygon::new("", quad.to_vec())
                            .fill_color(fill_color)
                            .stroke(band_stroke)
                            .allow_hover(false),
                    );
                }

                plot_ui.line(
                    egui_plot::Line::new(
                        trace.name.clone(),
                        egui_plot::PlotPoints::from(trace.mean_points.clone()),
                    )
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

            let mut pointer_utc_ms: Option<i64> = None;
            let mut pointer_plot_y: Option<f64> = None;
            if plot_ui.response().hovered()
                && let Some(coord) = plot_ui.pointer_coordinate()
            {
                pointer_plot_y = Some(coord.y);
                let clamped_x = coord.x.clamp(0.0, max_x_bound);
                match x_axis_mode {
                    ScopeXAxisMode::TrackDistance => {
                        view.set_hover_distance_m(Some(clamped_x));
                        if let Some((_, ts, ds)) = &cached.primary_distance_lookup
                            && let Some(utc_ms) = interpolate_ms_at_distance(ts, ds, clamped_x)
                        {
                            view.set_hover_utc_ms(Some(utc_ms));
                            pointer_utc_ms = Some(utc_ms);
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
                        pointer_utc_ms = Some(utc_ms);
                        if plot_ui.response().clicked() {
                            view.request_setpoint_utc_ms(utc_ms);
                        }
                    }
                }
            }

            // Double-clicking on Scope resets ONLY the Y-axis range to fit the locally
            // selected window; it never changes the MasterTimeline X range.
            if plot_ui.response().double_clicked() {
                next_y_override = None;
                plot_ui.set_plot_bounds_y(primary_min..=primary_max);
            }

            if !is_comparison {
                let span_ms = (end_ms - start_ms).max(daqcore::timeline::MIN_WINDOW_MS) as f64;
                let anchor_ms = pointer_utc_ms.unwrap_or_else(|| view.timeline.setpoint_utc_ms());

                if plot_ui.response().hovered() {
                    let (scroll, zoom_delta, shift_held) = plot_ui
                        .ctx()
                        .input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.modifiers.shift));
                    if shift_held && scroll.y.abs() > 0.5 {
                        let factor = if scroll.y > 0.0 { 0.8 } else { 1.25 };
                        let cur_span = (active_y_max - active_y_min).max(1e-6);
                        let new_span = (cur_span * factor).clamp(1e-4, 1e9);
                        let anchor_y = pointer_plot_y
                            .unwrap_or((active_y_min + active_y_max) * 0.5)
                            .clamp(active_y_min, active_y_max);
                        let ratio = (anchor_y - active_y_min) / cur_span;
                        let new_lo = anchor_y - ratio * new_span;
                        next_y_override = Some((new_lo, new_lo + new_span));
                    } else if (zoom_delta - 1.0).abs() > 1e-3 {
                        let factor = (1.0 / f64::from(zoom_delta)).clamp(0.25, 4.0);
                        view.request_zoom_around_ms(anchor_ms, factor);
                    } else if scroll.y.abs() > 0.5 {
                        let factor = if scroll.y > 0.0 { 0.8 } else { 1.25 };
                        view.request_zoom_around_ms(anchor_ms, factor);
                    }
                    if scroll.x.abs() > 0.5 {
                        let delta_ms =
                            (-(scroll.x as f64) / (plot_width as f64) * span_ms).round() as i64;
                        view.request_pan_delta_ms(delta_ms);
                    }
                }

                let plot_rect = plot_ui.response().rect;
                let drag_dx = if plot_ui
                    .response()
                    .dragged_by(eframe::egui::PointerButton::Primary)
                {
                    plot_ui.response().drag_delta().x
                } else {
                    plot_ui.ctx().input(|i| {
                        if i.pointer.primary_down()
                            && i.pointer
                                .press_origin()
                                .is_some_and(|origin| plot_rect.contains(origin))
                        {
                            i.pointer.delta().x
                        } else {
                            0.0
                        }
                    })
                };
                if drag_dx.abs() > 0.0 {
                    let delta_ms =
                        (-(drag_dx as f64) / (plot_width as f64) * span_ms).round() as i64;
                    view.request_pan_delta_ms(delta_ms);
                }
            }
        });
        self.y_bounds_override = next_y_override;

        egui_tiles::UiResponse::None
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

    let target_quads = (pixels / 3).clamp(64, 240);
    let step = buckets.len().div_ceil(target_quads).max(1);
    let mut sampled: Vec<&daqcore::router::EnvelopePoint> = buckets.iter().step_by(step).collect();
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
        if !b0.min.is_finite() || !b0.max.is_finite() || !b1.min.is_finite() || !b1.max.is_finite()
        {
            continue;
        }
        if (b0.max - b0.min).abs() <= 1e-9 && (b1.max - b1.min).abs() <= 1e-9 {
            continue;
        }
        let t0 = ((b0.utc_ms - start_ms) as f64) / 1000.0;
        let t1 = ((b1.utc_ms - start_ms) as f64) / 1000.0;
        quads.push([[t0, b0.min], [t1, b1.min], [t1, b1.max], [t0, b0.max]]);
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
                    let _ = scope.show(ui, &view);
                });
            });
        }
    }
}
