use crate::{ui, workspace_view};

pub struct Jitter {
    pub title: String,
    msg_picker: ui::message_picker::ActiveMessagePickerState,
    selected_msg: Option<ui::message_picker::ActiveMessageOption>,
    period_ms: usize,
}

impl Jitter {
    pub fn new(instance: usize) -> Self {
        Self {
            title: format!("Jitter #{instance}"),
            msg_picker: ui::message_picker::ActiveMessagePickerState::default(),
            selected_msg: None,
            period_ms: 100,
        }
    }

    pub fn show(
        &mut self,
        ui: &mut eframe::egui::Ui,
        view: &workspace_view::WorkspaceDataView<'_>,
    ) -> egui_tiles::UiResponse {
        if self.selected_msg.is_none() {
            eframe::egui::ScrollArea::vertical()
                .id_salt((&self.title, "jitter_picker"))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if let Some(msg) = self.msg_picker.show(ui, view, true) {
                        self.period_ms = msg.nominal_period_ms.map_or(100, |ms| ms as usize);
                        self.selected_msg = Some(msg);
                    }
                });
            return egui_tiles::UiResponse::None;
        }

        let Some(msg) = self.selected_msg.clone() else {
            return egui_tiles::UiResponse::None;
        };

        let mut change_msg = false;
        ui.horizontal_wrapped(|ui| {
            ui.strong(format!(
                "[{}] {} (0x{:03X})",
                msg.bus_name, msg.message_name, msg.can_id
            ));
            if ui.button("Change message").clicked() {
                change_msg = true;
            }
            ui.label("Nominal period:");
            ui.add(
                eframe::egui::DragValue::new(&mut self.period_ms)
                    .range(1..=1_000_000)
                    .suffix(" ms"),
            );
        });
        if change_msg {
            self.selected_msg = None;
            return egui_tiles::UiResponse::None;
        }

        let mut deviations =
            interval_deviations(view.live_frames_to_setpoint(), msg.key(), self.period_ms);
        if deviations.is_empty()
            && let daqcore::router::SignalSeries::Exact { timestamps_ms, .. } = view
                .query_signal_to_setpoint(
                    &msg.first_signal_id,
                    daqcore::router::Resolution::FullRes,
                )
        {
            let period = self.period_ms.max(1) as f64;
            let max_interval_ms = (self.period_ms as i64 * 20).max(5_000);
            deviations = timestamps_ms
                .windows(2)
                .filter_map(|w| {
                    let elapsed_ms = w[1] - w[0];
                    (elapsed_ms >= 0 && elapsed_ms <= max_interval_ms)
                        .then(|| 100.0 * ((elapsed_ms as f64) - period) / period)
                })
                .collect();
        }

        if deviations.is_empty() {
            ui.label("No retained intervals in the selected window.");
            return egui_tiles::UiResponse::None;
        }

        let minimum = deviations.iter().copied().fold(f64::INFINITY, f64::min);
        let maximum = deviations.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let worst_error = if minimum.abs() > maximum.abs() {
            minimum
        } else {
            maximum
        };
        let average = deviations.iter().sum::<f64>() / deviations.len() as f64;
        let mean_abs_error =
            deviations.iter().map(|dev| dev.abs()).sum::<f64>() / deviations.len() as f64;
        let period = self.period_ms as f64;

        // Calculate Standard Deviation (% and raw ms)
        let variance = deviations
            .iter()
            .map(|&dev| (dev - average).powi(2))
            .sum::<f64>()
            / deviations.len() as f64;
        let std_dev_pct = variance.sqrt();
        let std_dev_ms = (std_dev_pct / 100.0) * period;

        // Calculate 99th Percentile Error
        let idx_99 = ((deviations.len() as f64 * 0.99).ceil() as usize)
            .saturating_sub(1)
            .min(deviations.len().saturating_sub(1));

        let (_, p99, _) =
            deviations.select_nth_unstable_by(idx_99, |a, b| a.abs().total_cmp(&b.abs()));

        let p99_error = *p99;

        // Calculate the raw millisecond equivalents
        let max_ms = (worst_error / 100.0) * period;
        let p99_ms = (p99_error / 100.0) * period;
        let avg_ms = (mean_abs_error / 100.0) * period;

        ui.horizontal_wrapped(|ui| {
            ui.label(format!("Intervals: {}", deviations.len()));
            ui.label("·");
            ui.label(format!("Worst: {worst_error:.2}% ({max_ms:.2} ms)"));
            ui.label("·");
            ui.label(format!("P99: {p99_error:.2}% ({p99_ms:.2} ms)"));
            ui.label("·");
            ui.label(format!("Avg: {mean_abs_error:.2}% ({avg_ms:.2} ms)"));
            ui.label("·");
            ui.label(format!("Std Dev: {std_dev_pct:.2}% ({std_dev_ms:.2} ms)"));
        });

        // Generate the Bell Curve / Histogram
        let span_ms = (((maximum - minimum) / 100.0) * period).round() as usize;
        let num_buckets = (span_ms + 1).min(200);
        let bucket_size = ((maximum - minimum) / num_buckets as f64).max(0.0001); // Prevent divide-by-zero
        let mut buckets = vec![0; num_buckets];

        // Sort the deviations into the buckets
        for &dev in &deviations {
            let bucket_idx = ((dev - minimum) / bucket_size) as usize;
            buckets[bucket_idx.min(num_buckets - 1)] += 1;
        }

        // Convert the buckets into egui_plot Bars
        let bars: Vec<egui_plot::Bar> = buckets
            .iter() // Use iter() here so we don't consume the buckets list yet
            .enumerate()
            .map(|(i, &count)| {
                let x_val = minimum + (i as f64) * bucket_size + (bucket_size / 2.0);
                egui_plot::Bar::new(x_val, count as f64).width(bucket_size * 0.9)
            })
            .collect();

        // Find the highest count in any bucket to calculate headroom
        let max_bucket_count = buckets.into_iter().max().unwrap_or(0) as f64;

        let chart =
            egui_plot::BarChart::new("Histogram", bars).color(eframe::egui::Color32::LIGHT_BLUE);

        let plot_width = ui.available_width().max(60.0);
        let plot_height = ui.available_height().max(60.0);
        // Draw the actual plot graph on the screen
        egui_plot::Plot::new(format!("jitter_histogram_{}", self.title))
            .width(plot_width)
            .height(plot_height)
            .allow_zoom(false)
            .allow_drag(false)
            .allow_scroll(false) // Prevents the mouse wheel from moving the graph
            .allow_boxed_zoom(false)
            .auto_bounds(true) // Forces the graph to auto-fit the data
            .include_y(0.0)
            .include_y(max_bucket_count * 1.1) // Adds 10% blank space above the tallest bar
            .include_x(0.0)
            .x_axis_label("Error (%)")
            .y_axis_label("Intervals")
            .show(ui, |plot_ui| plot_ui.bar_chart(chart));
        egui_tiles::UiResponse::None
    }
}

fn interval_deviations(
    frames: &[daqcore::frame::ParsedFrame],
    key: daqcore::superdbc::message::MessageKey,
    period_ms: usize,
) -> Vec<f64> {
    let timestamps: Vec<_> = frames
        .iter()
        .filter(|f| f.key() == key)
        .map(|f| f.timestamp)
        .collect();
    timestamps
        .windows(2)
        .map(|interval| {
            let elapsed_ms = interval[1].secs(interval[0]) * 1000.0;
            let deviation_ms = elapsed_ms - period_ms as f64;

            100.0 * deviation_ms / period_ms.max(1) as f64
        })
        .collect()
}
