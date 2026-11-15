use crate::workspace_view;
use std::collections::BTreeMap;

const BUS_COLORS: [eframe::egui::Color32; 6] = [
    eframe::egui::Color32::from_rgb(66, 165, 245),  // Blue
    eframe::egui::Color32::from_rgb(102, 187, 106), // Green
    eframe::egui::Color32::from_rgb(255, 167, 38),  // Orange
    eframe::egui::Color32::from_rgb(171, 71, 188),  // Purple
    eframe::egui::Color32::from_rgb(239, 83, 80),   // Red
    eframe::egui::Color32::from_rgb(38, 198, 218),  // Cyan
];

fn color_for_bus(bus_id: u8) -> eframe::egui::Color32 {
    BUS_COLORS[usize::from(bus_id) % BUS_COLORS.len()]
}

fn max_bus_load(samples: &[workspace_view::BusLoadSample], bus_id: u8, window_idx: usize) -> f32 {
    samples
        .iter()
        .flat_map(|sample| &sample.buses)
        .filter(|bus| bus.bus_id == bus_id)
        .map(|bus| bus.values[window_idx])
        .fold(0.0, f32::max)
}

fn show_load_row(
    ui: &mut eframe::egui::Ui,
    samples: &[workspace_view::BusLoadSample],
    reading: &daqcore::can::bus_load::BusLoadReading,
) {
    let color = color_for_bus(reading.bus_id);
    ui.colored_label(color, format!("■ {}", reading.bus_name));
    for (win_idx, &current) in reading.values.iter().enumerate() {
        let maximum = max_bus_load(samples, reading.bus_id, win_idx);
        ui.label(format!("{current:.2}% / {maximum:.2}%"));
    }
    ui.end_row();
}

pub struct BusLoad {
    pub title: String,
}

impl BusLoad {
    pub fn new(instance: usize) -> Self {
        Self {
            title: format!("Bus Load #{instance}"),
        }
    }

    pub fn show(
        &self,
        ui: &mut eframe::egui::Ui,
        samples: &[workspace_view::BusLoadSample],
        view: &workspace_view::WorkspaceDataView<'_>,
    ) -> egui_tiles::UiResponse {
        let start_time = view.timeline.start();
        let end_time = view.timeline.end();
        let duration = end_time.secs(start_time).max(0.001);
        let playhead = view.timeline.setpoint().secs(start_time);

        let start_index = samples.partition_point(|sample| sample.timestamp < start_time);
        let end_index = samples.partition_point(|sample| sample.timestamp <= end_time);
        let window_samples = &samples[start_index..end_index];

        let mut bus_series: BTreeMap<u8, (&str, Vec<[f64; 2]>)> = BTreeMap::new();
        for sample in window_samples {
            let t = sample.timestamp.secs(start_time);
            for reading in &sample.buses {
                bus_series
                    .entry(reading.bus_id)
                    .or_insert_with(|| (reading.bus_name.as_str(), Vec::new()))
                    .1
                    .push([t, reading.values[0] as f64]);
            }
        }

        let pixels = ui.available_width().max(1.0) as usize;
        let decimated_series: Vec<(&str, eframe::egui::Color32, Vec<[f64; 2]>)> = bus_series
            .into_iter()
            .map(|(bus_id, (bus_name, points))| {
                (
                    bus_name,
                    color_for_bus(bus_id),
                    workspace_view::decimate(&points, pixels),
                )
            })
            .collect();

        let cursor =
            window_samples.partition_point(|sample| sample.timestamp <= view.timeline.setpoint());
        let upto_cursor = &window_samples[..cursor];

        let max_table_h = (ui.available_height() * 0.42).max(44.0);
        eframe::egui::TopBottomPanel::bottom(eframe::egui::Id::new((&self.title, "loads_panel")))
            .frame(eframe::egui::Frame::NONE)
            .show_separator_line(false)
            .max_height(max_table_h)
            .show_inside(ui, |ui| {
                ui.add_space(4.0);
                eframe::egui::ScrollArea::both()
                    .id_salt((&self.title, "loads_scroll"))
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        self.show_load_table(ui, upto_cursor);
                    });
            });

        if !decimated_series.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.label(eframe::egui::RichText::new("Key:").strong());
                for (bus_name, color, _) in &decimated_series {
                    ui.colored_label(*color, format!("■ {bus_name}"));
                    ui.add_space(8.0);
                }
            });
        }

        let plot_width = ui.available_width().max(60.0);
        let plot_height = ui.available_height().max(60.0);
        let start_ms = start_time.unix_millis();
        let end_ms = end_time.unix_millis();
        let span_ms = (end_ms - start_ms).max(daqcore::timeline::MIN_WINDOW_MS) as f64;
        egui_plot::Plot::new(&self.title)
            .width(plot_width)
            .height(plot_height)
            .allow_zoom(false)
            .allow_drag(false)
            .allow_scroll(false)
            .allow_boxed_zoom(false)
            .allow_double_click_reset(false)
            .x_axis_formatter(|mark, _| {
                let offset_ms = (mark.value * 1000.0).round() as i64;
                start_time.offset(offset_ms).label()
            })
            .x_axis_label("Time")
            .y_axis_label("Bus Load (%)")
            .show(ui, |plot| {
                plot.set_plot_bounds_x(0.0..=duration);
                for (bus_name, color, points) in decimated_series {
                    plot.line(egui_plot::Line::new(bus_name, points).color(color));
                }
                plot.vline(
                    egui_plot::VLine::new("Playhead", playhead)
                        .color(eframe::egui::Color32::YELLOW),
                );
                let mut anchor_ms = view.timeline.setpoint_utc_ms();
                if plot.response().hovered()
                    && let Some(coord) = plot.pointer_coordinate()
                {
                    let clamped_x = coord.x.clamp(0.0, duration);
                    let utc_ms = start_ms + (clamped_x * 1000.0).round() as i64;
                    anchor_ms = utc_ms;
                    view.set_hover_utc_ms(Some(utc_ms));
                    if plot.response().clicked() {
                        view.request_setpoint_utc_ms(utc_ms);
                    }
                }
                if plot.response().hovered() {
                    let (scroll, zoom_delta) = plot
                        .ctx()
                        .input(|i| (i.smooth_scroll_delta, i.zoom_delta()));
                    if (zoom_delta - 1.0).abs() > 1e-3 {
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
            });
        egui_tiles::UiResponse::None
    }

    fn show_load_table(
        &self,
        ui: &mut eframe::egui::Ui,
        samples: &[workspace_view::BusLoadSample],
    ) {
        let Some(last) = samples.last() else {
            ui.label("No bus-load samples in the selected interval.");
            return;
        };

        eframe::egui::Grid::new((&self.title, "loads"))
            .striped(true)
            .spacing([16.0, 4.0])
            .show(ui, |ui| {
                ui.label(eframe::egui::RichText::new("Bus").strong());
                for seconds in daqcore::can::bus_load::WINDOWS_SECS {
                    ui.label(
                        eframe::egui::RichText::new(format!("{seconds}s (Cur / Max)")).strong(),
                    );
                }
                ui.end_row();

                for reading in &last.buses {
                    show_load_row(ui, samples, reading);
                }
            });
    }
}
