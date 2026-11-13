use crate::ui;

pub const NUM_MODULES: usize = 5;
pub const CELLS_PER_MODULE: usize = 29;
pub const THERMISTORS_PER_MODULE: usize = 20;

pub const STALE_TIMEOUT_SECONDS: u64 = 1;

pub fn sample_age(
    last_update: Option<daqcore::time::Time>,
    view_time: daqcore::time::Time,
) -> (bool, f64) {
    let elapsed = last_update
        .map(|t| view_time.secs(t).max(0.0))
        .unwrap_or(f64::INFINITY);
    (elapsed > STALE_TIMEOUT_SECONDS as f64, elapsed)
}

pub fn stale_banner(
    ui: &mut eframe::egui::Ui,
    theme: &ui::theme::ThemeColors,
    stale: bool,
    elapsed: f64,
) {
    let (bg, dot, text) = if stale {
        let c = theme.warning_color();
        (
            eframe::egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 30),
            c,
            format!("No data — last message {:.1} s ago", elapsed),
        )
    } else {
        let c = theme.success_color();
        (
            eframe::egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 30),
            c,
            format!("Live — last message {:.1} s ago", elapsed),
        )
    };

    eframe::egui::Frame::NONE
        .fill(bg)
        .stroke(eframe::egui::Stroke::new(1.0_f32, dot.linear_multiply(0.5)))
        .inner_margin(eframe::egui::Margin::symmetric(10, 6))
        .corner_radius(eframe::egui::CornerRadius::same(4))
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                let (rect, _) = ui.allocate_exact_size(
                    eframe::egui::Vec2::splat(8.0),
                    eframe::egui::Sense::hover(),
                );
                ui.painter().circle_filled(rect.center(), 4.0, dot);
                ui.add_space(4.0);
                ui.add(
                    eframe::egui::Label::new(eframe::egui::RichText::new(&text).color(dot)).wrap(),
                );
            });
        });
}

pub struct StatCardSpec<'a> {
    pub label: &'a str,
    pub value: Option<f64>,
    pub unit: &'a str,
    pub override_color: Option<eframe::egui::Color32>,
}

pub fn stat_cards_grid(
    ui: &mut eframe::egui::Ui,
    theme: &ui::theme::ThemeColors,
    stale: bool,
    cards: &[StatCardSpec<'_>],
) {
    if cards.is_empty() {
        return;
    }
    let avail_w = ui.available_width().max(80.0);
    let cols_per_row = ((avail_w / 92.0).floor() as usize).clamp(1, cards.len());
    for (row_idx, row_cards) in cards.chunks(cols_per_row).enumerate() {
        if row_idx > 0 {
            ui.add_space(4.0);
        }
        ui.columns(row_cards.len(), |cols| {
            for (col_ui, spec) in cols.iter_mut().zip(row_cards.iter()) {
                stat_card(
                    col_ui,
                    theme,
                    spec.label,
                    spec.value,
                    spec.unit,
                    stale,
                    spec.override_color,
                );
            }
        });
    }
}

pub fn stat_card(
    ui: &mut eframe::egui::Ui,
    theme: &ui::theme::ThemeColors,
    label: &str,
    value: Option<f64>,
    unit: &str,
    stale: bool,
    override_color: Option<eframe::egui::Color32>,
) {
    let compact = ui.available_width() < 110.0;
    let margin = if compact { 6 } else { 10 };
    let value_font_size = if compact { 15.0 } else { 20.0 };

    eframe::egui::Frame::NONE
        .fill(theme.panel_color())
        .stroke(eframe::egui::Stroke::new(1.0_f32, theme.accent_color()))
        .inner_margin(eframe::egui::Margin::same(margin))
        .corner_radius(eframe::egui::CornerRadius::same(4))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.vertical(|ui| {
                ui.add(
                    eframe::egui::Label::new(
                        eframe::egui::RichText::new(label)
                            .size(10.0)
                            .color(theme.text_color().linear_multiply(0.5)),
                    )
                    .truncate(),
                );
                ui.add_space(2.0);
                let val_color = override_color.unwrap_or(theme.info_color());
                if stale {
                    ui.label(
                        eframe::egui::RichText::new("—")
                            .size(value_font_size)
                            .color(theme.text_color().linear_multiply(0.25)),
                    );
                } else {
                    let text = if let Some(value) = value.filter(|value| value.is_finite()) {
                        format!("{value:.2}")
                    } else {
                        "—".to_string()
                    };

                    ui.add(
                        eframe::egui::Label::new(
                            eframe::egui::RichText::new(text)
                                .size(value_font_size)
                                .color(val_color),
                        )
                        .truncate(),
                    );
                    ui.label(
                        eframe::egui::RichText::new(unit)
                            .size(10.0)
                            .color(theme.text_color().linear_multiply(0.4)),
                    );
                }
            });
        });
}

/// Keep unavailable projection values distinct from a measured zero.
pub fn reading(value: f64, precision: usize) -> String {
    if value.is_finite() {
        format!("{value:.precision$}")
    } else {
        "—".into()
    }
}
