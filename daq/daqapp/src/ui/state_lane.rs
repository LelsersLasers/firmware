use crate::{ui, workspace_view};

const STATE_PALETTE: [eframe::egui::Color32; 8] = [
    eframe::egui::Color32::from_rgb(65, 150, 230), // Blue
    eframe::egui::Color32::from_rgb(155, 95, 220), // Purple
    eframe::egui::Color32::from_rgb(45, 185, 185), // Teal
    eframe::egui::Color32::from_rgb(225, 130, 55), // Orange
    eframe::egui::Color32::from_rgb(120, 185, 65), // Olive Green
    eframe::egui::Color32::from_rgb(215, 90, 160), // Magenta
    eframe::egui::Color32::from_rgb(95, 125, 235), // Indigo
    eframe::egui::Color32::from_rgb(195, 165, 55), // Gold
];

pub const STANDARD_VEHICLE_STATE_LANES: &[(&str, &str)] = &[
    ("main_hb.car_state", "Car State"),
    ("main_hb_amk.precharge_state", "Precharge State"),
    ("vcu_driver_request.vcu_mode", "VCU Mode"),
    ("inva_diagnostics.amk_state", "Inverter A State"),
    ("a_box_fault_sync.IMD", "Fault: IMD"),
    ("a_box_fault_sync.PACK_OVERTEMP", "Fault: Pack Overtemp"),
];

pub const BATTERY_FAULT_STATE_LANES: &[(&str, &str)] = &[
    ("main_hb_amk.precharge_state", "Precharge State"),
    ("a_box_fault_sync.IMD", "Fault: IMD"),
    ("a_box_fault_sync.PACK_OVERTEMP", "Fault: Pack Overtemp"),
    (
        "a_box_fault_sync.CELL_UNDERVOLTAGE",
        "Fault: Cell Undervoltage",
    ),
    (
        "a_box_fault_sync.CELL_OVERVOLTAGE",
        "Fault: Cell Overvoltage",
    ),
    ("main_module_fault_sync.SDC1_IMD", "SDC1 IMD"),
];

#[derive(Debug, Clone)]
pub struct StateLaneEntry {
    pub signal_id: String,
    pub label: String,
}

pub struct StateLane {
    pub title: String,
    instance_num: usize,
    custom_title: bool,
    lanes: Vec<StateLaneEntry>,
    show_picker: bool,
    search_query: String,
}

impl StateLane {
    pub fn new(instance_num: usize) -> Self {
        let mut s = Self {
            title: format!("State Lanes #{instance_num}"),
            instance_num,
            custom_title: false,
            lanes: Vec::new(),
            show_picker: false,
            search_query: String::new(),
        };
        s.load_standard_vehicle_lanes();
        s
    }

    pub fn new_single(instance_num: usize, signal_id: String, label: String) -> Self {
        let display_label = if label.is_empty() {
            signal_id.clone()
        } else {
            label
        };
        Self {
            title: format!("State Lane: {signal_id}"),
            instance_num,
            custom_title: false,
            lanes: vec![StateLaneEntry {
                signal_id,
                label: display_label,
            }],
            show_picker: false,
            search_query: String::new(),
        }
    }

    pub fn new_with_lanes(
        instance_num: usize,
        title: impl Into<String>,
        lanes: Vec<(String, String)>,
    ) -> Self {
        Self {
            title: title.into(),
            instance_num,
            custom_title: true,
            lanes: lanes
                .into_iter()
                .map(|(signal_id, label)| StateLaneEntry { signal_id, label })
                .collect(),
            show_picker: false,
            search_query: String::new(),
        }
    }

    pub fn lanes(&self) -> &[StateLaneEntry] {
        &self.lanes
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

    pub fn load_standard_vehicle_lanes(&mut self) {
        for &(sig_id, label) in STANDARD_VEHICLE_STATE_LANES {
            if !self.lanes.iter().any(|l| l.signal_id == sig_id) {
                self.lanes.push(StateLaneEntry {
                    signal_id: sig_id.to_owned(),
                    label: label.to_owned(),
                });
            }
        }
        self.refresh_title();
    }

    pub fn add_lane(&mut self, signal_id: String, label: String) {
        if self.lanes.iter().any(|l| l.signal_id == signal_id) {
            self.show_picker = false;
            return;
        }
        let display = if label.is_empty() {
            signal_id.clone()
        } else {
            label
        };
        self.lanes.push(StateLaneEntry {
            signal_id,
            label: display,
        });
        self.show_picker = false;
        self.refresh_title();
    }

    pub fn remove_lane(&mut self, index: usize) {
        if index < self.lanes.len() {
            self.lanes.remove(index);
            if self.lanes.is_empty() {
                self.custom_title = false;
            }
            self.refresh_title();
        }
    }

    fn refresh_title(&mut self) {
        if self.custom_title && !self.lanes.is_empty() {
            return;
        }
        match self.lanes.as_slice() {
            [] => {
                self.title = format!("State Lanes #{}", self.instance_num);
            }
            [only] => {
                self.title = format!("State Lane: {}", only.signal_id);
            }
            many => {
                self.title = format!("State Lanes ({})", many.len());
            }
        }
    }

    pub fn apply_dragged_signal(&mut self, dragged: ui::signal_browser::DraggedSignal) {
        let signal_id = dragged.signal_id();
        self.add_lane(signal_id.clone(), signal_id);
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
        let mut load_std = false;
        let mut load_bms = false;
        let mut clear_all = false;
        ui.horizontal_wrapped(|ui| {
            if ui
                .small_button(if self.show_picker {
                    "⬅ Back to Lanes"
                } else {
                    "+ State / Fault"
                })
                .on_hover_text("Add a discrete state or 1-bit fault lane")
                .clicked()
            {
                self.show_picker = !self.show_picker;
            }
            ui.menu_button("…", |ui| {
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
                    if resp.lost_focus() && self.title.trim().is_empty() {
                        self.custom_title = false;
                        self.refresh_title();
                    }
                    if self.custom_title
                        && ui
                            .small_button("Auto")
                            .on_hover_text("Reset to automatic lane title")
                            .clicked()
                    {
                        self.custom_title = false;
                        self.refresh_title();
                    }
                });
                ui.separator();
                ui.label(eframe::egui::RichText::new("Lane Presets").weak().small());
                if ui.button("Load Vehicle States").clicked() {
                    load_std = true;
                    ui.close();
                }
                if ui.button("Load BMS / SDC Faults").clicked() {
                    load_bms = true;
                    ui.close();
                }
                if !self.lanes.is_empty() {
                    ui.separator();
                    if ui.button("Clear All Lanes").clicked() {
                        clear_all = true;
                        ui.close();
                    }
                }
            });
        });

        if load_std {
            self.load_standard_vehicle_lanes();
            self.show_picker = false;
        }
        if load_bms {
            for &(sig_id, label) in BATTERY_FAULT_STATE_LANES {
                if !self.lanes.iter().any(|l| l.signal_id == sig_id) {
                    self.lanes.push(StateLaneEntry {
                        signal_id: sig_id.to_owned(),
                        label: label.to_owned(),
                    });
                }
            }
            self.refresh_title();
            self.show_picker = false;
        }
        if clear_all {
            self.lanes.clear();
            self.custom_title = false;
            self.refresh_title();
            self.show_picker = true;
        }

        if self.show_picker || self.lanes.is_empty() {
            self.show_signal_picker(ui, view);
            return egui_tiles::UiResponse::None;
        }

        let start = view.timeline.start();
        let start_ms = start.unix_millis();
        let end_ms = view.timeline.end_utc_ms().max(start_ms + 1);
        let span_ms = (end_ms - start_ms).max(1) as f64;
        let cursor_ms = view.active_cursor_utc_ms();
        let setpoint_ms = view.timeline.setpoint_utc_ms();
        let hover_ms = view.hover_utc_ms();

        let mut remove_idx: Option<usize> = None;
        let avail_w = ui.available_width().max(80.0);
        let label_col_w = (avail_w * 0.30).clamp(110.0, 225.0);
        let row_h = 22.0_f32;

        eframe::egui::ScrollArea::vertical()
            .id_salt(("state_lane_scroll", self.instance_num))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (idx, lane) in self.lanes.iter().enumerate() {
                    let is_fault_sig = is_likely_fault_signal(&lane.signal_id);
                    let intervals = view.query_states(&lane.signal_id, start_ms..=end_ms);
                    let active_interval = intervals
                        .iter()
                        .rfind(|iv| iv.start_utc_ms <= cursor_ms && cursor_ms <= iv.end_utc_ms)
                        .or_else(|| intervals.iter().rfind(|iv| iv.start_utc_ms <= cursor_ms));

                    ui.horizontal(|ui| {
                        let (label_rect, _) = ui.allocate_exact_size(
                            eframe::egui::vec2(label_col_w, row_h),
                            eframe::egui::Sense::hover(),
                        );
                        let mut label_ui = ui.new_child(
                            eframe::egui::UiBuilder::new().max_rect(label_rect).layout(
                                eframe::egui::Layout::left_to_right(eframe::egui::Align::Center),
                            ),
                        );
                        label_ui.set_clip_rect(label_rect.intersect(ui.clip_rect()));
                        if label_ui
                            .small_button("×")
                            .on_hover_text(format!("Remove {}", lane.signal_id))
                            .clicked()
                        {
                            remove_idx = Some(idx);
                        }
                        let current_badge = active_interval.map(|iv| {
                            let resolved = resolve_state_label(&lane.signal_id, iv, is_fault_sig);
                            let color =
                                state_color_for_interval(&resolved, iv.raw_int, is_fault_sig);
                            (resolved, color)
                        });
                        let title_response = label_ui.add(
                            eframe::egui::Label::new(
                                eframe::egui::RichText::new(&lane.signal_id)
                                    .strong()
                                    .small(),
                            )
                            .truncate(),
                        );
                        let label_note = if !lane.label.is_empty() && lane.label != lane.signal_id {
                            format!(" ({})", lane.label)
                        } else {
                            String::new()
                        };
                        if let Some((badge_txt, badge_col)) = &current_badge {
                            title_response.on_hover_text(format!(
                                "{}{}\nCursor State: {}",
                                lane.signal_id, label_note, badge_txt
                            ));
                            label_ui.colored_label(
                                *badge_col,
                                eframe::egui::RichText::new(format!("[{badge_txt}]")).small(),
                            );
                        } else {
                            title_response
                                .on_hover_text(format!("{}{}", lane.signal_id, label_note));
                        }

                        let strip_w = ui.available_width().max(40.0);
                        let (strip_rect, strip_resp) = ui.allocate_exact_size(
                            eframe::egui::vec2(strip_w, row_h),
                            eframe::egui::Sense::click_and_drag(),
                        );
                        let painter = ui.painter_at(strip_rect);
                        painter.rect_filled(
                            strip_rect,
                            3.0,
                            eframe::egui::Color32::from_rgba_unmultiplied(28, 32, 40, 210),
                        );

                        let time_to_x = |t_ms: i64| -> f32 {
                            let frac = ((t_ms - start_ms) as f64 / span_ms).clamp(0.0, 1.0) as f32;
                            strip_rect.left() + frac * strip_rect.width()
                        };

                        for iv in &intervals {
                            let x0 = time_to_x(iv.start_utc_ms);
                            let x1 = time_to_x(iv.end_utc_ms)
                                .max(x0 + 2.0)
                                .min(strip_rect.right());
                            let bar_rect = eframe::egui::Rect::from_min_max(
                                eframe::egui::pos2(x0, strip_rect.top() + 2.0),
                                eframe::egui::pos2(x1, strip_rect.bottom() - 2.0),
                            );
                            let resolved = resolve_state_label(&lane.signal_id, iv, is_fault_sig);
                            let fill =
                                state_color_for_interval(&resolved, iv.raw_int, is_fault_sig);
                            painter.rect_filled(bar_rect, 2.5, fill.gamma_multiply(0.82));
                            painter.rect_stroke(
                                bar_rect,
                                2.5,
                                eframe::egui::Stroke::new(
                                    0.8_f32,
                                    eframe::egui::Color32::from_rgba_unmultiplied(
                                        255, 255, 255, 70,
                                    ),
                                ),
                                eframe::egui::StrokeKind::Inside,
                            );

                            if bar_rect.width() >= 34.0 {
                                let text_clip = bar_rect.shrink2(eframe::egui::vec2(3.0, 1.0));
                                painter
                                    .with_clip_rect(text_clip.intersect(strip_rect))
                                    .text(
                                        text_clip.left_center(),
                                        eframe::egui::Align2::LEFT_CENTER,
                                        &resolved,
                                        eframe::egui::FontId::proportional(11.0),
                                        eframe::egui::Color32::WHITE,
                                    );
                            }
                        }

                        // Playhead line (Yellow)
                        if setpoint_ms >= start_ms && setpoint_ms <= end_ms {
                            let px = time_to_x(setpoint_ms);
                            painter.line_segment(
                                [
                                    eframe::egui::pos2(px, strip_rect.top()),
                                    eframe::egui::pos2(px, strip_rect.bottom()),
                                ],
                                eframe::egui::Stroke::new(1.6_f32, eframe::egui::Color32::YELLOW),
                            );
                        }

                        // Synchronized Hover Crosshair line (Cyan)
                        if let Some(h_ms) = hover_ms
                            && h_ms >= start_ms
                            && h_ms <= end_ms
                        {
                            let hx = time_to_x(h_ms);
                            painter.line_segment(
                                [
                                    eframe::egui::pos2(hx, strip_rect.top()),
                                    eframe::egui::pos2(hx, strip_rect.bottom()),
                                ],
                                eframe::egui::Stroke::new(
                                    1.2_f32,
                                    eframe::egui::Color32::from_rgb(130, 225, 255),
                                ),
                            );
                        }

                        if strip_resp.dragged_by(eframe::egui::PointerButton::Primary) {
                            let drag_dx = strip_resp.drag_delta().x;
                            if drag_dx.abs() > 0.0 {
                                let delta_ms = (-(drag_dx as f64)
                                    / (strip_rect.width().max(1.0) as f64)
                                    * span_ms)
                                    .round() as i64;
                                view.request_pan_delta_ms(delta_ms);
                            }
                        }

                        if let Some(pos) = strip_resp.hover_pos() {
                            let frac = ((pos.x - strip_rect.left()) / strip_rect.width().max(1.0))
                                .clamp(0.0, 1.0) as f64;
                            let ptr_ms = start_ms + (frac * span_ms).round() as i64;
                            view.set_hover_utc_ms(Some(ptr_ms));
                            if strip_resp.clicked() {
                                view.request_setpoint_utc_ms(ptr_ms);
                            }

                            let (scroll, zoom_delta) =
                                ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta()));
                            if (zoom_delta - 1.0).abs() > 1e-3 {
                                let factor = (1.0 / f64::from(zoom_delta)).clamp(0.25, 4.0);
                                view.request_zoom_around_ms(ptr_ms, factor);
                            } else if scroll.y.abs() > 0.5 {
                                let factor = if scroll.y > 0.0 { 0.8 } else { 1.25 };
                                view.request_zoom_around_ms(ptr_ms, factor);
                            }
                            if scroll.x.abs() > 0.5 {
                                let delta_ms = (-(scroll.x as f64)
                                    / (strip_rect.width().max(1.0) as f64)
                                    * span_ms)
                                    .round() as i64;
                                view.request_pan_delta_ms(delta_ms);
                            }

                            if let Some(iv) = intervals
                                .iter()
                                .rfind(|iv| iv.start_utc_ms <= ptr_ms && ptr_ms <= iv.end_utc_ms)
                            {
                                let resolved =
                                    resolve_state_label(&lane.signal_id, iv, is_fault_sig);
                                let dur_s =
                                    ((iv.end_utc_ms - iv.start_utc_ms).max(0) as f64) / 1000.0;
                                strip_resp.on_hover_text(format!(
                                    "{} ({})\nState: {} (raw = {})\nDuration: {:.2}s\nAt: {}",
                                    lane.label,
                                    lane.signal_id,
                                    resolved,
                                    iv.raw_int,
                                    dur_s,
                                    daqcore::time::Time::from_unix_millis(ptr_ms).label()
                                ));
                            } else {
                                strip_resp.on_hover_text(format!(
                                    "{} ({})\nAt: {}",
                                    lane.label,
                                    lane.signal_id,
                                    daqcore::time::Time::from_unix_millis(ptr_ms).label()
                                ));
                            }
                        }
                    });
                    ui.add_space(2.0);
                }

                // Bottom time ruler aligned with the strips
                ui.horizontal(|ui| {
                    ui.allocate_exact_size(
                        eframe::egui::vec2(label_col_w, 18.0),
                        eframe::egui::Sense::hover(),
                    );
                    let strip_w = ui.available_width().max(40.0);
                    let (ruler_rect, _) = ui.allocate_exact_size(
                        eframe::egui::vec2(strip_w, 18.0),
                        eframe::egui::Sense::hover(),
                    );
                    let painter = ui.painter_at(ruler_rect);
                    let tick_count = ((strip_w / 140.0).floor() as usize).clamp(2, 8);
                    for i in 0..=tick_count {
                        let frac = (i as f32) / (tick_count as f32);
                        let x = ruler_rect.left() + frac * ruler_rect.width();
                        let t_ms = start_ms + ((frac as f64) * span_ms).round() as i64;
                        let align = if i == 0 {
                            eframe::egui::Align2::LEFT_TOP
                        } else if i == tick_count {
                            eframe::egui::Align2::RIGHT_TOP
                        } else {
                            eframe::egui::Align2::CENTER_TOP
                        };
                        painter.text(
                            eframe::egui::pos2(x, ruler_rect.top() + 2.0),
                            align,
                            daqcore::time::Time::from_unix_millis(t_ms).label(),
                            eframe::egui::FontId::monospace(10.0),
                            ui.visuals().weak_text_color(),
                        );
                    }
                });
            });

        if let Some(idx) = remove_idx {
            self.remove_lane(idx);
        }

        egui_tiles::UiResponse::None
    }

    fn show_signal_picker(
        &mut self,
        ui: &mut eframe::egui::Ui,
        view: &workspace_view::WorkspaceDataView<'_>,
    ) {
        let allow_cancel = !self.lanes.is_empty();
        match ui::message_picker::show_signal_picker(
            ui,
            ("state_lane_picker_scroll", self.instance_num),
            "Search state / fault signal:",
            allow_cancel,
            true,
            &mut self.search_query,
            view,
        ) {
            ui::message_picker::SignalPickerAction::None => {}
            ui::message_picker::SignalPickerAction::Cancelled => {
                self.show_picker = false;
            }
            ui::message_picker::SignalPickerAction::Picked(picked) => {
                self.add_lane(picked.signal_id.clone(), picked.signal_id);
            }
        }
    }
}

pub fn is_likely_fault_signal(signal_id: &str) -> bool {
    let lower = signal_id.to_lowercase();
    lower.contains("fault")
        || lower.contains("error")
        || lower.contains("berror")
        || lower.contains("overtemp")
        || lower.contains("undervoltage")
        || lower.contains("overvoltage")
        || lower.contains("imd")
        || lower.contains("sdc")
}

pub fn resolve_state_label(
    _signal_id: &str,
    interval: &daqcore::router::StateInterval,
    is_fault_sig: bool,
) -> String {
    if !interval.label.is_empty() && !interval.label.starts_with("RAW_") {
        return interval.label.clone();
    }
    if is_fault_sig {
        if interval.raw_int == 0 {
            "OK (0)".to_owned()
        } else {
            format!("FAULT ({})", interval.raw_int)
        }
    } else if interval.raw_int == 0 {
        "0".to_owned()
    } else if interval.raw_int == 1 {
        "1".to_owned()
    } else {
        format!("{}", interval.raw_int)
    }
}

pub fn state_color_for_interval(
    label: &str,
    raw_int: i64,
    is_fault_sig: bool,
) -> eframe::egui::Color32 {
    let upper = label.to_uppercase();
    if is_fault_sig {
        return if raw_int == 0 {
            eframe::egui::Color32::from_rgb(50, 175, 100)
        } else {
            eframe::egui::Color32::from_rgb(225, 65, 65)
        };
    }
    if upper.contains("FAULT")
        || upper.contains("ERROR")
        || upper.contains("TRIP")
        || upper.contains("LATCH")
        || upper.contains("OVERTEMP")
        || upper.contains("UNDERVOLT")
        || upper.contains("OVERVOLT")
    {
        return eframe::egui::Color32::from_rgb(225, 65, 65);
    }
    if upper.contains("READY")
        || upper.contains("DRIVING")
        || upper.contains("ACTIVE")
        || upper.contains("ON")
        || upper.contains("ENABLED")
        || upper.contains("CLOSED")
        || upper.contains("OK")
    {
        return eframe::egui::Color32::from_rgb(50, 180, 105);
    }
    if upper.contains("PRECHARGE")
        || upper.contains("INIT")
        || upper.contains("WAIT")
        || upper.contains("STANDBY")
    {
        return eframe::egui::Color32::from_rgb(235, 165, 45);
    }
    if raw_int == 0 || upper == "0" || upper.contains("OFF") || upper.contains("IDLE") {
        return eframe::egui::Color32::from_rgb(75, 95, 120);
    }
    STATE_PALETTE[(raw_int.unsigned_abs() as usize) % STATE_PALETTE.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_lane_resolves_fault_and_enum_colors_and_renders_cleanly() {
        let fault_iv = daqcore::router::StateInterval {
            start_utc_ms: 1_000,
            end_utc_ms: 2_000,
            raw_int: 1,
            label: "RAW_1".to_owned(),
        };
        let label = resolve_state_label("a_box_fault_sync.IMD", &fault_iv, true);
        assert_eq!(label, "FAULT (1)");
        let col = state_color_for_interval(&label, 1, true);
        assert_eq!(col, eframe::egui::Color32::from_rgb(225, 65, 65));

        let t0 = daqcore::time::Time::from_unix_millis(1_794_500_000_000);
        let router = daqcore::router::VirtualTimelineRouter::live(t0, 30.0).expect("live router");
        let mut lane_widget = StateLane::new(1);
        assert!(!lane_widget.lanes().is_empty());

        let custom = StateLane::new_with_lanes(
            2,
            "State Lanes: BMS Faults",
            vec![("a_box_fault_sync.IMD".to_owned(), "Fault: IMD".to_owned())],
        );
        assert_eq!(custom.lanes().len(), 1);
        let ctor = crate::widget_constructor::WidgetConstructor::StateLaneSingle {
            signal_id: "main_hb.car_state".to_owned(),
            label: "Car State".to_owned(),
        };
        assert_eq!(
            ctor.kind(),
            crate::widget_constructor::WidgetKind::StateLane
        );

        let ctx = eframe::egui::Context::default();
        ui::theme::store_theme(&ctx, ui::theme::ThemeSelection::default().get_colors());
        let view = workspace_view::WorkspaceDataView::with_hover(
            &router,
            Some(t0.unix_millis() - 5_000),
            None,
        );
        let raw_input = eframe::egui::RawInput {
            screen_rect: Some(eframe::egui::Rect::from_min_size(
                eframe::egui::Pos2::ZERO,
                eframe::egui::vec2(900.0, 320.0),
            )),
            ..Default::default()
        };
        let _ = ctx.run(raw_input, |ctx| {
            eframe::egui::CentralPanel::default().show(ctx, |ui| {
                let _ = lane_widget.show(ui, &view);
            });
        });
    }
}
