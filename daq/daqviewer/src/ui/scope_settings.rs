//! Persistent scope appearance, axis, and lane configuration.
use crate::ui::scope::ScopeChannel;
use eframe::egui;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceMode {
    #[default]
    Solid,
    Dashed,
    Dotted,
    Points,
    Steps,
}
impl TraceMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Solid => "Solid",
            Self::Dashed => "Dashed",
            Self::Dotted => "Dotted",
            Self::Points => "Points",
            Self::Steps => "Steps",
        }
    }
    pub fn line_style(self, comparison: egui_plot::LineStyle) -> egui_plot::LineStyle {
        match self {
            Self::Dashed => egui_plot::LineStyle::dashed_dense(),
            Self::Dotted | Self::Points => egui_plot::LineStyle::dotted_dense(),
            _ => comparison,
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SignalStyle {
    pub width: f32,
    pub opacity: f32,
    pub mode: TraceMode,
    pub points: bool,
    pub envelope: bool,
    pub axis: String,
    pub lane: usize,
}
impl Default for SignalStyle {
    fn default() -> Self {
        Self {
            width: 1.5,
            opacity: 1.0,
            mode: TraceMode::Solid,
            points: false,
            envelope: true,
            axis: String::new(),
            lane: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AxisSide {
    #[default]
    Auto,
    Left,
    Right,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct AxisConfig {
    pub label: String,
    pub side: AxisSide,
    pub range: Option<[f64; 2]>,
    pub color: Option<[u8; 3]>,
}
impl AxisConfig {
    pub fn valid_range(&self) -> Option<(f64, f64)> {
        self.range
            .filter(|[lo, hi]| lo.is_finite() && hi.is_finite() && lo < hi)
            .map(|[lo, hi]| (lo, hi))
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ScopeConfig {
    pub stacked: bool,
    pub lanes: usize,
    pub channels: BTreeMap<String, SignalStyle>,
    pub axes: BTreeMap<String, AxisConfig>,
}
impl Default for ScopeConfig {
    fn default() -> Self {
        Self {
            stacked: false,
            lanes: 2,
            channels: BTreeMap::new(),
            axes: BTreeMap::new(),
        }
    }
}
impl ScopeConfig {
    pub fn style(&self, signal: &str) -> SignalStyle {
        self.channels.get(signal).cloned().unwrap_or_default()
    }
    pub fn lane(&self, signal: &str) -> usize {
        if self.stacked {
            self.style(signal).lane.min(self.lanes.saturating_sub(1))
        } else {
            0
        }
    }
    pub fn axis_key(&self, channel: &ScopeChannel) -> String {
        let style = self.style(&channel.signal_id);
        format!(
            "{}|{}",
            self.lane(&channel.signal_id),
            if style.axis.is_empty() {
                &channel.unit
            } else {
                &style.axis
            }
        )
    }
    pub fn axis_lane(key: &str) -> usize {
        key.split_once('|')
            .and_then(|(lane, _)| lane.parse().ok())
            .unwrap_or(0)
    }
    pub fn toggle_stack(&mut self, channels: &[ScopeChannel]) {
        self.stacked = !self.stacked;
        if self.stacked && !channels.iter().any(|ch| self.style(&ch.signal_id).lane > 0) {
            self.lanes = 2;
            for (index, channel) in channels.iter().enumerate() {
                self.channels
                    .entry(channel.signal_id.clone())
                    .or_default()
                    .lane = index % 2;
            }
        }
    }
    pub fn sanitize(&mut self) {
        self.lanes = self.lanes.clamp(1, 8);
        for style in self.channels.values_mut() {
            style.width = if style.width.is_finite() {
                style.width.clamp(0.5, 8.0)
            } else {
                1.5
            };
            style.opacity = if style.opacity.is_finite() {
                style.opacity.clamp(0.05, 1.0)
            } else {
                1.0
            };
            style.lane = style.lane.min(self.lanes - 1);
        }
        for axis in self.axes.values_mut() {
            if axis.valid_range().is_none() {
                axis.range = None;
            }
        }
    }
}

pub(super) struct SignalUpdate {
    pub index: usize,
    pub style: SignalStyle,
    pub color: [u8; 3],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InspectorTab {
    Appearance,
    Assignment,
    Axis,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn inspector(
    ctx: &egui::Context,
    instance: usize,
    index: usize,
    channels: &[ScopeChannel],
    config: &ScopeConfig,
    tab: &mut InspectorTab,
    open: &mut bool,
    auto_range: (f64, f64),
) -> (Option<SignalUpdate>, Option<(String, AxisConfig)>) {
    let Some(channel) = channels.get(index) else {
        *open = false;
        return (None, None);
    };
    let original = config.style(&channel.signal_id);
    let mut style = original.clone();
    let original_color = [channel.color.r(), channel.color.g(), channel.color.b()];
    let mut color = original_color;
    let key = config.axis_key(channel);
    let mut axis_update = None;
    if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
        *open = false;
    }
    egui::Window::new("Signal settings")
        .id(egui::Id::new(("scope_inspector", instance)))
        .open(open)
        .resizable(false)
        .collapsible(false)
        .default_width(300.0)
        .show(ctx, |ui| {
            ui.label(&channel.signal_id);
            ui.horizontal(|ui| {
                ui.selectable_value(tab, InspectorTab::Appearance, "Appearance");
                ui.selectable_value(tab, InspectorTab::Assignment, "Assignment");
                ui.selectable_value(tab, InspectorTab::Axis, "Axis");
            });
            ui.separator();
            match tab {
                InspectorTab::Appearance => {
                    ui.horizontal(|ui| {
                        ui.label("Color");
                        ui.color_edit_button_srgb(&mut color);
                    });
                    ui.horizontal(|ui| {
                        ui.label("Width");
                        ui.add(
                            egui::DragValue::new(&mut style.width)
                                .range(0.5..=8.0)
                                .speed(0.1),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("Opacity");
                        ui.add(egui::Slider::new(&mut style.opacity, 0.05..=1.0));
                    });
                    egui::ComboBox::from_id_salt("trace_mode")
                        .selected_text(style.mode.label())
                        .show_ui(ui, |ui| {
                            for mode in [
                                TraceMode::Solid,
                                TraceMode::Dashed,
                                TraceMode::Dotted,
                                TraceMode::Points,
                                TraceMode::Steps,
                            ] {
                                ui.selectable_value(&mut style.mode, mode, mode.label());
                            }
                        });
                    ui.checkbox(&mut style.points, "Show points");
                    ui.checkbox(&mut style.envelope, "Show envelope");
                }
                InspectorTab::Assignment => {
                    ui.horizontal(|ui| {
                        ui.label("Plot");
                        egui::ComboBox::from_id_salt("signal_lane")
                            .selected_text((style.lane + 1).to_string())
                            .show_ui(ui, |ui| {
                                for lane in 0..config.lanes {
                                    ui.selectable_value(
                                        &mut style.lane,
                                        lane,
                                        (lane + 1).to_string(),
                                    );
                                }
                            });
                    });
                    egui::ComboBox::from_id_salt("signal_axis")
                        .selected_text(if style.axis.is_empty() {
                            "Auto"
                        } else {
                            &style.axis
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut style.axis,
                                String::new(),
                                format!("Auto ({})", channel.unit),
                            );
                            let mut choices = std::collections::BTreeSet::new();
                            for other in channels {
                                let assigned = config.style(&other.signal_id);
                                if other.unit == channel.unit && !assigned.axis.is_empty() {
                                    choices.insert(assigned.axis);
                                }
                            }
                            for key in choices {
                                ui.selectable_value(&mut style.axis, key.clone(), key);
                            }
                        });
                    if ui.button("Separate axis").clicked() {
                        style.axis = channel.signal_id.clone();
                    }
                }
                InspectorTab::Axis => {
                    let mut axis = config.axes.get(&key).cloned().unwrap_or_default();
                    if axis_editor(ui, &mut axis, auto_range, channel.color) {
                        axis_update = Some((key.clone(), axis));
                    }
                }
            }
        });
    let update = (style != original || color != original_color).then_some(SignalUpdate {
        index,
        style,
        color,
    });
    (update, axis_update)
}

pub(super) fn axis_editor(
    ui: &mut egui::Ui,
    axis: &mut AxisConfig,
    auto_range: (f64, f64),
    default_color: egui::Color32,
) -> bool {
    let original = axis.clone();
    let mut fixed = axis.range.is_some();
    ui.horizontal(|ui| {
        ui.selectable_value(&mut fixed, false, "Auto");
        ui.selectable_value(&mut fixed, true, "Fixed");
    });
    if fixed {
        let [mut lo, mut hi] = axis.range.unwrap_or([auto_range.0, auto_range.1]);
        ui.horizontal(|ui| {
            ui.label("Min");
            ui.add(egui::DragValue::new(&mut lo).speed(0.1));
            ui.label("Max");
            ui.add(egui::DragValue::new(&mut hi).speed(0.1));
        });
        if lo.is_finite() && hi.is_finite() && lo < hi {
            axis.range = Some([lo, hi]);
        } else {
            ui.label("Invalid range");
        }
    } else {
        axis.range = None;
    }
    ui.horizontal(|ui| {
        ui.selectable_value(&mut axis.side, AxisSide::Auto, "Auto side");
        ui.selectable_value(&mut axis.side, AxisSide::Left, "Left");
        ui.selectable_value(&mut axis.side, AxisSide::Right, "Right");
    });
    ui.horizontal(|ui| {
        ui.label("Name");
        ui.text_edit_singleline(&mut axis.label);
    });
    let mut color = axis
        .color
        .unwrap_or([default_color.r(), default_color.g(), default_color.b()]);
    ui.horizontal(|ui| {
        ui.label("Color");
        if ui.color_edit_button_srgb(&mut color).changed() {
            axis.color = Some(color);
        }
        if ui.button("Auto color").clicked() {
            axis.color = None;
        }
    });
    *axis != original
}

pub(super) fn steps(points: &[[f64; 2]]) -> Vec<[f64; 2]> {
    let mut out = Vec::with_capacity(points.len() * 2);
    for point in points {
        if let Some(previous) = out.last().copied() {
            let previous: [f64; 2] = previous;
            if previous[1].is_finite() && point[1].is_finite() {
                out.push([point[0], previous[1]]);
            }
        }
        out.push(*point);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configuration_defaults_and_validation_are_safe() {
        let mut config: ScopeConfig = serde_json::from_str(r#"{"lanes":999,"channels":{"a":{"width":-2,"opacity":4,"lane":99}} ,"axes":{"0|V":{"range":[5,1]}}}"#).unwrap();
        config.sanitize();
        assert_eq!(config.lanes, 8);
        assert_eq!(config.style("a").lane, 7);
        assert_eq!(config.style("a").width, 0.5);
        assert_eq!(config.style("a").opacity, 1.0);
        assert_eq!(config.axes["0|V"].range, None);
        assert_eq!(
            serde_json::from_str::<ScopeConfig>("{}").unwrap(),
            ScopeConfig::default()
        );
    }
    #[test]
    fn step_rendering_does_not_bridge_missing_data() {
        let data = [[0.0, 1.0], [1.0, 2.0], [2.0, f64::NAN], [3.0, 4.0]];
        let stepped = steps(&data);
        assert_eq!(&stepped[..3], &[[0.0, 1.0], [1.0, 1.0], [1.0, 2.0]]);
        assert!(stepped[3][1].is_nan());
        assert_eq!(stepped[4], [3.0, 4.0]);
    }
}
