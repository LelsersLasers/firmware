use crate::{ui, workspace_view};
use std::{cell::Cell, rc::Rc};

const DEFAULT_CENTER_LAT: f64 = 40.4344;
const DEFAULT_CENTER_LON: f64 = -86.9183;

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
}

impl GpsPlot {
    pub fn new(instance: usize) -> Self {
        Self {
            title: format!("GPS Plot #{instance}"),
            instance_num: instance,
            tiles: None,
            map_memory: walkers::MapMemory::default(),
            overlay_signal: Some(("gps_speed_heading.gps_speed".to_owned(), "m/s".to_owned())),
        }
    }

    pub fn set_overlay_signal(&mut self, signal_id: String, unit: String) {
        self.overlay_signal = Some((signal_id, unit));
    }

    pub fn overlay_signal(&self) -> Option<&(String, String)> {
        self.overlay_signal.as_ref()
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
                self.set_overlay_signal(payload.signal_id(), payload.unit().to_owned());
                let _ = eframe::egui::DragAndDrop::take_payload::<ui::signal_browser::DraggedSignal>(
                    ui.ctx(),
                );
            }
        }

        // Toolbar for Color-by-Signal selection
        ui.horizontal_wrapped(|ui| {
            ui.strong(&self.title);
            ui.separator();
            ui.label("Color by:");
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
                    self.overlay_signal = Some((sig_id.to_owned(), unit.to_owned()));
                }
            }
            if ui
                .selectable_label(self.overlay_signal.is_none(), "Solid")
                .clicked()
            {
                self.overlay_signal = None;
            }
            if self.map_memory.detached().is_some()
                && ui
                    .small_button("Recenter")
                    .on_hover_text("Snap map camera back to vehicle position")
                    .clicked()
            {
                self.map_memory.follow_my_position();
            }
        });

        let width_px = ui.available_width().max(60.0);
        let res = view.timeline.resolution_for_width(width_px);
        let staleness_ms = view.router.formula_engine().max_staleness_ms().max(1_000);

        let lat_series = view.query_signal("gps_coordinates.latitude", res);
        let lon_series = view.query_signal("gps_coordinates.longitude", res);
        let aligned_gps = daqcore::analysis::align_xy_pairs(&lat_series, &lon_series, staleness_ms);

        let mut gps_points: Vec<(i64, f64, f64)> = Vec::with_capacity(aligned_gps.timestamps_ms.len());
        for ((t_ms, lat), lon) in aligned_gps
            .timestamps_ms
            .into_iter()
            .zip(aligned_gps.x_values)
            .zip(aligned_gps.y_values)
        {
            if lat.is_finite()
                && lon.is_finite()
                && lat.abs() <= 90.0
                && lon.abs() <= 180.0
                && (lat.abs() > 1e-4 || lon.abs() > 1e-4)
            {
                gps_points.push((t_ms, lat, lon));
            }
        }

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
                if let Some((min_v, max_v)) = overlay_min_max {
                    ui.colored_label(
                        ui::xy_scatter::scalar_to_color(0.0),
                        format!("■ {min_v:.1}{unit_str}"),
                    );
                    if let Some(cur_v) = cursor_overlay_val {
                        ui.label(format!("→ {sig_id} ({cur_v:.2}{unit_str}) →"));
                    } else {
                        ui.label(format!("→ {sig_id} →"));
                    }
                    ui.colored_label(
                        ui::xy_scatter::scalar_to_color(1.0),
                        format!("■ {max_v:.1}{unit_str}"),
                    );
                } else {
                    ui.colored_label(
                        eframe::egui::Color32::from_rgb(80, 190, 255),
                        format!("Color: {sig_id}"),
                    );
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
        let stride = gps_points.len().div_ceil(4096).max(1);

        let mut trail: Vec<TrailPoint> = gps_points
            .iter()
            .step_by(stride)
            .map(|&(t_ms, lat, lon)| {
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
                TrailPoint {
                    utc_ms: t_ms,
                    position: walkers::lon_lat(lon, lat),
                    age_fraction,
                    before_setpoint,
                    norm_val,
                }
            })
            .collect();

        if let Some(&(last_t, last_lat, last_lon)) = gps_points.last()
            && trail.last().is_none_or(|tp| tp.utc_ms != last_t)
        {
            let norm_val = trail.last().and_then(|tp| tp.norm_val);
            trail.push(TrailPoint {
                utc_ms: last_t,
                position: walkers::lon_lat(last_lon, last_lat),
                age_fraction: 1.0,
                before_setpoint: last_t <= setpoint_ms,
                norm_val,
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
        gps.set_overlay_signal("battery.power_kw".to_owned(), "kW".to_owned());
        assert_eq!(
            gps.overlay_signal(),
            Some(&("battery.power_kw".to_owned(), "kW".to_owned()))
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
        let _ = ctx.run(raw_input, |ctx| {
            eframe::egui::CentralPanel::default().show(ctx, |ui| {
                let _ = gps.show(ui, &view);
            });
        });
    }
}
