use crate::{app, ui, workspace_view};
use std::{cell::Cell, rc::Rc};

const DEFAULT_CENTER_LAT: f64 = 40.4344;
const DEFAULT_CENTER_LON: f64 = -86.9183;
const EARTH_RADIUS_METERS: f64 = 6_371_008.8;
const DEFAULT_MAX_DISTANCE_METERS: f64 = 500.0;
const MAX_OVERVIEW_BUCKET_SPAN_DEG: f64 = 0.01;
const MAX_GPS_STEP_SPEED_MPS: f64 = 80.0;
const MAX_CONTIGUOUS_GAP_MS: i64 = 2_500;
const MAX_CONTIGUOUS_STEP_METERS: f64 = 50.0;
const GPS_FULL_RES_THRESHOLD_MS: i64 = 5 * 60 * 1_000;
const GPS_OVERVIEW_10HZ_THRESHOLD_MS: i64 = 30 * 60 * 1_000;
const MAX_TRAIL_POINTS: usize = 16_384;

const QUICK_COLOR_CHANNELS: &[(&str, &str, &str)] = &[
    ("Speed", "gps_speed_heading.gps_speed", "m/s"),
    ("Long G", "IMU_acceleration.X_axis", "g"),
    ("Lat G", "IMU_acceleration.Y_axis", "g"),
    ("Yaw Rate", "IMU_angular_rate.Z_axis", "deg/s"),
    ("Throttle", "pedals.throttle", "%"),
    ("Brake", "pedals.brake", "%"),
    ("Pack kW", "battery.power_kw", "kW"),
];

// Satellite imagery tile source
// <https://www.arcgis.com/home/item.html?id=10df2279f9684e4a9f6a7f08febac2a9>
struct EsriWorldImagery;

impl walkers::sources::TileSource for EsriWorldImagery {
    fn tile_url(&self, tile_id: walkers::TileId) -> String {
        format!(
            "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{}/{}/{}",
            tile_id.zoom, tile_id.y, tile_id.x
        )
    }

    fn attribution(&self) -> walkers::sources::Attribution {
        walkers::sources::Attribution {
            text: "Esri, Maxar, Earthstar Geographics, and the GIS User Community",
            url: "https://www.esri.com/en-us/legal/copyright-trademarks",
            logo_light: None,
            logo_dark: None,
        }
    }
}

pub struct GpsPlot {
    pub title: String,
    instance_num: usize,
    tiles: Option<walkers::HttpTiles>,
    map_memory: walkers::MapMemory,
    overlay_signal: Option<(String, String)>,
    max_distance_meters: f64,
    show_picker: bool,
    search_query: String,
}

impl GpsPlot {
    pub fn new(instance: usize) -> Self {
        Self {
            title: format!("GPS Plot #{instance}"),
            instance_num: instance,
            tiles: None,
            map_memory: walkers::MapMemory::default(),
            overlay_signal: Some(("gps_speed_heading.gps_speed".to_owned(), "m/s".to_owned())),
            max_distance_meters: DEFAULT_MAX_DISTANCE_METERS,
            show_picker: false,
            search_query: String::new(),
        }
    }

    pub fn set_overlay_signal(&mut self, signal_id: String, unit: String) {
        self.overlay_signal = Some((signal_id, unit));
        self.show_picker = false;
    }

    #[cfg(test)]
    pub fn overlay_signal(&self) -> Option<&(String, String)> {
        self.overlay_signal.as_ref()
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
                self.set_overlay_signal(payload.signal_id(), payload.unit().to_owned());
                let _ = eframe::egui::DragAndDrop::take_payload::<ui::signal_browser::DraggedSignal>(
                    ui.ctx(),
                );
            }
        }

        // Toolbar: arbitrary Color-by-Signal selector (like XYScatter) & outlier filter radius
        ui.spacing_mut().item_spacing = eframe::egui::vec2(4.0, 2.0);
        let mut clear_color = false;
        ui.horizontal_wrapped(|ui| {
            let color_badge = match &self.overlay_signal {
                Some((sig_id, unit)) if !unit.is_empty() => format!("Color: {sig_id} [{unit}]"),
                Some((sig_id, _)) => format!("Color: {sig_id}"),
                None => "Color: None (Solid)".to_owned(),
            };
            if ui
                .selectable_label(self.show_picker, color_badge)
                .on_hover_text(
                    "Click to choose any msg.signal or derived formula for trajectory coloring, or drag a signal from the Signal Browser",
                )
                .clicked()
            {
                self.show_picker = !self.show_picker;
            }
            if self.overlay_signal.is_some()
                && ui
                    .small_button("× Color")
                    .on_hover_text("Clear color gradient channel (use solid trajectory color)")
                    .clicked()
            {
                clear_color = true;
            }
            ui.separator();
            ui.label("Radius:");
            ui.add(
                eframe::egui::DragValue::new(&mut self.max_distance_meters)
                    .range(50.0..=50_000.0)
                    .speed(10.0)
                    .suffix(" m"),
            )
            .on_hover_text("Max distance from median track origin (filters invalid GPS outliers)");
            if self.map_memory.detached().is_some()
                && ui
                    .small_button("Recenter")
                    .on_hover_text("Snap map camera back to vehicle position")
                    .clicked()
            {
                self.map_memory.follow_my_position();
            }
        });
        if clear_color {
            self.overlay_signal = None;
            self.show_picker = false;
        }

        if self.show_picker {
            ui.horizontal_wrapped(|ui| {
                ui.label(eframe::egui::RichText::new("Quick:").weak().small());
                for &(short_lbl, sig_id, unit) in QUICK_COLOR_CHANNELS {
                    let is_sel = self
                        .overlay_signal
                        .as_ref()
                        .is_some_and(|(id, _)| id == sig_id);
                    if ui
                        .selectable_label(is_sel, short_lbl)
                        .on_hover_text(sig_id)
                        .clicked()
                    {
                        self.set_overlay_signal(sig_id.to_owned(), unit.to_owned());
                    }
                }
                if ui
                    .selectable_label(self.overlay_signal.is_none(), "Solid (None)")
                    .clicked()
                {
                    self.overlay_signal = None;
                    self.show_picker = false;
                }
            });
            match ui::message_picker::show_signal_picker(
                ui,
                ("gps_signal_picker", self.instance_num),
                "Select color signal:",
                true,
                false,
                &mut self.search_query,
                database,
                view,
            ) {
                ui::message_picker::SignalPickerAction::None => {}
                ui::message_picker::SignalPickerAction::Cancelled => {
                    self.show_picker = false;
                }
                ui::message_picker::SignalPickerAction::Picked(picked) => {
                    self.set_overlay_signal(picked.signal_id, picked.unit);
                }
            }
            return egui_tiles::UiResponse::None;
        }

        let window_ms = view
            .timeline
            .end_utc_ms()
            .saturating_sub(view.timeline.start_utc_ms());
        let res = gps_resolution_for_span(window_ms);
        let staleness_ms = view.router.formula_engine().max_staleness_ms().max(1_000);

        let lat_series = view.query_signal("gps_coordinates.latitude", res);
        let lon_series = view.query_signal("gps_coordinates.longitude", res);
        let gps_points = filter_and_align_gps_points(
            &lat_series,
            &lon_series,
            staleness_ms,
            Some(self.max_distance_meters),
        );

        let overlay_samples: Vec<(i64, f64)> = if let Some((sig_id, _)) = &self.overlay_signal {
            match view.query_signal(sig_id, res) {
                daqcore::router::SignalSeries::Exact {
                    timestamps_ms,
                    values,
                } => timestamps_ms
                    .into_iter()
                    .zip(values)
                    .filter(|(_, v)| v.is_finite())
                    .collect(),
                daqcore::router::SignalSeries::Envelope(buckets) => buckets
                    .into_iter()
                    .filter(|b| b.mean.is_finite())
                    .map(|b| (b.utc_ms, b.mean))
                    .collect(),
            }
        } else {
            Vec::new()
        };

        let overlay_min_max = if overlay_samples.is_empty() {
            None
        } else {
            let mut min_v = f64::INFINITY;
            let mut max_v = f64::NEG_INFINITY;
            for &(_, v) in &overlay_samples {
                min_v = min_v.min(v);
                max_v = max_v.max(v);
            }
            (min_v.is_finite() && max_v.is_finite()).then_some((min_v, max_v))
        };

        let setpoint_ms = view.timeline.setpoint_utc_ms();
        let hover_ms = view.hover_utc_ms();
        let cursor_ms = view.active_cursor_utc_ms();

        let playhead_fix = find_gps_at_or_before(&gps_points, setpoint_ms)
            .or_else(|| gps_points.last().copied());
        let hover_fix = hover_ms.and_then(|h_ms| find_gps_at_or_before(&gps_points, h_ms));
        let cursor_overlay_val = (!overlay_samples.is_empty()).then(|| {
            let idx = overlay_samples
                .partition_point(|(ts, _)| *ts <= cursor_ms)
                .saturating_sub(1)
                .min(overlay_samples.len() - 1);
            overlay_samples[idx].1
        });

        ui.horizontal_wrapped(|ui| {
            if let Some((t_ms, lat, lon)) = hover_fix.or(playhead_fix) {
                ui.label(
                    eframe::egui::RichText::new(format!(
                        "Fix: {lat:.6}, {lon:.6} @ {}",
                        daqcore::time::Time::from_unix_millis(t_ms).label()
                    ))
                    .monospace()
                    .small(),
                );
            } else {
                ui.label(
                    eframe::egui::RichText::new("No GPS fix in active window.")
                        .weak()
                        .small(),
                );
            }

            if let Some((sig_id, unit)) = &self.overlay_signal {
                ui.separator();
                let resolved_unit = if unit.is_empty() {
                    view.resolve_signal_unit(sig_id, None)
                } else {
                    unit.clone()
                };
                let unit_str = if resolved_unit.is_empty() {
                    ""
                } else {
                    resolved_unit.as_str()
                };
                let short_sig = sig_id.rsplit('.').next().unwrap_or(sig_id);
                if let Some((min_v, max_v)) = overlay_min_max {
                    ui.colored_label(
                        ui::xy_scatter::scalar_to_color(0.0),
                        eframe::egui::RichText::new(format!("■ {min_v:.1}{unit_str}")).small(),
                    );
                    if let Some(cur_v) = cursor_overlay_val {
                        ui.label(
                            eframe::egui::RichText::new(format!(
                                "{short_sig}: {cur_v:.1}{unit_str}"
                            ))
                            .small(),
                        )
                        .on_hover_text(sig_id);
                    } else {
                        ui.label(eframe::egui::RichText::new(short_sig).small())
                            .on_hover_text(sig_id);
                    }
                    ui.colored_label(
                        ui::xy_scatter::scalar_to_color(1.0),
                        eframe::egui::RichText::new(format!("■ {max_v:.1}{unit_str}")).small(),
                    );
                } else {
                    ui.colored_label(
                        eframe::egui::Color32::from_rgb(80, 190, 255),
                        eframe::egui::RichText::new(format!("Color: {short_sig}")).small(),
                    )
                    .on_hover_text(sig_id);
                }
            }
        });

        let center_pos = playhead_fix
            .map(|(_, lat, lon)| walkers::lon_lat(lon, lat))
            .unwrap_or_else(|| walkers::lon_lat(DEFAULT_CENTER_LON, DEFAULT_CENTER_LAT));
        if playhead_fix.is_some() && self.map_memory.detached().is_none() {
            self.map_memory.center_at(center_pos);
        }

        let start_ms = view.timeline.start_utc_ms();
        let span_ms = (setpoint_ms - start_ms).max(1) as f64;
        let stride = gps_points.len().div_ceil(MAX_TRAIL_POINTS).max(1);

        let mut trail: Vec<TrailPoint> = Vec::with_capacity(gps_points.len() / stride + 2);
        let mut prev_trail_fix: Option<(i64, f64, f64)> = None;
        for &(t_ms, lat, lon) in gps_points.iter().step_by(stride) {
            let before_setpoint = t_ms <= setpoint_ms;
            let age_fraction = if before_setpoint {
                ((t_ms - start_ms) as f64 / span_ms).clamp(0.0, 1.0) as f32
            } else {
                0.22
            };
            let norm_val = overlay_min_max.and_then(|(min_v, max_v)| {
                let idx = overlay_samples
                    .partition_point(|(ts, _)| *ts <= t_ms)
                    .saturating_sub(1);
                let (_, val) = *overlay_samples.get(idx)?;
                let denom = (max_v - min_v).abs().max(1e-6);
                Some(((val - min_v) / denom).clamp(0.0, 1.0) as f32)
            });
            let gap_before = prev_trail_fix.is_some_and(|(pt, plat, plon)| {
                let dt_ms = t_ms.saturating_sub(pt);
                let step_m = equirectangular_distance_m(plat, plon, lat, lon);
                dt_ms > MAX_CONTIGUOUS_GAP_MS * (stride as i64)
                    || step_m > MAX_CONTIGUOUS_STEP_METERS * (stride as f64)
            });
            prev_trail_fix = Some((t_ms, lat, lon));
            trail.push(TrailPoint {
                utc_ms: t_ms,
                position: walkers::lon_lat(lon, lat),
                age_fraction,
                before_setpoint,
                norm_val,
                gap_before,
            });
        }

        if let Some(&(last_t, last_lat, last_lon)) = gps_points.last()
            && trail.last().is_none_or(|tp| tp.utc_ms != last_t)
        {
            let norm_val = trail.last().and_then(|tp| tp.norm_val);
            let gap_before = prev_trail_fix.is_some_and(|(pt, plat, plon)| {
                let dt_ms = last_t.saturating_sub(pt);
                let step_m = equirectangular_distance_m(plat, plon, last_lat, last_lon);
                dt_ms > MAX_CONTIGUOUS_GAP_MS || step_m > MAX_CONTIGUOUS_STEP_METERS
            });
            trail.push(TrailPoint {
                utc_ms: last_t,
                position: walkers::lon_lat(last_lon, last_lat),
                age_fraction: 1.0,
                before_setpoint: last_t <= setpoint_ms,
                norm_val,
                gap_before,
            });
        }

        let hovered_out = Rc::new(Cell::new(None));
        let clicked_out = Rc::new(Cell::new(None));

        let tiles = self
            .tiles
            .get_or_insert_with(|| walkers::HttpTiles::new(EsriWorldImagery, ui.ctx().clone()));
        let _ = self.instance_num;
        ui.add(
            walkers::Map::new(Some(tiles), &mut self.map_memory, center_pos).with_plugin(
                CarTrajectoryPlugin {
                    trail,
                    playhead_pos: playhead_fix.map(|(_, lat, lon)| walkers::lon_lat(lon, lat)),
                    hover_pos: hover_fix.map(|(_, lat, lon)| walkers::lon_lat(lon, lat)),
                    hovered_utc_ms: Rc::clone(&hovered_out),
                    clicked_utc_ms: Rc::clone(&clicked_out),
                },
            ),
        );

        if let Some(h_ms) = hovered_out.get() {
            view.set_hover_utc_ms(Some(h_ms));
        }
        if let Some(c_ms) = clicked_out.get() {
            view.request_setpoint_utc_ms(c_ms);
        }

        egui_tiles::UiResponse::None
    }
}

/// Choose GPS query resolution from the viewport time span alone (ignoring pane pixel width,
/// since a 2D map's width does not correspond to time).
fn gps_resolution_for_span(span_ms: i64) -> daqcore::router::Resolution {
    let span_ms = span_ms.max(0);
    if span_ms <= GPS_FULL_RES_THRESHOLD_MS {
        daqcore::router::Resolution::FullRes
    } else if span_ms <= GPS_OVERVIEW_10HZ_THRESHOLD_MS {
        daqcore::router::Resolution::Overview10Hz
    } else {
        daqcore::router::Resolution::Overview1Hz
    }
}

/// Sanitize a single GPS coordinate series (`latitude` or `longitude`) BEFORE temporal
/// alignment/interpolation so `(0.0, 0.0)` unlocked fixes and overview buckets spanning
/// lock transitions never pollute adjacent valid fixes via linear interpolation or bucket averaging.
fn sanitize_gps_coordinate_series(
    series: &daqcore::router::SignalSeries,
    max_abs_deg: f64,
) -> daqcore::router::SignalSeries {
    match series {
        daqcore::router::SignalSeries::Exact {
            timestamps_ms,
            values,
        } => {
            let mut clean_ts = Vec::with_capacity(timestamps_ms.len());
            let mut clean_vals = Vec::with_capacity(values.len());
            for (&t, &v) in timestamps_ms.iter().zip(values.iter()) {
                if v.is_finite() && v.abs() > 1e-4 && v.abs() <= max_abs_deg {
                    clean_ts.push(t);
                    clean_vals.push(v);
                }
            }
            daqcore::router::SignalSeries::Exact {
                timestamps_ms: clean_ts,
                values: clean_vals,
            }
        }
        daqcore::router::SignalSeries::Envelope(buckets) => {
            let mut clean_ts = Vec::with_capacity(buckets.len());
            let mut clean_vals = Vec::with_capacity(buckets.len());
            for b in buckets {
                if b.mean.is_finite()
                    && b.min.is_finite()
                    && b.max.is_finite()
                    && b.min.abs() > 1e-4
                    && b.max.abs() > 1e-4
                    && b.mean.abs() > 1e-4
                    && b.max.abs() <= max_abs_deg
                    && b.min.abs() <= max_abs_deg
                    && (b.max - b.min).abs() <= MAX_OVERVIEW_BUCKET_SPAN_DEG
                {
                    clean_ts.push(b.utc_ms);
                    clean_vals.push(b.mean);
                }
            }
            daqcore::router::SignalSeries::Exact {
                timestamps_ms: clean_ts,
                values: clean_vals,
            }
        }
    }
}

fn median_f64(mut values: Vec<f64>) -> Option<f64> {
    values.retain(|v| v.is_finite());
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.total_cmp(b));
    let mid = values.len() / 2;
    if values.len() % 2 == 1 {
        Some(values[mid])
    } else {
        Some(0.5 * (values[mid - 1] + values[mid]))
    }
}

fn equirectangular_distance_m(lat0: f64, lon0: f64, lat1: f64, lon1: f64) -> f64 {
    let mean_lat_rad = (0.5 * (lat0 + lat1)).to_radians();
    let lon_scale = (std::f64::consts::PI / 180.0) * EARTH_RADIUS_METERS * mean_lat_rad.cos();
    let lat_scale = (std::f64::consts::PI / 180.0) * EARTH_RADIUS_METERS;
    let east_m = (lon1 - lon0) * lon_scale;
    let north_m = (lat1 - lat0) * lat_scale;
    east_m.hypot(north_m)
}

/// Filter and align `(utc_ms, latitude, longitude)` fixes using `plot_factory`'s coordinate
/// validation and median-origin local tangent plane (`east_m`, `north_m`) radius filter,
/// plus pre-interpolation zero/bucket sanitization and single-sample spike rejection.
pub fn filter_and_align_gps_points(
    lat_series: &daqcore::router::SignalSeries,
    lon_series: &daqcore::router::SignalSeries,
    staleness_ms: i64,
    max_distance_meters: Option<f64>,
) -> Vec<(i64, f64, f64)> {
    let clean_lat = sanitize_gps_coordinate_series(lat_series, 90.0);
    let clean_lon = sanitize_gps_coordinate_series(lon_series, 180.0);
    let aligned_gps = daqcore::analysis::align_xy_pairs(&clean_lat, &clean_lon, staleness_ms);

    let mut candidates: Vec<(i64, f64, f64)> = Vec::with_capacity(aligned_gps.timestamps_ms.len());
    for ((t_ms, lat), lon) in aligned_gps
        .timestamps_ms
        .into_iter()
        .zip(aligned_gps.x_values)
        .zip(aligned_gps.y_values)
    {
        if lat.is_finite()
            && lon.is_finite()
            && (-90.0..=90.0).contains(&lat)
            && (-180.0..=180.0).contains(&lon)
            && lat.abs() > 1e-4
            && lon.abs() > 1e-4
        {
            candidates.push((t_ms, lat, lon));
        }
    }

    if candidates.is_empty() {
        return Vec::new();
    }

    let Some(latitude_origin) = median_f64(candidates.iter().map(|&(_, lat, _)| lat).collect())
    else {
        return Vec::new();
    };
    let Some(longitude_origin) = median_f64(candidates.iter().map(|&(_, _, lon)| lon).collect())
    else {
        return Vec::new();
    };

    let longitude_scale = (std::f64::consts::PI / 180.0)
        * EARTH_RADIUS_METERS
        * latitude_origin.to_radians().cos();
    let latitude_scale = (std::f64::consts::PI / 180.0) * EARTH_RADIUS_METERS;

    let mut out: Vec<(i64, f64, f64)> = Vec::with_capacity(candidates.len());
    for (t_ms, lat, lon) in candidates {
        if let Some(max_dist_m) = max_distance_meters {
            let east_m = (lon - longitude_origin) * longitude_scale;
            let north_m = (lat - latitude_origin) * latitude_scale;
            if east_m.hypot(north_m) > max_dist_m {
                continue;
            }
        }

        // Reject impossible single-step teleport glitches within a short time interval
        if let Some(&(prev_t, prev_lat, prev_lon)) = out.last() {
            let dt_ms = t_ms.saturating_sub(prev_t);
            if dt_ms > 0 && dt_ms <= MAX_CONTIGUOUS_GAP_MS {
                let step_m = equirectangular_distance_m(prev_lat, prev_lon, lat, lon);
                let dt_s = (dt_ms as f64 / 1000.0).max(0.02);
                if step_m / dt_s > MAX_GPS_STEP_SPEED_MPS {
                    continue;
                }
            }
        }

        out.push((t_ms, lat, lon));
    }

    out
}

fn find_gps_at_or_before(
    points: &[(i64, f64, f64)],
    target_ms: i64,
) -> Option<(i64, f64, f64)> {
    if points.is_empty() {
        return None;
    }
    let idx = points
        .partition_point(|&(t, _, _)| t <= target_ms)
        .saturating_sub(1)
        .min(points.len() - 1);
    Some(points[idx])
}

#[derive(Clone, Copy)]
struct TrailPoint {
    utc_ms: i64,
    position: walkers::Position,
    age_fraction: f32,
    before_setpoint: bool,
    norm_val: Option<f32>,
    gap_before: bool,
}

struct CarTrajectoryPlugin {
    trail: Vec<TrailPoint>,
    playhead_pos: Option<walkers::Position>,
    hover_pos: Option<walkers::Position>,
    hovered_utc_ms: Rc<Cell<Option<i64>>>,
    clicked_utc_ms: Rc<Cell<Option<i64>>>,
}

impl walkers::Plugin for CarTrajectoryPlugin {
    fn run(
        self: Box<Self>,
        ui: &mut eframe::egui::Ui,
        response: &eframe::egui::Response,
        projector: &walkers::Projector,
        _map_memory: &walkers::MapMemory,
    ) {
        if self.trail.is_empty() {
            return;
        }

        let painter = ui.painter_at(response.rect);
        let screen_points: Vec<eframe::egui::Pos2> = self
            .trail
            .iter()
            .map(|tp| projector.project(tp.position).to_pos2())
            .collect();

        for i in 1..screen_points.len() {
            let tp = self.trail[i];
            if tp.gap_before {
                continue;
            }
            let alpha = if tp.before_setpoint {
                0.35 + 0.65 * tp.age_fraction
            } else {
                0.28
            };
            let width = if tp.before_setpoint {
                1.6 + 2.0 * tp.age_fraction
            } else {
                1.3
            };
            let base_color = tp
                .norm_val
                .map(ui::xy_scatter::scalar_to_color)
                .unwrap_or(eframe::egui::Color32::from_rgb(80, 190, 255));

            painter.line_segment(
                [screen_points[i - 1], screen_points[i]],
                eframe::egui::Stroke::new(width, base_color.gamma_multiply(alpha)),
            );
        }

        // Playhead (`setpoint`) vehicle marker
        if let Some(play_pos) = self.playhead_pos {
            let screen_pt = projector.project(play_pos).to_pos2();
            painter.circle_filled(screen_pt, 5.5, eframe::egui::Color32::YELLOW);
            painter.circle_stroke(
                screen_pt,
                5.5,
                eframe::egui::Stroke::new(1.8_f32, eframe::egui::Color32::BLACK),
            );
        }

        // Synchronized Hover Crosshair vehicle marker
        if let Some(h_pos) = self.hover_pos {
            let screen_pt = projector.project(h_pos).to_pos2();
            painter.circle_filled(
                screen_pt,
                5.0,
                eframe::egui::Color32::from_rgb(120, 235, 255),
            );
            painter.circle_stroke(
                screen_pt,
                6.5,
                eframe::egui::Stroke::new(1.6_f32, eframe::egui::Color32::WHITE),
            );
        }

        // Hover & click along the GPS trajectory to drive linked cursor & setpoint
        if let Some(ptr) = response.hover_pos()
            && !response.dragged()
        {
            let mut best_idx = 0usize;
            let mut best_d2 = f32::INFINITY;
            for (idx, &sp) in screen_points.iter().enumerate() {
                let d2 = sp.distance_sq(ptr);
                if d2 < best_d2 {
                    best_d2 = d2;
                    best_idx = idx;
                }
            }
            if best_d2 <= 28.0 * 28.0 {
                let t_ms = self.trail[best_idx].utc_ms;
                self.hovered_utc_ms.set(Some(t_ms));
                if response.clicked() {
                    self.clicked_utc_ms.set(Some(t_ms));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gps_plot_colors_trajectory_by_signal_and_renders_linked_cursor() {
        let t0 = daqcore::time::Time::from_unix_millis(1_794_500_000_000);
        let router = daqcore::router::VirtualTimelineRouter::live(t0, 30.0).expect("live router");
        let mut gps = GpsPlot::new(1);
        assert_eq!(
            gps.overlay_signal().map(|(s, _)| s.as_str()),
            Some("gps_speed_heading.gps_speed")
        );
        gps.set_overlay_signal("front_Sabre.front_right_temp".to_owned(), "C".to_owned());
        assert_eq!(
            gps.overlay_signal(),
            Some(&("front_Sabre.front_right_temp".to_owned(), "C".to_owned()))
        );

        let ctx = eframe::egui::Context::default();
        ui::theme::store_theme(&ctx, ui::theme::ThemeSelection::default().get_colors());
        let view = workspace_view::WorkspaceDataView::with_hover(
            &router,
            Some(t0.unix_millis() - 3_000),
            None,
        );
        let raw_input = eframe::egui::RawInput {
            screen_rect: Some(eframe::egui::Rect::from_min_size(
                eframe::egui::Pos2::ZERO,
                eframe::egui::vec2(720.0, 520.0),
            )),
            ..Default::default()
        };
        let _ = ctx.run(raw_input.clone(), |ctx| {
            eframe::egui::CentralPanel::default().show(ctx, |ui| {
                let _ = gps.show(ui, None, &view);
            });
        });

        gps.show_picker = true;
        let _ = ctx.run(raw_input, |ctx| {
            eframe::egui::CentralPanel::default().show(ctx, |ui| {
                let _ = gps.show(ui, None, &view);
            });
        });
    }

    #[test]
    fn gps_filtering_rejects_zeros_corrupted_buckets_distant_outliers_and_spikes() {
        // 1. Exact series with pre-lock 0.0, single-axis 0.0, a >500m outlier, a 100ms teleport spike, and valid track points
        let lat_series = daqcore::router::SignalSeries::Exact {
            timestamps_ms: vec![
                1_000, // (0.0, 0.0) pre-lock
                1_050, // (40.43440, 0.0) single-axis zero
                1_100, // valid 1
                1_200, // valid 2
                1_300, // valid 3
                1_400, // >500m distant outlier (41.0, -86.9183)
                1_500, // valid 4
                1_550, // 300m jump in 50ms (6000 m/s teleport glitch inside 500m radius)
                1_600, // valid 5
            ],
            values: vec![
                0.0, 40.43440, 40.43440, 40.43442, 40.43444, 41.00000, 40.43446, 40.43700, 40.43448,
            ],
        };
        let lon_series = daqcore::router::SignalSeries::Exact {
            timestamps_ms: vec![1_000, 1_050, 1_100, 1_200, 1_300, 1_400, 1_500, 1_550, 1_600],
            values: vec![
                0.0,
                0.0,
                -86.91830,
                -86.91828,
                -86.91826,
                -86.91830,
                -86.91824,
                -86.91824,
                -86.91822,
            ],
        };

        let cleaned = filter_and_align_gps_points(&lat_series, &lon_series, 1_000, Some(500.0));
        let cleaned_ts: Vec<i64> = cleaned.iter().map(|&(t, _, _)| t).collect();
        assert_eq!(cleaned_ts, vec![1_100, 1_200, 1_300, 1_500, 1_600]);
        for &(_, lat, lon) in &cleaned {
            assert!((lat - 40.4344).abs() < 0.001, "unexpected lat {lat}");
            assert!((lon - (-86.9183)).abs() < 0.001, "unexpected lon {lon}");
        }

        // 2. Envelope buckets where bucket 0 blends 0.0 and 40.4344 (mean = 20.2172)
        let env_lat = daqcore::router::SignalSeries::Envelope(vec![
            daqcore::router::EnvelopePoint {
                utc_ms: 1_000,
                min: 0.0,
                max: 40.43440,
                mean: 20.21720,
            },
            daqcore::router::EnvelopePoint {
                utc_ms: 2_000,
                min: 40.43440,
                max: 40.43445,
                mean: 40.43442,
            },
            daqcore::router::EnvelopePoint {
                utc_ms: 3_000,
                min: 40.43445,
                max: 40.43450,
                mean: 40.43448,
            },
        ]);
        let env_lon = daqcore::router::SignalSeries::Envelope(vec![
            daqcore::router::EnvelopePoint {
                utc_ms: 1_000,
                min: -86.91830,
                max: 0.0,
                mean: -43.45915,
            },
            daqcore::router::EnvelopePoint {
                utc_ms: 2_000,
                min: -86.91830,
                max: -86.91825,
                mean: -86.91828,
            },
            daqcore::router::EnvelopePoint {
                utc_ms: 3_000,
                min: -86.91825,
                max: -86.91820,
                mean: -86.91822,
            },
        ]);
        let env_cleaned = filter_and_align_gps_points(&env_lat, &env_lon, 1_500, Some(500.0));
        let env_ts: Vec<i64> = env_cleaned.iter().map(|&(t, _, _)| t).collect();
        assert_eq!(
            env_ts,
            vec![2_000, 3_000],
            "Mixed-lock overview bucket at 1000ms must be rejected"
        );
    }

    #[test]
    fn gps_resolution_uses_custom_span_thresholds() {
        use daqcore::router::Resolution;

        assert_eq!(gps_resolution_for_span(-100), Resolution::FullRes);
        assert_eq!(gps_resolution_for_span(0), Resolution::FullRes);
        assert_eq!(gps_resolution_for_span(5 * 60 * 1_000), Resolution::FullRes);

        assert_eq!(
            gps_resolution_for_span(5 * 60 * 1_000 + 1),
            Resolution::Overview10Hz
        );
        assert_eq!(
            gps_resolution_for_span(30 * 60 * 1_000),
            Resolution::Overview10Hz
        );

        assert_eq!(
            gps_resolution_for_span(30 * 60 * 1_000 + 1),
            Resolution::Overview1Hz
        );
    }
}
