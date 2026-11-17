//! A bounded overlay that does not participate in plot layout or navigation.
use crate::ui::{
    self,
    scope::{ChannelStats, ScopeChannel},
};
use eframe::egui::{self, Color32, Rect, Stroke};

#[derive(Clone)]
pub(super) struct Entry {
    pub channel_index: usize,
    pub label: String,
    pub signal_id: String,
    pub value: Option<String>,
    pub color: Color32,
    pub style: egui_plot::LineStyle,
    pub stats: Option<ChannelStats>,
    pub visible: bool,
    pub measurements: Option<(f64, f64)>,
}

#[derive(Clone)]
pub(super) struct SignalDrag {
    pub scope: usize,
    pub signal: String,
}

pub(super) enum Action {
    Remove(usize),
    Toggle(usize),
    Inspect(usize, ui::scope_settings::InspectorTab),
}

pub(super) fn toggle_button(ui: &mut egui::Ui, visible: bool) -> egui::Response {
    let response = ui
        .add(
            egui::Button::new("")
                .small()
                .selected(visible)
                .min_size(egui::vec2(24.0, 18.0)),
        )
        .on_hover_text("Toggle legend");
    let center = response.rect.center();
    let text_color = ui::theme::get_theme(ui.ctx()).text_color();
    for (index, dy) in [-4.0, 0.0, 4.0].into_iter().enumerate() {
        ui.painter().rect_filled(
            Rect::from_center_size(center + egui::vec2(-6.0, dy), egui::vec2(3.0, 2.0)),
            0.0,
            ui::scope::CHANNEL_PALETTE[index],
        );
        ui.painter().line_segment(
            [center + egui::vec2(-1.0, dy), center + egui::vec2(7.0, dy)],
            Stroke::new(1.0_f32, text_color),
        );
    }
    response
}

pub(super) fn short_label(channel: &ScopeChannel) -> &str {
    if !channel.label.is_empty() && channel.label != channel.signal_id {
        &channel.label
    } else {
        channel
            .signal_id
            .rsplit('.')
            .next()
            .unwrap_or(&channel.signal_id)
    }
}

pub(super) fn bounds(plot: Rect, rows: usize, statistics: bool) -> Option<Rect> {
    if rows == 0 || plot.width() < 96.0 || plot.height() < 48.0 {
        return None;
    }
    let width = (plot.width() * 0.45)
        .clamp(160.0, 320.0)
        .min(plot.width() - 16.0);
    let row_height = if statistics { 40.0 } else { 22.0 };
    let height = (rows as f32 * row_height + 12.0)
        .min((plot.height() * 0.45).clamp(32.0, 180.0))
        .min(plot.height() - 16.0);
    Some(Rect::from_min_size(
        plot.left_bottom() + egui::vec2(8.0, -height - 8.0),
        egui::vec2(width, height),
    ))
}

pub(super) fn show(
    ui: &mut egui::Ui,
    rect: Rect,
    instance: usize,
    entries: &[Entry],
    statistics: bool,
    scope_instance: usize,
) -> Option<Action> {
    let theme = ui::theme::get_theme(ui.ctx());
    let mut action = None;
    let clip = ui.clip_rect().intersect(rect);
    egui::Area::new(egui::Id::new(("scope_legend", instance)))
        .order(egui::Order::Middle)
        .pivot(egui::Align2::LEFT_BOTTOM)
        .default_size(rect.size())
        .fixed_pos(rect.left_bottom())
        .movable(false)
        .constrain(false)
        .show(ui.ctx(), |overlay| {
            overlay.set_width(rect.width());
            overlay.set_clip_rect(clip);
            egui::Frame::new()
                .inner_margin(6)
                .corner_radius(4.0)
                .fill(theme.panel_color().gamma_multiply(0.88))
                .stroke(Stroke::new(
                    1.0_f32,
                    theme.border_color().gamma_multiply(0.8),
                ))
                .show(overlay, |ui| {
                    ui.set_width((rect.width() - 14.0).max(1.0));
                    egui::ScrollArea::vertical()
                        .id_salt(("scope_legend_scroll", instance))
                        .max_height((rect.height() - 14.0).max(1.0))
                        .min_scrolled_height(0.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            for (index, entry) in entries.iter().enumerate() {
                                let (row, response) = ui.allocate_exact_size(
                                    egui::vec2(ui.available_width(), 20.0),
                                    egui::Sense::click(),
                                );
                                if response.hovered() {
                                    ui.painter().rect_filled(
                                        row,
                                        2.0,
                                        ui.visuals().widgets.hovered.weak_bg_fill,
                                    );
                                }
                                let text_color = if entry.visible {
                                    theme.text_color()
                                } else {
                                    theme.muted_text_color().gamma_multiply(0.6)
                                };
                                let eye_rect = Rect::from_min_size(
                                    egui::pos2(row.right() - 22.0, row.top()),
                                    egui::vec2(22.0, row.height()),
                                );
                                let eye_response = ui
                                    .interact(
                                        eye_rect,
                                        ui.id().with(("signal_visibility", index)),
                                        egui::Sense::click(),
                                    )
                                    .on_hover_text(if entry.visible {
                                        "Hide signal"
                                    } else {
                                        "Show signal"
                                    });
                                paint_eye(ui, eye_rect, entry.visible, text_color);
                                if eye_response.clicked() {
                                    action = Some(Action::Toggle(entry.channel_index));
                                }
                                let swatch = Rect::from_min_size(
                                    row.left_top(),
                                    egui::vec2(18.0, row.height()),
                                );
                                ui.interact(
                                    swatch,
                                    ui.id().with(("signal_drag", index)),
                                    egui::Sense::drag(),
                                )
                                .on_hover_text("Drag signal between plots")
                                .dnd_set_drag_payload(SignalDrag {
                                    scope: scope_instance,
                                    signal: entry.signal_id.clone(),
                                });
                                let value_right = eye_rect.left() - 5.0;
                                paint_swatch(
                                    ui,
                                    Rect::from_min_size(
                                        row.left_top(),
                                        egui::vec2(18.0, row.height()),
                                    ),
                                    if entry.visible {
                                        entry.color
                                    } else {
                                        entry.color.gamma_multiply(0.35)
                                    },
                                    entry.style,
                                );
                                let value = entry.value.as_ref().map(|value| {
                                    ui.painter().layout_no_wrap(
                                        value.clone(),
                                        egui::FontId::monospace(10.5),
                                        text_color,
                                    )
                                });
                                let value_width = value
                                    .as_ref()
                                    .map_or(0.0, |galley| galley.size().x.min(row.width() * 0.48));
                                let label_rect = Rect::from_min_max(
                                    row.left_top() + egui::vec2(24.0, 0.0),
                                    egui::pos2(
                                        (value_right - value_width - 8.0).max(row.left() + 24.0),
                                        row.bottom(),
                                    ),
                                );
                                let mut label_job = egui::text::LayoutJob::simple_singleline(
                                    entry.label.clone(),
                                    egui::FontId::proportional(11.0),
                                    text_color,
                                );
                                label_job.wrap.max_width = label_rect.width();
                                label_job.wrap.max_rows = 1;
                                label_job.wrap.break_anywhere = true;
                                let label = ui.fonts_mut(|fonts| fonts.layout_job(label_job));
                                ui.painter_at(label_rect).galley(
                                    label_rect.left_center()
                                        - egui::vec2(0.0, label.size().y * 0.5),
                                    label,
                                    text_color,
                                );
                                if let Some(galley) = value {
                                    let value_rect = Rect::from_min_max(
                                        egui::pos2(value_right - value_width, row.top()),
                                        egui::pos2(value_right, row.bottom()),
                                    );
                                    ui.painter_at(value_rect).galley(
                                        egui::pos2(
                                            value_right - galley.size().x,
                                            row.center().y - galley.size().y * 0.5,
                                        ),
                                        galley,
                                        text_color,
                                    );
                                }
                                let response = response.on_hover_text("Channel details");
                                egui::Popup::menu(&response)
                                    .id(ui.id().with(("channel_details", index)))
                                    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                                    .info(
                                        egui::UiStackInfo::new(egui::UiKind::Menu).with_tag_value(
                                            egui::containers::menu::MenuConfig::MENU_CONFIG_TAG,
                                            egui::containers::menu::MenuConfig::new()
                                                .close_behavior(
                                                    egui::PopupCloseBehavior::CloseOnClickOutside,
                                                ),
                                        ),
                                    )
                                    .show(|ui| {
                                        ui.label(&entry.signal_id);
                                        if let Some(stats) = entry.stats {
                                            egui::Grid::new(("legend_statistics", instance, index))
                                                .show(ui, |ui| {
                                                    for (label, value) in [
                                                        ("Min", stats.min),
                                                        ("Max", stats.max),
                                                        ("Mean", stats.mean),
                                                    ] {
                                                        ui.label(label);
                                                        ui.monospace(format!("{value:.3}"));
                                                        ui.end_row();
                                                    }
                                                });
                                        }
                                        for (label, tab) in [
                                            (
                                                "Appearance",
                                                ui::scope_settings::InspectorTab::Appearance,
                                            ),
                                            (
                                                "Assignment",
                                                ui::scope_settings::InspectorTab::Assignment,
                                            ),
                                            ("Axis", ui::scope_settings::InspectorTab::Axis),
                                        ] {
                                            if ui.button(label).clicked() {
                                                action =
                                                    Some(Action::Inspect(entry.channel_index, tab));
                                                ui.close();
                                            }
                                        }
                                        if let Some((a, b)) = entry.measurements {
                                            ui.label(format!("A ({a:.3})"));
                                            ui.label(format!("B ({b:.3})"));
                                            ui.label(format!("Δ ({:.3})", b - a));
                                        }
                                        if ui.button("Remove signal").clicked() {
                                            action = Some(Action::Remove(entry.channel_index));
                                            ui.close();
                                        }
                                    });
                                if statistics && let Some(stats) = entry.stats {
                                    ui.horizontal(|ui| {
                                        for (label, value) in [
                                            ("Min", stats.min),
                                            ("Max", stats.max),
                                            ("Mean", stats.mean),
                                        ] {
                                            ui.add(
                                                egui::Label::new(
                                                    egui::RichText::new(format!(
                                                        "{label} ({value:.2})"
                                                    ))
                                                    .size(9.0)
                                                    .color(theme.muted_text_color()),
                                                )
                                                .truncate(),
                                            );
                                        }
                                    });
                                }
                            }
                        });
                });
        });
    action
}

fn paint_eye(ui: &egui::Ui, rect: Rect, visible: bool, color: Color32) {
    let center = rect.center();
    let points = [
        (-7.0, 0.0),
        (-4.0, -3.0),
        (0.0, -4.0),
        (4.0, -3.0),
        (7.0, 0.0),
        (4.0, 3.0),
        (0.0, 4.0),
        (-4.0, 3.0),
        (-7.0, 0.0),
    ]
    .map(|(x, y)| center + egui::vec2(x, y))
    .to_vec();
    ui.painter()
        .add(egui::Shape::line(points, Stroke::new(1.0_f32, color)));
    if visible {
        ui.painter().circle_filled(center, 1.6, color);
    } else {
        ui.painter().line_segment(
            [
                center + egui::vec2(-7.0, 5.0),
                center + egui::vec2(7.0, -5.0),
            ],
            Stroke::new(1.3_f32, color),
        );
    }
}

fn paint_swatch(ui: &egui::Ui, rect: Rect, color: Color32, style: egui_plot::LineStyle) {
    let y = rect.center().y;
    match style {
        egui_plot::LineStyle::Solid => {
            ui.painter().line_segment(
                [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
                Stroke::new(2.0_f32, color),
            );
        }
        egui_plot::LineStyle::Dashed { .. } => {
            for offset in [0.0, 7.0, 14.0] {
                ui.painter().line_segment(
                    [
                        egui::pos2(rect.left() + offset, y),
                        egui::pos2((rect.left() + offset + 4.0).min(rect.right()), y),
                    ],
                    Stroke::new(2.0_f32, color),
                );
            }
        }
        egui_plot::LineStyle::Dotted { .. } => {
            for offset in [1.0, 6.0, 11.0, 16.0] {
                ui.painter()
                    .circle_filled(egui::pos2(rect.left() + offset, y), 1.3, color);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_panel_has_equal_side_and_bottom_insets() {
        let ctx = egui::Context::default();
        ui::theme::store_theme(&ctx, ui::theme::ThemeSelection::default().get_colors());
        let plot = Rect::from_min_size(egui::pos2(30.0, 20.0), egui::vec2(600.0, 400.0));
        for rows in [1, 2, 20] {
            let entries: Vec<_> = (0..rows)
                .map(|index| Entry {
                    channel_index: index,
                    label: format!("Signal {index}"),
                    signal_id: format!("message.signal_{index}"),
                    value: None,
                    color: Color32::LIGHT_BLUE,
                    style: egui_plot::LineStyle::Solid,
                    stats: None,
                    visible: true,
                    measurements: None,
                })
                .collect();
            let rect = bounds(plot, rows, false).unwrap();
            let mut panel = None;
            for pass in 0..3 {
                let output = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(800.0, 600.0),
                        )),
                        time: Some((rows * 10 + pass) as f64),
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            show(ui, rect, rows, &entries, false, rows);
                        });
                    },
                );
                panel = output.shapes.iter().find_map(|shape| match &shape.shape {
                    egui::Shape::Rect(shape)
                        if shape.rect.width() >= 150.0 && shape.rect.width() <= 330.0 =>
                    {
                        Some(shape.rect)
                    }
                    _ => None,
                });
            }
            let panel = panel.expect("Legend frame");
            let side_gap = panel.left() - plot.left();
            let bottom_gap = plot.bottom() - panel.bottom();
            assert!(
                (side_gap - bottom_gap).abs() <= 1.0,
                "Rows {rows} have side gap {side_gap} but bottom gap {bottom_gap}"
            );
            assert!((bottom_gap - 8.0).abs() <= 1.0);
        }
    }

    #[test]
    fn legend_is_bounded_and_anchored_bottom_left() {
        for size in [egui::vec2(240.0, 120.0), egui::vec2(1200.0, 700.0)] {
            let plot = Rect::from_min_size(egui::pos2(30.0, 20.0), size);
            for rows in [1, 3, 100] {
                for stats in [false, true] {
                    let legend = bounds(plot, rows, stats).unwrap();
                    assert!(plot.contains_rect(legend));
                    assert_eq!(
                        legend.left_bottom(),
                        plot.left_bottom() + egui::vec2(8.0, -8.0)
                    );
                    assert!(legend.width() <= 320.0);
                    assert!(legend.height() <= 180.0);
                }
            }
        }
        assert!(
            bounds(
                Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(60.0, 40.0)),
                1,
                false
            )
            .is_none()
        );
    }
}
