use crate::{ui, workspace_view};
use std::f64::consts::TAU;

const COMET_TRAIL_MS: i64 = 2_500;
const GRADIENT_BINS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScatterAxisSlot {
    X,
    Y,
    ZColor,
}

impl ScatterAxisSlot {
    pub fn label(self) -> &'static str {
        match self {
            Self::X => "X Axis",
            Self::Y => "Y Axis",
            Self::ZColor => "Color (Z)",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::X => Self::Y,
            Self::Y => Self::ZColor,
            Self::ZColor => Self::X,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ScatterCacheKey {
    start_ms: i64,
    end_ms: i64,
    resolution: daqcore::router::Resolution,
    staleness_ms: i64,
    x_signal: String,
    y_signal: String,
    z_color_signal: Option<String>,
    data_revision: u64,
}

struct CachedScatterData {
    key: ScatterCacheKey,
    x_unit: String,
    y_unit: String,
    z_unit: String,
    timestamps_ms: Vec<i64>,
    x_vals: Vec<f64>,
    y_vals: Vec<f64>,
    z_vals: Option<Vec<f64>>,
    x_bounds: (f64, f64),
    y_bounds: (f64, f64),
    z_range: Option<(f64, f64)>,
    color_bins: Vec<(eframe::egui::Color32, Vec<[f64; 2]>)>,
}

pub struct XYScatter {
    pub title: String,
    instance_num: usize,
    custom_title: bool,
    pub x_signal: String,
    pub y_signal: String,
    pub z_color_signal: Option<String>,
    pub equal_aspect: bool,
    pub show_g_rings: bool,
    bounds_override: Option<([f64; 2], [f64; 2])>,
    active_drop_slot: ScatterAxisSlot,
    picker_slot: Option<ScatterAxisSlot>,
    search_query: String,
    ring_points: Vec<(String, Vec<[f64; 2]>)>,
    cached_data: Option<CachedScatterData>,
}

impl XYScatter {
    pub fn new(instance_num: usize) -> Self {
        Self::preset_gg_diagram(instance_num)
    }

    pub fn new_configured(
        instance_num: usize,
        title: impl Into<String>,
        x_signal: impl Into<String>,
        y_signal: impl Into<String>,
        z_color_signal: Option<String>,
        equal_aspect: bool,
        show_g_rings: bool,
    ) -> Self {
        Self {
            title: title.into(),
            instance_num,
            custom_title: true,
            x_signal: x_signal.into(),
            y_signal: y_signal.into(),
            z_color_signal,
            equal_aspect,
            show_g_rings,
            bounds_override: None,
            active_drop_slot: ScatterAxisSlot::X,
            picker_slot: None,
            search_query: String::new(),
            ring_points: build_g_rings(),
            cached_data: None,
        }
    }

    pub fn preset_gg_diagram(instance_num: usize) -> Self {
        Self::new_configured(
            instance_num,
            format!("XY Scatter: G-G Diagram #{instance_num}"),
            "-IMU_acceleration.Y_axis",
            "IMU_acceleration.X_axis",
            Some("gps_speed_heading.gps_speed".to_owned()),
            true,
            true,
        )
    }

    pub fn preset_understeer(instance_num: usize) -> Self {
        Self::new_configured(
            instance_num,
            format!("XY Scatter: Handling Understeer #{instance_num}"),
            "IMU_acceleration.Y_axis",
            "IMU_angular_rate.Z_axis",
            Some("gps_speed_heading.gps_speed".to_owned()),
            false,
            false,
        )
    }

    pub fn preset_battery_scatter(instance_num: usize) -> Self {
        Self::new_configured(
            instance_num,
            format!("XY Scatter: Pack Temp vs Voltage #{instance_num}"),
            "pack_stats.max_temp",
            "pack_stats.pack_voltage",
            Some("battery.power_kw".to_owned()),
            false,
            false,
        )
    }

    pub fn is_custom_title(&self) -> bool {
        self.custom_title
    }

    pub fn set_custom_title(&mut self, custom_title: bool) {
        self.custom_title = custom_title;
    }

    pub fn set_title(&mut self, new_title: String) {
        self.title = new_title;
        self.custom_title = !self.title.trim().is_empty();
    }

    pub fn reset_auto_title(&mut self) {
        self.custom_title = false;
        self.refresh_title();
    }

    pub fn set_axis_signal(&mut self, slot: ScatterAxisSlot, signal_id: String) {
        match slot {
            ScatterAxisSlot::X => self.x_signal = signal_id,
            ScatterAxisSlot::Y => self.y_signal = signal_id,
            ScatterAxisSlot::ZColor => self.z_color_signal = Some(signal_id),
        }
        if matches!(slot, ScatterAxisSlot::X | ScatterAxisSlot::Y) {
            let both_imu_accel =
                is_imu_accel_signal(&self.x_signal) && is_imu_accel_signal(&self.y_signal);
            self.equal_aspect = both_imu_accel;
            self.show_g_rings = both_imu_accel;
            self.bounds_override = None;
        }
        self.custom_title = false;
        self.refresh_title();
    }

    fn refresh_title(&mut self) {
        if self.custom_title {
            return;
        }
        self.title = format!("XY: {} vs {}", self.x_signal, self.y_signal);
    }

    pub fn apply_dragged_signal(&mut self, dragged: ui::signal_browser::DraggedSignal) {
        let sig_id = dragged.signal_id();
        let target_slot = self.picker_slot.unwrap_or(self.active_drop_slot);
        self.set_axis_signal(target_slot, sig_id);
        self.picker_slot = None;
        self.active_drop_slot = target_slot.next();
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

        ui.spacing_mut().item_spacing = eframe::egui::vec2(4.0, 2.0);

        let width_px = ui.available_width().max(60.0);
        let resolution = view.timeline.resolution_for_width(width_px);
        let staleness_ms = view.router.formula_engine().max_staleness_ms();
        let range_ms = view.timeline.range_ms();
        let cache_key = ScatterCacheKey {
            start_ms: *range_ms.start(),
            end_ms: *range_ms.end(),
            resolution,
            staleness_ms,
            x_signal: self.x_signal.clone(),
            y_signal: self.y_signal.clone(),
            z_color_signal: self.z_color_signal.clone(),
            data_revision: view.data_revision(),
        };

        if self.cached_data.as_ref().is_none_or(|c| c.key != cache_key) {
            let x_series = view.query_signal(&self.x_signal, resolution);
            let y_series = view.query_signal(&self.y_signal, resolution);

            let x_unit = view.resolve_signal_unit(&self.x_signal);
            let y_unit = view.resolve_signal_unit(&self.y_signal);
            let z_unit = self
                .z_color_signal
                .as_deref()
                .map(|z| view.resolve_signal_unit(z))
                .unwrap_or_default();

            let mut timestamps_ms: Vec<i64>;
            let mut x_vals: Vec<f64>;
            let mut y_vals: Vec<f64>;
            let mut z_vals: Option<Vec<f64>> = None;

            if let Some(z_sig) = &self.z_color_signal {
                let z_series = view.query_signal(z_sig, resolution);
                let aligned_xyz = daqcore::analysis::align_xyz_triples(
                    &x_series,
                    &y_series,
                    &z_series,
                    staleness_ms,
                );
                if !aligned_xyz.timestamps_ms.is_empty() {
                    timestamps_ms = aligned_xyz.timestamps_ms;
                    x_vals = aligned_xyz.x_values;
                    y_vals = aligned_xyz.y_values;
                    z_vals = Some(aligned_xyz.z_values);
                } else {
                    let aligned_xy =
                        daqcore::analysis::align_xy_pairs(&x_series, &y_series, staleness_ms);
                    timestamps_ms = aligned_xy.timestamps_ms;
                    x_vals = aligned_xy.x_values;
                    y_vals = aligned_xy.y_values;
                }
            } else {
                let aligned_xy =
                    daqcore::analysis::align_xy_pairs(&x_series, &y_series, staleness_ms);
                timestamps_ms = aligned_xy.timestamps_ms;
                x_vals = aligned_xy.x_values;
                y_vals = aligned_xy.y_values;
            }

            // Decimate if sample count is very large so interactive scatter stays 120fps
            let max_scatter_pts = 4_000usize;
            if timestamps_ms.len() > max_scatter_pts {
                let step = timestamps_ms.len().div_ceil(max_scatter_pts).max(1);
                timestamps_ms = timestamps_ms.into_iter().step_by(step).collect();
                x_vals = x_vals.into_iter().step_by(step).collect();
                y_vals = y_vals.into_iter().step_by(step).collect();
                if let Some(zv) = z_vals {
                    z_vals = Some(zv.into_iter().step_by(step).collect());
                }
            }

            let x_bounds = compute_axis_bounds(&x_vals);
            let y_bounds = compute_axis_bounds(&y_vals);

            let z_range = z_vals.as_ref().and_then(|zv| {
                let mut lo = f64::INFINITY;
                let mut hi = f64::NEG_INFINITY;
                for &v in zv {
                    if v.is_finite() {
                        lo = lo.min(v);
                        hi = hi.max(v);
                    }
                }
                (lo.is_finite() && hi.is_finite()).then_some((lo, hi))
            });

            let mut color_bins: Vec<(eframe::egui::Color32, Vec<[f64; 2]>)> = Vec::new();
            if let (Some(zv), Some((z_min, z_max))) = (&z_vals, z_range) {
                let z_span = (z_max - z_min).abs().max(1e-9);
                let mut bins: Vec<Vec<[f64; 2]>> = vec![Vec::new(); GRADIENT_BINS];
                for i in 0..timestamps_ms.len() {
                    let norm = ((zv[i] - z_min) / z_span).clamp(0.0, 1.0);
                    let bin = ((norm * ((GRADIENT_BINS - 1) as f64)).round() as usize)
                        .min(GRADIENT_BINS - 1);
                    bins[bin].push([x_vals[i], y_vals[i]]);
                }
                for (bin_idx, pts) in bins.into_iter().enumerate() {
                    if pts.is_empty() {
                        continue;
                    }
                    let t = (bin_idx as f32) / ((GRADIENT_BINS - 1) as f32);
                    let color = scalar_to_color(t).gamma_multiply(0.78);
                    color_bins.push((color, pts));
                }
            } else if !timestamps_ms.is_empty() {
                let all_pts: Vec<[f64; 2]> = x_vals
                    .iter()
                    .copied()
                    .zip(y_vals.iter().copied())
                    .map(|(x, y)| [x, y])
                    .collect();
                color_bins.push((
                    eframe::egui::Color32::from_rgba_unmultiplied(80, 190, 255, 150),
                    all_pts,
                ));
            }

            self.cached_data = Some(CachedScatterData {
                key: cache_key,
                x_unit,
                y_unit,
                z_unit,
                timestamps_ms,
                x_vals,
                y_vals,
                z_vals,
                x_bounds,
                y_bounds,
                z_range,
                color_bins,
            });
        }

        // Single compact Nominal-style header strip:
        // Clickable X / Y / Z pills with live cursor value + `…` menu for Presets, 1:1 Aspect, and G-Rings.
        let mut clear_z = false;
        let mut preset_choice: Option<u8> = None;

        let cursor_ms = view.active_cursor_utc_ms();
        let (x_pill, y_pill, z_pill, x_tip, y_tip, z_tip, x_axis_lbl, y_axis_lbl) = {
            let cached = self.cached_data.as_ref().expect("populated above");
            let cursor_idx = (!cached.timestamps_ms.is_empty()).then(|| {
                let pos = cached.timestamps_ms.partition_point(|&t| t <= cursor_ms);
                pos.saturating_sub(1).min(cached.timestamps_ms.len() - 1)
            });

            let x_u = if cached.x_unit.is_empty() {
                String::new()
            } else {
                format!(" {}", cached.x_unit)
            };
            let y_u = if cached.y_unit.is_empty() {
                String::new()
            } else {
                format!(" {}", cached.y_unit)
            };

            let x_val_str = cursor_idx
                .map(|i| format!(" = {:.2}{x_u}", cached.x_vals[i]))
                .unwrap_or_else(|| x_u.clone());
            let y_val_str = cursor_idx
                .map(|i| format!(" = {:.2}{y_u}", cached.y_vals[i]))
                .unwrap_or_else(|| y_u.clone());

            let (z_p, z_t) = if let Some(z_sig) = &self.z_color_signal {
                let z_u = if cached.z_unit.is_empty() {
                    String::new()
                } else {
                    format!(" {}", cached.z_unit)
                };
                let z_val = cursor_idx
                    .and_then(|i| cached.z_vals.as_ref().map(|zv| zv[i]))
                    .map(|v| format!(" = {v:.2}{z_u}"))
                    .unwrap_or_else(|| z_u.clone());
                let range_str = cached
                    .z_range
                    .map(|(lo, hi)| format!("\nRange: {lo:.2}{z_u} .. {hi:.2}{z_u}"))
                    .unwrap_or_default();
                (
                    format!("Z: {z_sig}{z_val}"),
                    format!("Color (Z): {z_sig}{range_str}\nClick to change or drag signal here"),
                )
            } else {
                (
                    "Z: None".to_owned(),
                    "Click to choose a Color (Z) gradient signal".to_owned(),
                )
            };

            let x_axis_l = if cached.x_unit.is_empty() {
                self.x_signal.clone()
            } else {
                format!("{} [{}]", self.x_signal, cached.x_unit)
            };
            let y_axis_l = if cached.y_unit.is_empty() {
                self.y_signal.clone()
            } else {
                format!("{} [{}]", self.y_signal, cached.y_unit)
            };

            (
                format!("X: {}{x_val_str}", self.x_signal),
                format!("Y: {}{y_val_str}", self.y_signal),
                z_p,
                format!(
                    "X Axis: {}\nClick to change or drag signal here",
                    self.x_signal
                ),
                format!(
                    "Y Axis: {}\nClick to change or drag signal here",
                    self.y_signal
                ),
                z_t,
                x_axis_l,
                y_axis_l,
            )
        };

        ui.horizontal_wrapped(|ui| {
            ui.style_mut().wrap_mode = Some(eframe::egui::TextWrapMode::Extend);

            for (slot, pill_text, tip_text) in [
                (ScatterAxisSlot::X, x_pill.as_str(), x_tip.as_str()),
                (ScatterAxisSlot::Y, y_pill.as_str(), y_tip.as_str()),
                (ScatterAxisSlot::ZColor, z_pill.as_str(), z_tip.as_str()),
            ] {
                let is_picking = self.picker_slot == Some(slot);
                let is_drop = self.active_drop_slot == slot;
                let badge = format!("{}{}", if is_drop { "▶ " } else { "" }, pill_text);
                if ui
                    .selectable_label(
                        is_picking || is_drop,
                        eframe::egui::RichText::new(badge).size(11.0),
                    )
                    .on_hover_text(tip_text)
                    .clicked()
                {
                    self.active_drop_slot = slot;
                    self.picker_slot = if is_picking { None } else { Some(slot) };
                }
            }

            if self.z_color_signal.is_some()
                && ui
                    .small_button("×")
                    .on_hover_text("Clear color gradient channel")
                    .clicked()
            {
                clear_z = true;
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
                                .on_hover_text("Reset to automatic XY title")
                                .clicked()
                        {
                            self.custom_title = false;
                            self.refresh_title();
                        }
                    });
                    ui.separator();
                    ui.label(
                        eframe::egui::RichText::new("Scatter Presets")
                            .weak()
                            .small(),
                    );
                    if ui.button("G-G Diagram").clicked() {
                        preset_choice = Some(0);
                        ui.close();
                    }
                    if ui.button("Understeer Diagram").clicked() {
                        preset_choice = Some(1);
                        ui.close();
                    }
                    if ui.button("Pack Temp / Voltage").clicked() {
                        preset_choice = Some(2);
                        ui.close();
                    }
                    ui.separator();
                    if ui
                        .checkbox(&mut self.equal_aspect, "1:1 Aspect Ratio")
                        .changed()
                    {
                        self.bounds_override = None;
                    }
                    if ui
                        .checkbox(&mut self.show_g_rings, "Show G-Rings")
                        .changed()
                    {
                        self.bounds_override = None;
                    }
                    if self.bounds_override.is_some() && ui.button("Reset Axis Zoom").clicked() {
                        self.bounds_override = None;
                        ui.close();
                    }
                    if self.z_color_signal.is_some() && ui.button("Clear Color (Z)").clicked() {
                        clear_z = true;
                        ui.close();
                    }
                });
        });

        match preset_choice {
            Some(0) => {
                *self = Self::preset_gg_diagram(self.instance_num);
                return egui_tiles::UiResponse::None;
            }
            Some(1) => {
                *self = Self::preset_understeer(self.instance_num);
                return egui_tiles::UiResponse::None;
            }
            Some(2) => {
                *self = Self::preset_battery_scatter(self.instance_num);
                return egui_tiles::UiResponse::None;
            }
            _ => {}
        }

        if clear_z {
            self.z_color_signal = None;
            if self.picker_slot == Some(ScatterAxisSlot::ZColor) {
                self.picker_slot = None;
            }
            self.cached_data = None;
            return egui_tiles::UiResponse::None;
        }

        if let Some(slot) = self.picker_slot {
            self.show_signal_picker(ui, slot, view);
            return egui_tiles::UiResponse::None;
        }

        let cached = self.cached_data.as_ref().expect("populated above");
        let timestamps_ms = &cached.timestamps_ms;
        let x_vals = &cached.x_vals;
        let y_vals = &cached.y_vals;
        let z_vals = &cached.z_vals;
        let z_range = cached.z_range;

        let setpoint_ms = view.timeline.setpoint_utc_ms();
        let hover_ms = view.hover_utc_ms();

        let text_color = ui.visuals().text_color();
        let plot_width = ui.available_width().max(60.0);
        let plot_height = ui.available_height().max(60.0);

        let (mut default_x_min, mut default_x_max) = cached.x_bounds;
        let (mut default_y_min, mut default_y_max) = cached.y_bounds;
        if self.show_g_rings {
            default_x_min = default_x_min.min(-1.65);
            default_x_max = default_x_max.max(1.65);
            default_y_min = default_y_min.min(-1.65);
            default_y_max = default_y_max.max(1.65);
        }
        if self.equal_aspect {
            let x_span = (default_x_max - default_x_min).abs().max(1e-6);
            let y_span = (default_y_max - default_y_min).abs().max(1e-6);
            let aspect = f64::from(plot_width) / f64::from(plot_height.max(1.0));
            if x_span / y_span < aspect {
                let target_x_span = y_span * aspect;
                let x_mid = 0.5 * (default_x_min + default_x_max);
                default_x_min = x_mid - 0.5 * target_x_span;
                default_x_max = x_mid + 0.5 * target_x_span;
            } else {
                let target_y_span = x_span / aspect;
                let y_mid = 0.5 * (default_y_min + default_y_max);
                default_y_min = y_mid - 0.5 * target_y_span;
                default_y_max = y_mid + 0.5 * target_y_span;
            }
        }
        let ([active_x_min, active_x_max], [active_y_min, active_y_max]) =
            self.bounds_override.unwrap_or((
                [default_x_min, default_x_max],
                [default_y_min, default_y_max],
            ));
        let mut next_bounds_override = self.bounds_override;

        let plot = egui_plot::Plot::new(("xy_scatter_plot_v2", self.instance_num))
            .width(plot_width)
            .height(plot_height)
            .x_axis_label(x_axis_lbl)
            .y_axis_label(y_axis_lbl)
            .allow_zoom(false)
            .allow_drag(false)
            .allow_boxed_zoom(false)
            .allow_scroll(false)
            .allow_double_click_reset(false);

        let has_z = z_vals.is_some() && z_range.is_some();
        let radius = if has_z { 2.4_f32 } else { 2.3_f32 };

        plot.show(ui, |plot_ui| {
            plot_ui.set_plot_bounds_x(active_x_min..=active_x_max);
            plot_ui.set_plot_bounds_y(active_y_min..=active_y_max);

            if self.show_g_rings {
                for (label, ring) in &self.ring_points {
                    plot_ui.line(
                        egui_plot::Line::new(
                            label.clone(),
                            egui_plot::PlotPoints::from(ring.clone()),
                        )
                        .color(text_color.linear_multiply(0.18))
                        .allow_hover(false),
                    );
                }
            }

            for (color, pts) in &cached.color_bins {
                plot_ui.points(
                    egui_plot::Points::new("", pts.clone())
                        .radius(radius)
                        .color(*color)
                        .allow_hover(false),
                );
            }

            // Recent comet trail leading up to playhead (`setpoint_ms`)
            if !timestamps_ms.is_empty() {
                let trail_start = setpoint_ms.saturating_sub(COMET_TRAIL_MS);
                let lo = timestamps_ms.partition_point(|&t| t < trail_start);
                let hi = timestamps_ms.partition_point(|&t| t <= setpoint_ms);
                if lo < hi {
                    let comet_pts: Vec<[f64; 2]> =
                        (lo..hi).map(|i| [x_vals[i], y_vals[i]]).collect();
                    plot_ui.line(
                        egui_plot::Line::new("", comet_pts.clone())
                            .color(eframe::egui::Color32::from_rgba_unmultiplied(
                                255, 220, 90, 180,
                            ))
                            .width(1.8_f32)
                            .allow_hover(false),
                    );
                    plot_ui.points(
                        egui_plot::Points::new("", comet_pts)
                            .radius(3.4_f32)
                            .color(eframe::egui::Color32::from_rgb(255, 210, 75))
                            .allow_hover(false),
                    );
                }

                // Playhead marker
                let play_idx = hi.saturating_sub(1).min(timestamps_ms.len() - 1);
                plot_ui.points(
                    egui_plot::Points::new("Playhead", vec![[x_vals[play_idx], y_vals[play_idx]]])
                        .radius(5.8_f32)
                        .color(eframe::egui::Color32::YELLOW)
                        .allow_hover(false),
                );
            }

            // Synchronized Hover Crosshair marker
            if let Some(h_ms) = hover_ms
                && !timestamps_ms.is_empty()
            {
                let h_idx = timestamps_ms
                    .partition_point(|&t| t <= h_ms)
                    .saturating_sub(1)
                    .min(timestamps_ms.len() - 1);
                plot_ui.points(
                    egui_plot::Points::new("Linked Cursor", vec![[x_vals[h_idx], y_vals[h_idx]]])
                        .radius(6.2_f32)
                        .color(eframe::egui::Color32::from_rgb(130, 235, 255))
                        .allow_hover(false),
                );
            }

            let popup_open = eframe::egui::Popup::is_any_open(plot_ui.ctx());
            if !popup_open && plot_ui.response().double_clicked() {
                next_bounds_override = None;
            }

            let x_span = (active_x_max - active_x_min).abs().max(1e-9);
            let y_span = (active_y_max - active_y_min).abs().max(1e-9);

            if !popup_open && plot_ui.response().hovered() {
                let (scroll_y, zoom_delta) = plot_ui
                    .ctx()
                    .input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
                let zoom_factor = if (zoom_delta - 1.0).abs() > 1e-3 {
                    Some((1.0 / f64::from(zoom_delta)).clamp(0.25, 4.0))
                } else if scroll_y.abs() > 0.5 {
                    Some(if scroll_y > 0.0 { 0.8 } else { 1.25 })
                } else {
                    None
                };
                if let Some(factor) = zoom_factor {
                    let anchor = plot_ui.pointer_coordinate().unwrap_or_else(|| {
                        egui_plot::PlotPoint::new(
                            0.5 * (active_x_min + active_x_max),
                            0.5 * (active_y_min + active_y_max),
                        )
                    });
                    let new_x_span = (x_span * factor).clamp(1e-6, 1e9);
                    let new_y_span = (y_span * factor).clamp(1e-6, 1e9);
                    let rx = ((anchor.x - active_x_min) / x_span).clamp(0.0, 1.0);
                    let ry = ((anchor.y - active_y_min) / y_span).clamp(0.0, 1.0);
                    let nx_min = anchor.x - rx * new_x_span;
                    let ny_min = anchor.y - ry * new_y_span;
                    next_bounds_override =
                        Some(([nx_min, nx_min + new_x_span], [ny_min, ny_min + new_y_span]));
                }
            }

            if !popup_open
                && plot_ui
                    .response()
                    .dragged_by(eframe::egui::PointerButton::Primary)
            {
                let delta = plot_ui.response().drag_delta();
                if delta.x.abs() > 0.0 || delta.y.abs() > 0.0 {
                    let dx = -f64::from(delta.x) / f64::from(plot_width.max(1.0)) * x_span;
                    let dy = f64::from(delta.y) / f64::from(plot_height.max(1.0)) * y_span;
                    next_bounds_override = Some((
                        [active_x_min + dx, active_x_max + dx],
                        [active_y_min + dy, active_y_max + dy],
                    ));
                }
            }

            // Interactive hover & click-to-setpoint on nearest point (normalized by independent X/Y spans)
            if plot_ui.response().hovered()
                && let Some(coord) = plot_ui.pointer_coordinate()
                && !timestamps_ms.is_empty()
            {
                let mut best_idx = 0usize;
                let mut best_d2 = f64::INFINITY;
                for i in 0..timestamps_ms.len() {
                    let dx = (x_vals[i] - coord.x) / x_span;
                    let dy = (y_vals[i] - coord.y) / y_span;
                    let d2 = dx * dx + dy * dy;
                    if d2 < best_d2 {
                        best_d2 = d2;
                        best_idx = i;
                    }
                }
                let nearest_ms = timestamps_ms[best_idx];
                view.set_hover_utc_ms(Some(nearest_ms));
                if plot_ui.response().clicked() {
                    view.request_setpoint_utc_ms(nearest_ms);
                }
            }
        });
        self.bounds_override = next_bounds_override;

        egui_tiles::UiResponse::None
    }

    fn show_signal_picker(
        &mut self,
        ui: &mut eframe::egui::Ui,
        slot: ScatterAxisSlot,
        view: &workspace_view::WorkspaceDataView<'_>,
    ) {
        let prompt = format!("Select signal for {}:", slot.label());
        match ui::message_picker::show_signal_picker(
            ui,
            ("xy_scatter_picker", self.instance_num),
            &prompt,
            true,
            false,
            &mut self.search_query,
            view,
        ) {
            ui::message_picker::SignalPickerAction::None => {}
            ui::message_picker::SignalPickerAction::Cancelled => {
                self.picker_slot = None;
            }
            ui::message_picker::SignalPickerAction::Picked(picked) => {
                self.set_axis_signal(slot, picked.signal_id);
                self.picker_slot = None;
            }
        }
    }
}

fn is_imu_accel_signal(sig_id: &str) -> bool {
    sig_id
        .trim_start_matches('-')
        .starts_with("IMU_acceleration.")
}

fn compute_axis_bounds(vals: &[f64]) -> (f64, f64) {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for &v in vals {
        if v.is_finite() {
            lo = lo.min(v);
            hi = hi.max(v);
        }
    }
    if !lo.is_finite() || !hi.is_finite() {
        return (-1.0, 1.0);
    }
    let span = (hi - lo).abs();
    let pad = if span < 1e-6 {
        (lo.abs() * 0.05).max(1.0)
    } else {
        span * 0.06
    };
    (lo - pad, hi + pad)
}

fn build_g_rings() -> Vec<(String, Vec<[f64; 2]>)> {
    [0.5, 1.0, 1.5]
        .into_iter()
        .map(|radius: f64| {
            let n = 64usize;
            let pts = (0..=n)
                .map(|i| {
                    let theta = (i as f64) * TAU / (n as f64);
                    [radius * theta.cos(), radius * theta.sin()]
                })
                .collect();
            (format!("{radius:.1}g"), pts)
        })
        .collect()
}

/// Smooth Blue -> Cyan -> Green -> Yellow -> Red gradient for `t in [0.0, 1.0]`.
pub fn scalar_to_color(t: f32) -> eframe::egui::Color32 {
    let t = t.clamp(0.0, 1.0);
    let (r, g, b) = if t < 0.25 {
        let u = t / 0.25;
        (40.0 * (1.0 - u), 110.0 + 130.0 * u, 245.0)
    } else if t < 0.5 {
        let u = (t - 0.25) / 0.25;
        (40.0 * u, 240.0, 245.0 * (1.0 - u) + 80.0 * u)
    } else if t < 0.75 {
        let u = (t - 0.5) / 0.25;
        (40.0 + 215.0 * u, 240.0 - 25.0 * u, 80.0 * (1.0 - u))
    } else {
        let u = (t - 0.75) / 0.25;
        (255.0, 215.0 * (1.0 - u) + 55.0 * u, 50.0 * u)
    };
    eframe::egui::Color32::from_rgb(r as u8, g as u8, b as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xy_scatter_presets_and_drag_assignment_render_cleanly() {
        let t0 = daqcore::time::Time::from_unix_millis(1_794_500_000_000);
        let router = daqcore::router::VirtualTimelineRouter::live(t0, 30.0).expect("live router");

        let mut scatter = XYScatter::preset_gg_diagram(1);
        assert_eq!(scatter.x_signal, "-IMU_acceleration.Y_axis");
        assert_eq!(scatter.y_signal, "IMU_acceleration.X_axis");
        assert_eq!(
            scatter.z_color_signal.as_deref(),
            Some("gps_speed_heading.gps_speed")
        );

        scatter.apply_dragged_signal(ui::signal_browser::DraggedSignal::Derived {
            signal_id: "vd.understeer_gradient".to_owned(),
            label: "Understeer".to_owned(),
            unit: "deg/g".to_owned(),
        });
        assert_eq!(scatter.x_signal, "vd.understeer_gradient");
        assert!(!scatter.equal_aspect);
        assert!(!scatter.show_g_rings);
        assert_eq!(
            scatter.title,
            "XY: vd.understeer_gradient vs IMU_acceleration.X_axis"
        );

        let rpm_bounds = compute_axis_bounds(&[0.0, 8800.0]);
        let g_bounds = compute_axis_bounds(&[-1.2, 1.4]);
        assert!(rpm_bounds.1 - rpm_bounds.0 > 8_800.0);
        assert!(g_bounds.1 - g_bounds.0 < 3.5);

        let ctx = eframe::egui::Context::default();
        ui::theme::store_theme(&ctx, ui::theme::ThemeSelection::default().get_colors());
        let view = workspace_view::WorkspaceDataView::new(&router);
        for mut w in [
            XYScatter::preset_gg_diagram(1),
            XYScatter::preset_understeer(2),
            XYScatter::preset_battery_scatter(3),
        ] {
            let raw_input = eframe::egui::RawInput {
                screen_rect: Some(eframe::egui::Rect::from_min_size(
                    eframe::egui::Pos2::ZERO,
                    eframe::egui::vec2(640.0, 480.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(raw_input, |ctx| {
                eframe::egui::CentralPanel::default().show(ctx, |ui| {
                    let _ = w.show(ui, &view);
                });
            });
        }
    }
}
