use eframe::egui::{self, Rect, Stroke};

pub(super) fn button(ui: &mut egui::Ui, stack: bool, selected: bool) -> egui::Response {
    let response = ui
        .add(
            egui::Button::new("")
                .small()
                .selected(selected)
                .min_size(egui::vec2(24.0, 18.0)),
        )
        .on_hover_text(if stack {
            "Toggle stacked plots"
        } else {
            "Measure differences"
        });
    let center = response.rect.center();
    let color = ui.visuals().text_color();
    if stack {
        for dy in [-3.5, 3.5] {
            ui.painter().rect_stroke(
                Rect::from_center_size(center + egui::vec2(0.0, dy), egui::vec2(14.0, 5.0)),
                1.0,
                Stroke::new(1.0_f32, color),
                egui::StrokeKind::Inside,
            );
        }
    } else {
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
    }
    response
}
