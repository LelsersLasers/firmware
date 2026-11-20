use eframe::egui::{self, Rect, Stroke};

const BUTTON_SIZE: egui::Vec2 = egui::vec2(24.0, 18.0);

pub(super) fn toolbar_button(label: &str) -> egui::Button<'_> {
    egui::Button::new(label).small().min_size(BUTTON_SIZE)
}

pub(super) fn delete_button(ui: &mut egui::Ui, armed: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(BUTTON_SIZE, egui::Sense::click());
    let label = if armed {
        "Confirm deletion"
    } else {
        "Delete plot"
    };
    let color = if armed {
        egui::Color32::from_rgb(70, 200, 110)
    } else if response.hovered() || response.has_focus() {
        egui::Color32::from_rgb(235, 75, 75)
    } else {
        ui.visuals().weak_text_color()
    };
    let visuals = ui.style().interact(&response);
    ui.painter().rect(
        rect.shrink(0.5),
        3.0,
        visuals.bg_fill,
        Stroke::new(1.0_f32, color),
        egui::StrokeKind::Inside,
    );
    if armed {
        let center = rect.center();
        ui.painter().line_segment(
            [
                center + egui::vec2(-5.0, 0.0),
                center + egui::vec2(-1.0, 4.0),
            ],
            Stroke::new(2.0_f32, color),
        );
        ui.painter().line_segment(
            [
                center + egui::vec2(-1.0, 4.0),
                center + egui::vec2(6.0, -4.0),
            ],
            Stroke::new(2.0_f32, color),
        );
    } else {
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "×",
            egui::FontId::proportional(16.0),
            color,
        );
    }
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(label)
}

pub(super) fn button(ui: &mut egui::Ui, stack: bool, selected: bool) -> egui::Response {
    let response = ui
        .add(toolbar_button("").selected(selected))
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
