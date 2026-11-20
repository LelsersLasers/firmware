use eframe::egui::{self, Stroke};

const BUTTON_SIZE: egui::Vec2 = egui::vec2(24.0, 18.0);

pub(super) fn toolbar_button(label: &str) -> egui::Button<'_> {
    egui::Button::new(label).small().min_size(BUTTON_SIZE)
}

pub(super) fn measurements_button(ui: &mut egui::Ui, selected: bool) -> egui::Response {
    let response = ui
        .add(toolbar_button("").selected(selected))
        .on_hover_text("Measure differences");
    let center = response.rect.center();
    let color = ui.visuals().text_color();
    for dx in [-5.0, 5.0] {
        ui.painter().line_segment(
            [center + egui::vec2(dx, -5.0), center + egui::vec2(dx, 5.0)],
            Stroke::new(1.0_f32, color),
        );
        ui.painter()
            .circle_filled(center + egui::vec2(dx, -5.0), 1.7, color);
    }
    ui.painter().line_segment(
        [
            center + egui::vec2(-5.0, 0.0),
            center + egui::vec2(5.0, 0.0),
        ],
        Stroke::new(1.0_f32, color),
    );
    response
}
