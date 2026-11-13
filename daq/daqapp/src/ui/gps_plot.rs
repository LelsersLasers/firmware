use crate::{telemetry, ui};

const DEFAULT_CENTER_LAT: f64 = 40.4344;
const DEFAULT_CENTER_LON: f64 = -86.9183;
// satellite imagery instead of street map
// <https://www.arcgis.com/home/item.html?id=10df2279f9684e4a9f6a7f08febac2a9>
struct EsriWorldImagery;

impl walkers::sources::TileSource for EsriWorldImagery {
    fn tile_url(&self, tile_id: walkers::TileId) -> String {
        format!(
            "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{}/{}/{}",
            tile_id.zoom,
            tile_id.y,
            tile_id.x // ArcGIS wants zoom/row(y)/col(x), not zoom/x/y
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
    tiles: Option<walkers::HttpTiles>,
    map_memory: walkers::MapMemory,
    overlay_signal: Option<(String, String)>,
}

impl GpsPlot {
    pub fn new(instance: usize) -> Self {
        Self {
            title: format!("GPS Plot #{instance}"),
            tiles: None,
            map_memory: walkers::MapMemory::default(),
            overlay_signal: None,
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
        view: &telemetry::TelemetryView<'_>,
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
                self.overlay_signal = Some((payload.signal_id(), payload.unit().to_owned()));
                let _ = eframe::egui::DragAndDrop::take_payload::<ui::signal_browser::DraggedSignal>(
                    ui.ctx(),
                );
            }
        }

        let res = view
            .timeline
            .resolution_for_width(ui.available_width().max(1.0));
        let mut samples: Vec<_> = view.setpoint_frames.iter().filter_map(gps_sample).collect();
        if samples.is_empty() {
            let lat_series = view.query_signal_to_setpoint("gps_coordinates.latitude", res);
            let lon_series = view.query_signal_to_setpoint("gps_coordinates.longitude", res);
            match (&lat_series, &lon_series) {
                (
                    daqcore::router::SignalSeries::Exact {
                        timestamps_ms: ts,
                        values: lats,
                    },
                    daqcore::router::SignalSeries::Exact { values: lons, .. },
                ) => {
                    for ((&t_ms, &lat), &lon) in ts.iter().zip(lats.iter()).zip(lons.iter()) {
                        if lat.is_finite()
                            && lon.is_finite()
                            && lat.abs() <= 90.0
                            && lon.abs() <= 180.0
                        {
                            samples.push((daqcore::time::Time::from_unix_millis(t_ms), lat, lon));
                        }
                    }
                }
                (
                    daqcore::router::SignalSeries::Envelope(blat),
                    daqcore::router::SignalSeries::Envelope(blon),
                ) => {
                    for (lat_pt, lon_pt) in blat.iter().zip(blon.iter()) {
                        let lat = lat_pt.mean;
                        let lon = lon_pt.mean;
                        if lat.is_finite()
                            && lon.is_finite()
                            && lat.abs() <= 90.0
                            && lon.abs() <= 180.0
                        {
                            samples.push((
                                daqcore::time::Time::from_unix_millis(lat_pt.utc_ms),
                                lat,
                                lon,
                            ));
                        }
                    }
                }
                _ => {}
            }
        }

        let overlay_samples: Vec<(i64, f64)> = if let Some((sig_id, _)) = &self.overlay_signal {
            match view.query_signal_to_setpoint(sig_id, res) {
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
            Some((min_v, max_v))
        };

        let fix = samples.last().copied();
        let mut clear_overlay = false;
        ui.horizontal_wrapped(|ui| {
            if let Some((t, lat, lon)) = fix {
                ui.add(
                    eframe::egui::Label::new(format!(
                        "Last fix: {lat:.6}, {lon:.6} @ {}",
                        t.label()
                    ))
                    .wrap(),
                );
            } else {
                ui.add(
                    eframe::egui::Label::new("No retained GPS fix in the selected interval.")
                        .wrap(),
                );
            }
            if let Some((sig_id, unit)) = &self.overlay_signal {
                ui.separator();
                let unit_str = if unit.is_empty() { "" } else { unit.as_str() };
                if let Some(&(_, last_val)) = overlay_samples.last() {
                    ui.colored_label(
                        eframe::egui::Color32::from_rgb(80, 190, 255),
                        format!("Color: {sig_id} = {last_val:.2} {unit_str}"),
                    );
                } else {
                    ui.colored_label(
                        eframe::egui::Color32::from_rgb(80, 190, 255),
                        format!("Color: {sig_id}"),
                    );
                }
                if ui.small_button("× Clear").clicked() {
                    clear_overlay = true;
                }
            }
        });
        if clear_overlay {
            self.overlay_signal = None;
        }

        let position = fix
            .map(|(_, lat, lon)| walkers::lon_lat(lon, lat))
            .unwrap_or_else(|| walkers::lon_lat(DEFAULT_CENTER_LON, DEFAULT_CENTER_LAT));
        if fix.is_some() && self.map_memory.detached().is_none() {
            self.map_memory.center_at(position);
        }

        let span = view.timeline.setpoint().secs(view.timeline.start());
        let stride = samples.len().div_ceil(4096).max(1);
        let mut trail: Vec<_> = samples
            .iter()
            .step_by(stride)
            .map(|(t, lat, lon)| {
                let age_fraction = if span > 0.0 {
                    (t.secs(view.timeline.start()) / span).clamp(0.0, 1.0) as f32
                } else {
                    1.0
                };
                let norm_val = overlay_min_max.and_then(|(min_v, max_v)| {
                    let t_ms = t.unix_millis();
                    let idx = overlay_samples
                        .partition_point(|(ts, _)| *ts <= t_ms)
                        .saturating_sub(1);
                    let (_, val) = *overlay_samples.get(idx)?;
                    let denom = (max_v - min_v).max(1e-6);
                    Some(((val - min_v) / denom).clamp(0.0, 1.0) as f32)
                });

                let position = walkers::lon_lat(*lon, *lat);

                (position, age_fraction, norm_val)
            })
            .collect();
        if let Some((_, lat, lon)) = fix {
            let last_norm = trail.last().and_then(|(_, _, n)| *n);
            trail.push((walkers::lon_lat(lon, lat), 1.0, last_norm));
        }

        let tiles = self
            .tiles
            .get_or_insert_with(|| walkers::HttpTiles::new(EsriWorldImagery, ui.ctx().clone()));
        ui.add(
            walkers::Map::new(Some(tiles), &mut self.map_memory, position).with_plugin(CarDot {
                trail,
                visible: fix.is_some(),
                color: eframe::egui::Color32::BLACK,
            }),
        );
        egui_tiles::UiResponse::None
    }
}
// drawing cars path
struct CarDot {
    trail: Vec<(walkers::Position, f32, Option<f32>)>, // (position, age fraction 0.0=oldest..1.0=current, normalized signal value)
    visible: bool,
    color: eframe::egui::Color32,
}

impl walkers::Plugin for CarDot {
    fn run(
        self: Box<Self>,
        ui: &mut eframe::egui::Ui,
        response: &eframe::egui::Response,
        projector: &walkers::Projector,
        _map_memory: &walkers::MapMemory,
    ) {
        if !self.visible || self.trail.is_empty() {
            return;
        }

        let painter = ui.painter_at(response.rect);
        let screen_points: Vec<eframe::egui::Pos2> = self
            .trail
            .iter()
            .map(|(position, _, _)| projector.project(*position).to_pos2())
            .collect();

        // drawing the trail as a bunch of connected lines
        for i in 1..screen_points.len() {
            // age of the segment's newer endpoint drives its look
            let (_, age, norm_val) = self.trail[i];
            let fade = 0.15 + 0.85 * age;
            let width = 1.0 + 2.5 * age;
            let seg_color = if let Some(norm) = norm_val {
                let r = (60.0 + 195.0 * norm).round() as u8;
                let g = (150.0 - 60.0 * norm).round() as u8;
                let b = (255.0 - 205.0 * norm).round() as u8;
                eframe::egui::Color32::from_rgb(r, g, b)
            } else {
                self.color
            };

            painter.line_segment(
                [screen_points[i - 1], screen_points[i]],
                eframe::egui::Stroke::new(width, seg_color.gamma_multiply(fade)),
            );
        }

        if let Some(&current) = screen_points.last() {
            // dot for current pos
            painter.circle_filled(current, 4.0, self.color);
            painter.circle_stroke(
                current,
                4.0,
                eframe::egui::Stroke::new(1.5_f32, eframe::egui::Color32::WHITE),
            );
        }
    }
}

fn gps_sample(f: &daqcore::frame::ParsedFrame) -> Option<(daqcore::time::Time, f64, f64)> {
    let d = f.decoded.as_ref()?;
    if d.name() != "gps_coordinates" {
        return None;
    }

    let lat = d.signal("latitude")?.physical();
    let lon = d.signal("longitude")?.physical();
    if !lat.is_finite() || !lon.is_finite() || lat.abs() > 90.0 || lon.abs() > 180.0 {
        return None;
    }
    Some((f.timestamp, lat, lon))
}
