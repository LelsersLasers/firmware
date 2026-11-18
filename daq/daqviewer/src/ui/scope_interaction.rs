//! Scope gestures request changes to the shared timeline, never a private X viewport.
use crate::workspace_view::WorkspaceDataView;
use eframe::egui::{self, Color32, Pos2};

#[derive(Debug, Clone, Default)]
pub(super) struct Measurements {
    pub enabled: bool,
    pub pins: [Option<i64>; 2],
    pub next: usize,
    pub selection: Option<(i64, i64)>,
}

pub(super) enum Gesture {
    /// Left-click drag: highlights a section of the graph and displays section statistics
    /// (`min`, `max`, `avg`, `sum`, `Δ`); simple left-click (`< 5 px`) scrubs the playhead.
    SelectStats {
        lane: usize,
        origin: Pos2,
        start: i64,
        end: i64,
    },
    /// Right-click drag: box-selects a time/distance range to zoom into.
    ZoomRange {
        lane: usize,
        origin: Pos2,
        start: i64,
        end: i64,
    },
    Measure {
        lane: usize,
        slot: usize,
    },
    /// Middle-click drag: pans the timeline horizontally.
    Pan {
        lane: usize,
        previous: Pos2,
    },
}
impl Gesture {
    fn lane(&self) -> usize {
        match self {
            Self::SelectStats { lane, .. }
            | Self::ZoomRange { lane, .. }
            | Self::Measure { lane, .. }
            | Self::Pan { lane, .. } => *lane,
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn handle(
    plot: &mut egui_plot::PlotUi<'_>,
    view: &WorkspaceDataView<'_>,
    gesture: &mut Option<Gesture>,
    measurements: &mut Measurements,
    lane: usize,
    to_ms: &impl Fn(f64) -> Option<i64>,
    to_x: &impl Fn(i64) -> Option<f64>,
    max_x: f64,
    blocked: bool,
) {
    let frame = *plot.transform().frame();
    let input = plot.ctx().input(|input| {
        (
            input.pointer.interact_pos(),
            input.pointer.button_pressed(egui::PointerButton::Primary),
            input.pointer.button_released(egui::PointerButton::Primary),
            input.pointer.button_down(egui::PointerButton::Primary),
            input.pointer.button_pressed(egui::PointerButton::Secondary),
            input
                .pointer
                .button_released(egui::PointerButton::Secondary),
            input.pointer.button_down(egui::PointerButton::Secondary),
            input.pointer.button_pressed(egui::PointerButton::Middle),
            input.pointer.button_released(egui::PointerButton::Middle),
            input.pointer.button_down(egui::PointerButton::Middle),
            input.key_pressed(egui::Key::Escape),
        )
    });
    let (
        pointer,
        pressed,
        released,
        down,
        right_pressed,
        right_released,
        right_down,
        mid_pressed,
        mid_released,
        mid_down,
        cancel,
    ) = input;
    if cancel {
        *gesture = None;
        measurements.selection = None;
    }
    let time_at = |pos: Pos2| to_ms(plot.plot_from_screen(pos).x.clamp(0.0, max_x));
    if !blocked
        && !cancel
        && gesture.is_none()
        && let Some(pointer) = pointer.filter(|pos| frame.contains(*pos))
    {
        if mid_pressed {
            view.request_pause();
            *gesture = Some(Gesture::Pan {
                lane,
                previous: pointer,
            });
        } else if right_pressed && let Some(time) = time_at(pointer) {
            view.request_pause();
            *gesture = Some(Gesture::ZoomRange {
                lane,
                origin: pointer,
                start: time,
                end: time,
            });
        } else if pressed && let Some(time) = time_at(pointer) {
            view.request_pause();
            if measurements.enabled {
                let closest = measurements
                    .pins
                    .iter()
                    .enumerate()
                    .filter_map(|(slot, pin)| {
                        let x = to_x((*pin)?)?;
                        let px = plot.screen_from_plot(egui_plot::PlotPoint::new(x, 0.0)).x;
                        Some((slot, (px - pointer.x).abs()))
                    })
                    .filter(|(_, distance)| *distance <= 7.0)
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(slot, _)| slot);
                let slot = closest.unwrap_or(measurements.next);
                measurements.pins[slot] = Some(time);
                measurements.next = 1 - slot;
                *gesture = Some(Gesture::Measure { lane, slot });
            } else {
                *gesture = Some(Gesture::SelectStats {
                    lane,
                    origin: pointer,
                    start: time,
                    end: time,
                });
            }
        }
    }
    let owns = gesture
        .as_ref()
        .is_some_and(|gesture| gesture.lane() == lane);
    if owns && let Some(pointer) = pointer {
        match gesture.as_mut().unwrap() {
            Gesture::SelectStats {
                origin, start, end, ..
            } => {
                if let Some(time) = time_at(pointer) {
                    *end = time;
                }
                if (pointer.x - origin.x).abs() >= 5.0 {
                    measurements.selection = Some(((*start).min(*end), (*start).max(*end)));
                }
                if released {
                    if (pointer.x - origin.x).abs() >= 5.0 {
                        measurements.selection = Some(((*start).min(*end), (*start).max(*end)));
                    } else {
                        measurements.selection = None;
                        view.request_setpoint_utc_ms(*end);
                    }
                }
            }
            Gesture::ZoomRange {
                origin, start, end, ..
            } => {
                if let Some(time) = time_at(pointer) {
                    *end = time;
                }
                if right_released && (pointer.x - origin.x).abs() >= 5.0 {
                    measurements.selection = None;
                    view.request_range_ms(*start, *end);
                }
            }
            Gesture::Measure { slot, .. } => {
                if let Some(time) = time_at(pointer) {
                    measurements.pins[*slot] = Some(time);
                }
            }
            Gesture::Pan { previous, .. } => {
                if mid_down || mid_released {
                    if let (Some(before), Some(after)) = (time_at(*previous), time_at(pointer)) {
                        view.request_pan_delta_ms(before.saturating_sub(after));
                    }
                    *previous = pointer;
                }
            }
        }
    }
    if owns
        && let Some(Gesture::ZoomRange {
            origin, start, end, ..
        }) = gesture.as_ref()
        && right_down
        && pointer.is_some_and(|pos| (pos.x - origin.x).abs() >= 5.0)
        && let (Some(a), Some(b)) = (to_x(*start), to_x(*end))
    {
        let bounds = plot.plot_bounds();
        plot.polygon(
            egui_plot::Polygon::new(
                "",
                vec![
                    [a, bounds.min()[1]],
                    [a, bounds.max()[1]],
                    [b, bounds.max()[1]],
                    [b, bounds.min()[1]],
                ],
            )
            .fill_color(Color32::from_rgba_unmultiplied(80, 190, 255, 35))
            .stroke(egui::Stroke::new(1.0_f32, Color32::LIGHT_BLUE))
            .allow_hover(false),
        );
        let point = plot.plot_from_screen(egui::pos2(frame.center().x, frame.top() + 8.0));
        plot.text(
            egui_plot::Text::new(
                "",
                point,
                egui::RichText::new(format!(
                    "Zoom Δt ({:.3} s)",
                    end.abs_diff(*start) as f64 / 1000.0
                ))
                .monospace()
                .size(11.0),
            )
            .anchor(egui::Align2::CENTER_TOP)
            .color(Color32::LIGHT_BLUE),
        );
    }
    if owns
        && (released
            || right_released
            || mid_released
            || (!down && !right_down && !mid_down))
    {
        *gesture = None;
    }
    if measurements.enabled {
        for (slot, pin) in measurements.pins.iter().enumerate() {
            if let Some(x) = pin.and_then(to_x)
                && x >= 0.0
                && x <= max_x
            {
                let color = if slot == 0 {
                    Color32::LIGHT_BLUE
                } else {
                    Color32::from_rgb(255, 170, 75)
                };
                plot.vline(
                    egui_plot::VLine::new(if slot == 0 { "A" } else { "B" }, x)
                        .color(color)
                        .width(1.5_f32),
                );
                let px = plot.screen_from_plot(egui_plot::PlotPoint::new(x, 0.0)).x;
                let label = plot.plot_from_screen(egui::pos2(px, frame.top() + 22.0));
                plot.text(
                    egui_plot::Text::new(
                        "",
                        label,
                        egui::RichText::new(if slot == 0 { "A" } else { "B" })
                            .size(10.0)
                            .strong(),
                    )
                    .color(color)
                    .anchor(egui::Align2::CENTER_TOP),
                );
            }
        }
        if let [Some(a), Some(b)] = measurements.pins {
            let point = plot.plot_from_screen(egui::pos2(frame.right() - 8.0, frame.top() + 8.0));
            plot.text(
                egui_plot::Text::new(
                    "",
                    point,
                    egui::RichText::new(format!("Δt ({:.3} s)", a.abs_diff(b) as f64 / 1000.0))
                        .monospace()
                        .size(11.0),
                )
                .anchor(egui::Align2::RIGHT_TOP),
            );
        }
    }
}

pub(super) fn sample(points: &[[f64; 2]], x: f64, step: bool) -> Option<f64> {
    if !x.is_finite() || points.is_empty() || x < points.first()?[0] || x > points.last()?[0] {
        return None;
    }
    let index = points
        .partition_point(|point| point[0] <= x)
        .saturating_sub(1);
    let left = points[index];
    if !left[1].is_finite() {
        return None;
    }
    if step || left[0] == x {
        return Some(left[1]);
    }
    let right = *points.get(index + 1)?;
    if !right[1].is_finite() || right[0] <= left[0] {
        return None;
    }
    Some(left[1] + (right[1] - left[1]) * (x - left[0]) / (right[0] - left[0]))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn measurements_interpolate_without_crossing_gaps() {
        let points = [[0.0, 1.0], [1.0, 3.0], [2.0, f64::NAN], [3.0, 7.0]];
        assert_eq!(sample(&points, 0.5, false), Some(2.0));
        assert_eq!(sample(&points, 0.5, true), Some(1.0));
        assert_eq!(sample(&points, 1.5, false), None);
        assert_eq!(sample(&points, 2.5, false), None);
        assert_eq!(sample(&points, 4.0, false), None);
    }
}
