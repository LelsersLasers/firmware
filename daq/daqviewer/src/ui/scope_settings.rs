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
    pub discrete: bool,
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
            discrete: false,
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
    #[serde(deserialize_with = "deserialize_plot_names")]
    pub plot_names: BTreeMap<usize, String>,
}
// PaneSpec's tagged-enum deserializer buffers JSON object keys as strings.
fn deserialize_plot_names<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<usize, String>, D::Error> {
    let names = <BTreeMap<String, String> as serde::Deserialize>::deserialize(deserializer)?;
    names
        .into_iter()
        .map(|(key, name)| {
            key.parse::<usize>()
                .map(|lane| (lane, name))
                .map_err(serde::de::Error::custom)
        })
        .collect()
}

impl Default for ScopeConfig {
    fn default() -> Self {
        Self {
            stacked: false,
            lanes: 2,
            channels: BTreeMap::new(),
            axes: BTreeMap::new(),
            plot_names: BTreeMap::new(),
        }
    }
}
pub(super) fn axis_quantity(signal: &str) -> Option<&'static str> {
    let field = signal.rsplit('.').next()?;
    match field {
        "AMK_TorqueSetpoint"
        | "AMK_ActualTorque"
        | "total_actual_torque"
        | "total_torque_request" => Some("Torque"),
        "throttle" => Some("Throttle"),
        _ => None,
    }
}

fn unit_group(channel: &ScopeChannel) -> String {
    if matches!(channel.unit.trim(), "" | "%" | "percent")
        && let Some(quantity) = axis_quantity(&channel.signal_id)
    {
        format!("quantity:{quantity}:{}", channel.unit)
    } else {
        channel.unit.clone()
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
        let group = unit_group(channel);
        let axis = match style.axis.split_once(':') {
            Some((side @ ("left" | "right"), unit)) if unit == channel.unit => {
                format!("{side}:{group}")
            }
            _ if style.axis.is_empty() => group,
            _ => style.axis,
        };
        format!("{}|{axis}", self.lane(&channel.signal_id))
    }

    pub(super) fn axis_config(&self, key: &str) -> Option<&AxisConfig> {
        self.axes.get(key).or_else(|| {
            // Before quantity grouping, saved scales used only the unit and optional side.
            let (lane, group) = key.split_once('|')?;
            let (side, group) = match group.split_once(':') {
                Some((side @ ("left" | "right"), group)) => (Some(side), group),
                _ => (None, group),
            };
            let (_, unit) = group.strip_prefix("quantity:")?.split_once(':')?;
            let old_key = side.map_or_else(
                || format!("{lane}|{unit}"),
                |side| format!("{lane}|{side}:{unit}"),
            );
            self.axes.get(&old_key)
        })
    }

    pub(super) fn resolved_axis_sides(
        &self,
        channels: &[ScopeChannel],
    ) -> BTreeMap<String, AxisSide> {
        let mut counts = BTreeMap::<usize, [usize; 2]>::new();
        let mut sides = BTreeMap::new();
        let mut automatic = Vec::new();
        for channel in channels {
            if self.style(&channel.signal_id).discrete {
                continue;
            }
            let key = self.axis_key(channel);
            if sides.contains_key(&key) {
                continue;
            }
            let side = self
                .axis_config(&key)
                .map_or(AxisSide::Auto, |axis| axis.side);
            let lane = self.lane(&channel.signal_id);
            match side {
                AxisSide::Left => counts.entry(lane).or_default()[0] += 1,
                AxisSide::Right => counts.entry(lane).or_default()[1] += 1,
                AxisSide::Auto => automatic.push((lane, key.clone())),
            }
            sides.insert(key, side);
        }
        for (lane, key) in automatic {
            let [left, right] = counts.entry(lane).or_default();
            let side = if *left > *right {
                *right += 1;
                AxisSide::Right
            } else {
                *left += 1;
                AxisSide::Left
            };
            sides.insert(key, side);
        }
        sides
    }
    pub fn axis_lane(key: &str) -> usize {
        key.split_once('|')
            .and_then(|(lane, _)| lane.parse().ok())
            .unwrap_or(0)
    }
    pub fn toggle_stack(&mut self, channels: &[ScopeChannel]) {
        self.stacked = !self.stacked;
        if self.stacked && !channels.iter().any(|ch| self.style(&ch.signal_id).lane > 0) {
            self.lanes = self.lanes.max(2);
            for (index, channel) in channels.iter().enumerate() {
                self.channels
                    .entry(channel.signal_id.clone())
                    .or_default()
                    .lane = index % self.lanes;
            }
        }
    }
    pub fn assign(&mut self, channel: &ScopeChannel, lane: usize, destination: Destination) {
        let style = self.channels.entry(channel.signal_id.clone()).or_default();
        style.lane = lane;
        style.discrete = destination == Destination::Discrete;
        if style.discrete {
            style.axis = format!("discrete:{}", channel.signal_id);
        } else {
            let side = if destination == Destination::Right {
                "right"
            } else {
                "left"
            };
            style.axis = format!("{side}:{}", unit_group(channel));
            self.axes
                .entry(format!("{lane}|{}", style.axis))
                .or_default()
                .side = if destination == Destination::Right {
                AxisSide::Right
            } else {
                AxisSide::Left
            };
        }
    }

    pub fn destination(&self, channel: &ScopeChannel) -> Destination {
        if self.style(&channel.signal_id).discrete {
            Destination::Discrete
        } else if self
            .axis_config(&self.axis_key(channel))
            .is_some_and(|axis| axis.side == AxisSide::Right)
        {
            Destination::Right
        } else {
            Destination::Left
        }
    }

    pub fn sanitize(&mut self) {
        self.lanes = self.lanes.max(1);
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Destination {
    Left,
    Right,
    Discrete,
}
impl Destination {
    pub fn label(self) -> &'static str {
        match self {
            Self::Left => "Left",
            Self::Right => "Right",
            Self::Discrete => "Discrete",
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
                                format!(
                                    "Auto ({})",
                                    if channel.unit.is_empty() {
                                        "unit unknown"
                                    } else {
                                        &channel.unit
                                    }
                                ),
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
                    let mut axis = config.axis_config(&key).cloned().unwrap_or_default();
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
        assert_eq!(config.lanes, 999);
        assert_eq!(config.style("a").lane, 99);
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
