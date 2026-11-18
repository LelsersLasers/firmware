//! Scope rendering uses one shared X window with independently configured plot lanes.
use super::*;
use eframe::egui::{self, Color32, RichText};
use ui::scope_settings::{AxisSide, ScopeConfig, TraceMode};

struct AxisView {
    index: usize,
    lane: usize,
    right: bool,
    custom_label: String,
    unit: String,
    color: Color32,
}

/// Scale trace stroke thickness as a function of plot dimensions (`plot_width`, `plot_height`)
/// and active time window zoom (`window_s`).
///
/// - Small multi-tile plots (`~450x220`) zoomed out (`>= 120 s`) stay at `base_width` (`1.5 px`).
/// - Large/full-workspace plots (`~1150x700`) and zoomed-in windows (`< 120 s`) smoothly scale up
///   so lines have more visual weight on large canvases and high-zoom views.
pub(super) fn adaptive_trace_width(
    base_width: f32,
    plot_width: f32,
    plot_height: f32,
    window_s: f64,
) -> f32 {
    let geom_dim = (plot_width.max(1.0) * plot_height.max(1.0)).sqrt();
    let size_t = ((geom_dim - 300.0) / 650.0).clamp(0.0, 1.0);
    let size_scale = 1.0 + 0.45 * size_t;

    let clamped_s = window_s.clamp(2.0, 120.0);
    let zoom_t =
        ((120.0_f64.ln() - clamped_s.ln()) / (120.0_f64.ln() - 2.0_f64.ln())).clamp(0.0, 1.0)
            as f32;
    let zoom_scale = 1.0 + 0.45 * zoom_t;

    (base_width * size_scale * zoom_scale).clamp(base_width, 4.5)
}

fn axes(
    cached: &CachedScopeData,
    config: &ScopeConfig,
    channels: &[ScopeChannel],
) -> Vec<AxisView> {
    let mut counts = std::collections::BTreeMap::<usize, usize>::new();
    cached
        .unit_groups
        .iter()
        .enumerate()
        .map(|(index, key)| {
            let lane = ScopeConfig::axis_lane(key);
            let order = counts.entry(lane).or_default();
            let axis = config.axes.get(key).cloned().unwrap_or_default();
            let channel = channels
                .iter()
                .find(|channel| config.axis_key(channel) == *key);
            let unit = channel
                .map(|channel| channel.unit.clone())
                .unwrap_or_default();
            let color = axis
                .color
                .map(|[r, g, b]| Color32::from_rgb(r, g, b))
                .or_else(|| channel.map(|channel| channel.color))
                .unwrap_or(Color32::GRAY);
            let right = match axis.side {
                AxisSide::Auto => *order % 2 == 1,
                AxisSide::Left => false,
                AxisSide::Right => true,
            };
            *order += 1;
            AxisView {
                index,
                lane,
                right,
                custom_label: axis.label,
                unit,
                color,
            }
        })
        .collect()
}

fn hints(
    cached: &CachedScopeData,
    axes: &[AxisView],
    lane: usize,
    lane_count: usize,
    slots: (usize, usize),
) -> Vec<egui_plot::AxisHints<'static>> {
    let min_thick = if lane_count > 1 { 64.0 } else { 14.0 };
    let mut result = Vec::new();
    for (right, count) in [(false, slots.0), (true, slots.1)] {
        let selected: Vec<_> = axes
            .iter()
            .filter(|axis| axis.lane == lane && axis.right == right)
            .collect();
        for slot in 0..count {
            let placement = if right {
                egui_plot::HPlacement::Right
            } else {
                egui_plot::HPlacement::Left
            };
            let mut hint = egui_plot::AxisHints::new_y()
                .placement(placement)
                .min_thickness(min_thick);
            result.push(if let Some(axis) = selected.get(slot) {
                let tf = cached.unit_transforms[axis.index];
                let unit_suffix = axis.unit.clone();
                if !axis.custom_label.is_empty() {
                    hint = hint.label(
                        RichText::new(axis.custom_label.chars().take(18).collect::<String>())
                            .color(axis.color),
                    );
                }
                hint.formatter(move |mark, _| {
                    let value = tf.plot_to_phys(mark.value);
                    let step = (mark.step_size / tf.scale.abs().max(1e-9)).max(1e-6);
                    let decimals = (-step.log10().floor() as isize).clamp(0, 3) as usize;
                    let num_str =
                        egui::emath::format_with_decimals_in_range(value, decimals..=decimals);
                    if unit_suffix.is_empty() {
                        num_str
                    } else {
                        format!("{num_str} {unit_suffix}")
                    }
                })
            } else {
                hint.formatter(|_, _| String::new())
            });
        }
    }
    result
}

impl Scope {
    pub(super) fn build_legend_entries(
        &self,
        view: &workspace_view::WorkspaceDataView<'_>,
        comparison: &[daqcore::storage::Annotation],
        channel_stats: &[Option<ChannelStats>],
    ) -> Vec<ui::scope_legend::Entry> {
        let Some(cached) = self.cached_data.as_ref() else {
            return Vec::new();
        };
        let is_comparison = comparison.len() >= 2;
        let x_mode = self.x_axis_mode;
        let start_ms = view.timeline.start_utc_ms();
        let reference_ms = if is_comparison {
            comparison[0].start_utc_ms
        } else {
            start_ms
        };
        let max_x = cached.max_x_bound;
        let to_x =
            |time: i64| match x_mode {
                ScopeXAxisMode::TrackDistance => cached.primary_distance_lookup.as_ref().and_then(
                    |(_, timestamps, distances)| {
                        interpolate_distance_at_ms(timestamps, distances, time)
                    },
                ),
                _ => Some((time - reference_ms) as f64 / 1000.0),
            };
        let traces_per_channel = if is_comparison { comparison.len() } else { 1 };
        let measurement_pair = |index: usize| -> Option<(f64, f64)> {
            if !self.measurements.enabled {
                return None;
            }
            let [Some(a), Some(b)] = self.measurements.pins else {
                return None;
            };
            let (ax, bx) = (to_x(a)?, to_x(b)?);
            if ax < 0.0 || ax > max_x || bx < 0.0 || bx > max_x {
                return None;
            }
            let trace = cached.mapped_traces.get(index)?;
            let channel = self.channels.get(index / traces_per_channel)?;
            let step = self.config.style(&channel.signal_id).mode == TraceMode::Steps;
            let tf = cached.unit_transforms[trace.unit_idx];
            Some((
                tf.plot_to_phys(ui::scope_interaction::sample(&trace.mean_points, ax, step)?),
                tf.plot_to_phys(ui::scope_interaction::sample(&trace.mean_points, bx, step)?),
            ))
        };
        cached
            .mapped_traces
            .iter()
            .enumerate()
            .filter_map(|(index, trace)| {
                let channel_index = index / traces_per_channel;
                let channel = self.channels.get(channel_index)?;
                let stats = channel_stats.get(channel_index).copied().flatten();
                let style = self.config.style(&channel.signal_id);
                let pair = measurement_pair(index);
                let label = if is_comparison {
                    format!(
                        "{} ({})",
                        ui::scope_legend::short_label(channel),
                        comparison[index % traces_per_channel].name
                    )
                } else if channel.is_derived {
                    format!("ƒ {}", ui::scope_legend::short_label(channel))
                } else {
                    ui::scope_legend::short_label(channel).to_owned()
                };
                let value = if let Some((a, b)) = pair {
                    Some(format!("Δ ({:.3} {})", b - a, channel.unit))
                } else if self.measurements.enabled
                    && self.measurements.pins.iter().all(Option::is_some)
                {
                    Some("No measurement".to_owned())
                } else if !is_comparison {
                    Some(
                        stats
                            .and_then(|stats| stats.cursor)
                            .map(|value| format!("{value:.2} {}", channel.unit))
                            .unwrap_or_else(|| "No data".to_owned()),
                    )
                } else {
                    None
                };
                Some(ui::scope_legend::Entry {
                    channel_index,
                    label,
                    signal_id: channel.signal_id.clone(),
                    value,
                    color: trace.color,
                    style: style.mode.line_style(trace.style),
                    stats: if is_comparison { None } else { stats },
                    visible: !self.hidden_channels.contains(&channel.signal_id),
                    measurements: pair,
                })
            })
            .collect()
    }

    pub(super) fn apply_legend_action(&mut self, action: ui::scope_legend::Action) {
        match action {
            ui::scope_legend::Action::Remove(index) => self.remove_channel(index),
            ui::scope_legend::Action::Toggle(index) => self.toggle_channel_visibility(index),
            ui::scope_legend::Action::Inspect(index, tab) => {
                if let Some(channel) = self.channels.get(index) {
                    self.inspector = Some((channel.signal_id.clone(), tab));
                }
            }
        }
    }

    pub(super) fn render_plots(
        &mut self,
        ui: &mut egui::Ui,
        view: &workspace_view::WorkspaceDataView<'_>,
        comparison: &[daqcore::storage::Annotation],
        entries: &[ui::scope_legend::Entry],
    ) -> egui_tiles::UiResponse {
        let Some(cached) = self.cached_data.as_ref() else {
            return egui_tiles::UiResponse::None;
        };
        let is_comparison = comparison.len() >= 2;
        let x_mode = self.x_axis_mode;
        let start_ms = view.timeline.start_utc_ms();
        let end_ms = view.timeline.end_utc_ms();
        let reference_ms = if is_comparison {
            comparison[0].start_utc_ms
        } else {
            start_ms
        };
        let max_x = cached.max_x_bound;
        let to_ms =
            |x: f64| match x_mode {
                ScopeXAxisMode::TrackDistance => cached.primary_distance_lookup.as_ref().and_then(
                    |(_, timestamps, distances)| {
                        interpolate_ms_at_distance(timestamps, distances, x)
                    },
                ),
                _ => Some(reference_ms.saturating_add((x * 1000.0).round() as i64)),
            };
        let to_x =
            |time: i64| match x_mode {
                ScopeXAxisMode::TrackDistance => cached.primary_distance_lookup.as_ref().and_then(
                    |(_, timestamps, distances)| {
                        interpolate_distance_at_ms(timestamps, distances, time)
                    },
                ),
                _ => Some((time - reference_ms) as f64 / 1000.0),
            };
        let traces_per_channel = if is_comparison { comparison.len() } else { 1 };
        let lane_count = if self.config.stacked {
            self.config.lanes
        } else {
            1
        };
        let axis_views = axes(cached, &self.config, &self.channels);
        let slots = (0..lane_count).fold((0usize, 0usize), |(left, right), lane| {
            (
                left.max(
                    axis_views
                        .iter()
                        .filter(|axis| axis.lane == lane && !axis.right)
                        .count(),
                ),
                right.max(
                    axis_views
                        .iter()
                        .filter(|axis| axis.lane == lane && axis.right)
                        .count(),
                ),
            )
        });
        let slots = if slots == (0, 0) { (1, 0) } else { slots };
        let plot_width = ui.available_width().max(1.0);
        let height =
            ((ui.available_height() - (lane_count - 1) as f32 * 4.0) / lane_count as f32).max(1.0);
        let mut actions = Vec::new();
        let mut move_signal = None;
        let mut external_signal = None;
        self.plot_frames.clear();
        for lane in 0..lane_count {
            let legend_entries: Vec<_> = entries
                .iter()
                .filter(|entry| self.config.lane(&entry.signal_id) == lane)
                .cloned()
                .collect();
            let legend_instance = if lane_count == 1 {
                self.instance_num
            } else {
                egui::Id::new(("scope_lane_legend", self.instance_num, lane)).value() as usize
            };
            let plot_id = (
                "scope_plot_v3",
                self.instance_num,
                lane,
                is_comparison,
                x_mode as u8,
            );
            let (active_min, active_max) = if lane_count == 1 {
                self.y_bounds_override
            } else {
                self.lane_y_overrides.get(&lane).copied()
            }
            .unwrap_or((cached.primary_min, cached.primary_max));
            let mut next_y_override = if lane_count == 1 {
                self.y_bounds_override
            } else {
                self.lane_y_overrides.get(&lane).copied()
            };
            let plot = egui_plot::Plot::new(plot_id)
                .custom_y_axes(hints(cached, &axis_views, lane, lane_count, slots))
                .width(plot_width)
                .height(height)
                .min_size(egui::vec2(1.0, 1.0))
                .show_axes([lane + 1 == lane_count, true])
                .allow_zoom(false)
                .allow_drag(false)
                .allow_scroll(false)
                .allow_boxed_zoom(false)
                .allow_double_click_reset(false)
                .x_axis_formatter(move |mark, _| match (is_comparison, x_mode) {
                    (_, ScopeXAxisMode::TrackDistance) => format!("{:.0} m", mark.value),
                    (true, _) | (_, ScopeXAxisMode::ElapsedTime) => format!("{:.1}s", mark.value),
                    _ => daqcore::time::Time::from_unix_millis(reference_ms)
                        .offset((mark.value * 1000.0).round() as i64)
                        .label(),
                });
            let window_s = ((end_ms - start_ms).max(1) as f64) / 1000.0;
            let response = plot.show(ui, |plot| {
                plot.set_plot_bounds_x(0.0..=max_x);
                plot.set_plot_bounds_y(active_min..=active_max);
                for (index, trace) in cached.mapped_traces.iter().enumerate() {
                    let channel_index = index / traces_per_channel;
                    let Some(channel) = self.channels.get(channel_index) else {
                        continue;
                    };
                    if self.config.lane(&channel.signal_id) != lane
                        || self.hidden_channels.contains(&channel.signal_id)
                    {
                        continue;
                    }
                    let style = self.config.style(&channel.signal_id);
                    let effective_width =
                        adaptive_trace_width(style.width, plot_width, height, window_s);
                    let color = trace.color.gamma_multiply(style.opacity);
                    if style.envelope && style.mode != TraceMode::Points {
                        for quad in &trace.envelope_bands {
                            plot.polygon(
                                egui_plot::Polygon::new("", quad.to_vec())
                                    .fill_color(color.gamma_multiply(0.22))
                                    .stroke(egui::Stroke::new(0.5_f32, color.gamma_multiply(0.35)))
                                    .allow_hover(false),
                            );
                        }
                    }
                    if style.mode != TraceMode::Points {
                        let points = if style.mode == TraceMode::Steps {
                            ui::scope_settings::steps(&trace.mean_points)
                        } else {
                            trace.mean_points.clone()
                        };
                        plot.line(
                            egui_plot::Line::new(trace.name.clone(), points)
                                .color(color)
                                .width(effective_width)
                                .style(style.mode.line_style(trace.style)),
                        );
                    }
                    if style.points || style.mode == TraceMode::Points {
                        let points: Vec<_> = trace
                            .mean_points
                            .iter()
                            .copied()
                            .filter(|point| point[0].is_finite() && point[1].is_finite())
                            .collect();
                        plot.points(
                            egui_plot::Points::new(trace.name.clone(), points)
                                .color(color)
                                .radius(effective_width + 1.0),
                        );
                    }
                }
                let theme = ui::theme::get_theme(plot.ctx());
                if let Some((sel_start_ms, sel_end_ms)) = self.measurements.selection
                    && sel_end_ms > sel_start_ms
                    && let (Some(raw_a), Some(raw_b)) = (to_x(sel_start_ms), to_x(sel_end_ms))
                {
                    let (a, b) = (raw_a.min(raw_b).max(0.0), raw_a.max(raw_b).min(max_x));
                    if b > a {
                        let bounds = plot.plot_bounds();
                        let sel_color = Color32::from_rgb(245, 158, 11);
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
                            .fill_color(Color32::from_rgba_unmultiplied(245, 158, 11, 28))
                            .stroke(egui::Stroke::new(1.2_f32, sel_color))
                            .allow_hover(false),
                        );
                    }
                }
                if !is_comparison && let Some(x) = to_x(view.timeline.setpoint_utc_ms()) {
                    plot.vline(
                        egui_plot::VLine::new("", x)
                            .color(theme.playhead_color())
                            .width(2.8_f32),
                    );
                }
                let hover_x = if x_mode == ScopeXAxisMode::TrackDistance {
                    view.hover_distance_m()
                        .or_else(|| view.hover_utc_ms().and_then(to_x))
                } else {
                    view.hover_utc_ms().and_then(to_x)
                };
                if let Some(x) = hover_x
                    && x >= 0.0
                    && x <= max_x
                {
                    plot.vline(
                        egui_plot::VLine::new("", x)
                            .color(theme.hover_cursor_color())
                            .style(egui_plot::LineStyle::dashed_dense()),
                    );
                }
                let frame = *plot.transform().frame();
                if lane_count > 1 {
                    let point =
                        plot.plot_from_screen(egui::pos2(frame.left() + 8.0, frame.top() + 6.0));
                    plot.text(
                        egui_plot::Text::new(
                            "",
                            point,
                            RichText::new(format!("Plot ({})", lane + 1)).size(10.0),
                        )
                        .anchor(egui::Align2::LEFT_TOP),
                    );
                }
                let legend_interaction = self.show_legend
                    && ui::scope_legend::bounds(
                        frame,
                        legend_entries.len(),
                        self.show_inline_stats,
                    )
                    .is_some_and(|rect| {
                        plot.ctx().input(|input| {
                            input
                                .pointer
                                .hover_pos()
                                .is_some_and(|pos| rect.contains(pos))
                                || input
                                    .pointer
                                    .press_origin()
                                    .is_some_and(|pos| rect.contains(pos))
                        })
                    });
                let scrolling = plot
                    .ctx()
                    .input(|input| input.smooth_scroll_delta != egui::Vec2::ZERO);
                let scroll_id = egui::Id::new(("scope_legend_scroll_capture", self.instance_num));
                let captured = plot.ctx().data_mut(|data| {
                    let captured = scrolling
                        && (legend_interaction
                            || data.get_temp::<bool>(scroll_id).unwrap_or(false));
                    data.insert_temp(scroll_id, captured);
                    captured
                });
                let dragging_signal = egui::DragAndDrop::has_any_payload(plot.ctx());
                let blocked =
                    egui::Popup::is_any_open(plot.ctx()) || legend_interaction || dragging_signal;
                let mut pointer_time = None;
                let mut pointer_y = None;
                if !blocked
                    && plot.response().hovered()
                    && let Some(point) = plot.pointer_coordinate()
                {
                    pointer_y = Some(point.y);
                    if let Some(time) = to_ms(point.x.clamp(0.0, max_x)) {
                        view.set_hover_utc_ms(Some(time));
                        pointer_time = Some(time);
                        if x_mode == ScopeXAxisMode::TrackDistance {
                            view.set_hover_distance_m(Some(point.x));
                        }
                    }
                }
                if !blocked && !self.measurements.enabled && plot.response().double_clicked() {
                    next_y_override = None;
                    plot.set_plot_bounds_y(cached.primary_min..=cached.primary_max);
                }
                if !blocked && plot.response().hovered() && !captured {
                    let (scroll, zoom, shift) = plot.ctx().input(|input| {
                        (
                            input.smooth_scroll_delta,
                            input.zoom_delta(),
                            input.modifiers.shift,
                        )
                    });
                    let anchor = pointer_time.unwrap_or(view.timeline.setpoint_utc_ms());
                    if shift && scroll.y != 0.0 {
                        let span = ((active_max - active_min) * ui::scroll_zoom_factor(scroll.y))
                            .clamp(1e-4, 1e9);
                        let y = pointer_y
                            .unwrap_or((active_min + active_max) * 0.5)
                            .clamp(active_min, active_max);
                        let fraction = (y - active_min) / (active_max - active_min).max(1e-9);
                        let lo = y - fraction * span;
                        next_y_override = Some((lo, lo + span));
                    } else if (zoom - 1.0).abs() > 1e-3 {
                        view.request_zoom_around_ms(
                            anchor,
                            (1.0 / f64::from(zoom)).clamp(0.25, 4.0),
                        );
                    } else if scroll.y != 0.0 {
                        view.request_zoom_around_ms(anchor, ui::scroll_zoom_factor(scroll.y));
                    }
                    if scroll.x.abs() > 0.5 {
                        view.request_pan_delta_ms(
                            (-(scroll.x as f64) / frame.width().max(1.0) as f64
                                * (end_ms - start_ms) as f64)
                                .round() as i64,
                        );
                    }
                }
                ui::scope_interaction::handle(
                    plot,
                    view,
                    &mut self.gesture,
                    &mut self.measurements,
                    lane,
                    &to_ms,
                    &to_x,
                    max_x,
                    blocked || !plot.response().contains_pointer(),
                );
            });
            self.plot_frames.push(*response.transform.frame());
            if let Some((sel_start_ms, sel_end_ms)) = self.measurements.selection
                && sel_end_ms > sel_start_ms
            {
                self.draw_selection_stats_overlay(
                    ui,
                    *response.transform.frame(),
                    cached,
                    lane,
                    traces_per_channel,
                    sel_start_ms,
                    sel_end_ms,
                    &to_x,
                );
            }
            if let Some(payload) =
                egui::DragAndDrop::payload::<ui::signal_browser::DraggedSignal>(ui.ctx())
                && ui.input(|input| {
                    input
                        .pointer
                        .hover_pos()
                        .is_some_and(|pos| response.transform.frame().contains(pos))
                })
            {
                ui.painter().rect_stroke(
                    *response.transform.frame(),
                    2.0,
                    egui::Stroke::new(2.0_f32, Color32::LIGHT_BLUE),
                    egui::StrokeKind::Inside,
                );
                if ui.input(|input| input.pointer.any_released()) {
                    external_signal = Some(((*payload).clone(), lane));
                    egui::DragAndDrop::take_payload::<ui::signal_browser::DraggedSignal>(ui.ctx());
                }
            }
            if lane_count == 1 {
                self.y_bounds_override = next_y_override;
            } else if let Some(range) = next_y_override {
                self.lane_y_overrides.insert(lane, range);
            } else {
                self.lane_y_overrides.remove(&lane);
            }
            if let Some(payload) =
                egui::DragAndDrop::payload::<ui::scope_legend::SignalDrag>(ui.ctx())
                && payload.scope == self.instance_num
                && ui.input(|input| {
                    input
                        .pointer
                        .hover_pos()
                        .is_some_and(|pos| response.transform.frame().contains(pos))
                })
            {
                ui.painter().rect_stroke(
                    *response.transform.frame(),
                    2.0,
                    egui::Stroke::new(2.0_f32, Color32::LIGHT_BLUE),
                    egui::StrokeKind::Inside,
                );
                if ui.input(|input| input.pointer.any_released()) {
                    move_signal = Some((payload.signal.clone(), lane));
                    egui::DragAndDrop::take_payload::<ui::scope_legend::SignalDrag>(ui.ctx());
                }
            }
            if self.show_legend
                && let Some(rect) = ui::scope_legend::bounds(
                    *response.transform.frame(),
                    legend_entries.len(),
                    self.show_inline_stats,
                )
                && let Some(action) = ui::scope_legend::show(
                    ui,
                    rect,
                    legend_instance,
                    &legend_entries,
                    self.show_inline_stats,
                    self.instance_num,
                )
            {
                actions.push(action);
            }
            if lane + 1 < lane_count {
                ui.add_space(4.0);
            }
        }
        if let Some((payload, lane)) = external_signal {
            let signal = payload.signal_id();
            self.apply_dragged_signal(payload);
            self.config.channels.entry(signal).or_default().lane = lane;
            self.cached_data = None;
        }
        if let Some((signal, lane)) = move_signal {
            self.config.channels.entry(signal).or_default().lane = lane;
            self.cached_data = None;
        }
        for action in actions {
            self.apply_legend_action(action);
        }
        self.show_inspector(ui.ctx());
        egui_tiles::UiResponse::None
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_selection_stats_overlay(
        &self,
        ui: &mut egui::Ui,
        frame: egui::Rect,
        cached: &CachedScopeData,
        lane: usize,
        traces_per_channel: usize,
        sel_start_ms: i64,
        sel_end_ms: i64,
        to_x: &impl Fn(i64) -> Option<f64>,
    ) {
        let mut rows: Vec<(String, Color32, String, SectionStats)> = Vec::new();
        let (x0, x1) = match (to_x(sel_start_ms), to_x(sel_end_ms)) {
            (Some(a), Some(b)) => (a.min(b), a.max(b)),
            _ => (0.0, 0.0),
        };

        for (ch_idx, channel) in self.channels.iter().enumerate() {
            if self.config.lane(&channel.signal_id) != lane
                || self.hidden_channels.contains(&channel.signal_id)
            {
                continue;
            }
            let stats = cached
                .channel_cursor_lookups
                .get(ch_idx)
                .and_then(|lookup| lookup.section_stats(sel_start_ms, sel_end_ms))
                .or_else(|| {
                    let trace = cached.mapped_traces.get(ch_idx * traces_per_channel)?;
                    let step = self.config.style(&channel.signal_id).mode == TraceMode::Steps;
                    let tf = cached.unit_transforms[trace.unit_idx];
                    let v0 =
                        tf.plot_to_phys(ui::scope_interaction::sample(&trace.mean_points, x0, step)?);
                    let v1 =
                        tf.plot_to_phys(ui::scope_interaction::sample(&trace.mean_points, x1, step)?);
                    compute_section_stats(&[sel_start_ms, sel_end_ms], &[v0, v1])
                });
            if let Some(st) = stats {
                rows.push((
                    ui::scope_legend::short_label(channel).to_owned(),
                    channel.color,
                    channel.unit.clone(),
                    st,
                ));
            }
        }

        if rows.is_empty() {
            return;
        }

        let theme = ui::theme::get_theme(ui.ctx());
        let dt_s = (sel_end_ms - sel_start_ms) as f64 / 1000.0;
        let font = egui::FontId::monospace(10.5);
        let painter = ui.painter_at(frame);

        let header_text = format!("Selection Δt = {dt_s:.3} s  (Esc to clear)");
        let header_galley =
            painter.layout_no_wrap(header_text, font.clone(), ui.visuals().strong_text_color());
        let mut max_w = header_galley.size().x;
        let row_h = 15.0_f32;
        let mut formatted_rows: Vec<(Color32, std::sync::Arc<egui::Galley>)> =
            Vec::with_capacity(rows.len());

        for (label, color, unit, st) in rows {
            let unit_suffix = if unit.is_empty() {
                String::new()
            } else {
                format!(" {unit}")
            };
            let line = format!(
                "{label}: min {:.2} | max {:.2} | avg {:.2} | sum {:.2} | Δ {:+.2}{unit_suffix}",
                st.min, st.max, st.avg, st.sum, st.delta
            );
            let galley = painter.layout_no_wrap(line, font.clone(), color);
            max_w = max_w.max(galley.size().x);
            formatted_rows.push((color, galley));
        }

        let pad = egui::vec2(8.0, 6.0);
        let box_w = (max_w + pad.x * 2.0).min((frame.width() - 16.0).max(80.0));
        let box_h = (formatted_rows.len() as f32 + 1.0) * row_h + pad.y * 2.0;
        let box_rect = egui::Rect::from_min_size(
            egui::pos2(frame.left() + 8.0, frame.top() + 8.0),
            egui::vec2(box_w, box_h),
        );

        let bg = theme.panel_color().gamma_multiply(0.92);
        painter.rect(
            box_rect,
            4.0,
            bg,
            egui::Stroke::new(1.0_f32, Color32::from_rgb(245, 158, 11)),
            egui::StrokeKind::Inside,
        );

        let clipped = ui.painter_at(box_rect.shrink2(egui::vec2(4.0, 2.0)).intersect(frame));
        let mut y = box_rect.top() + pad.y;
        clipped.galley(
            egui::pos2(box_rect.left() + pad.x, y),
            header_galley,
            ui.visuals().strong_text_color(),
        );
        y += row_h;

        for (color, galley) in formatted_rows {
            clipped.galley(egui::pos2(box_rect.left() + pad.x, y), galley, color);
            y += row_h;
        }
    }

    fn show_inspector(&mut self, ctx: &egui::Context) {
        let Some((signal, mut tab)) = self.inspector.clone() else {
            return;
        };
        let Some(index) = self
            .channels
            .iter()
            .position(|channel| channel.signal_id == signal)
        else {
            self.inspector = None;
            return;
        };
        let key = self.config.axis_key(&self.channels[index]);
        let auto_range = self
            .cached_data
            .as_ref()
            .and_then(|cached| {
                let index = cached.unit_groups.iter().position(|group| group == &key)?;
                let tf = cached.unit_transforms[index];
                Some((
                    tf.plot_to_phys(cached.primary_min),
                    tf.plot_to_phys(cached.primary_max),
                ))
            })
            .unwrap_or((-1.0, 1.0));
        let mut open = true;
        let (update, axis) = ui::scope_settings::inspector(
            ctx,
            self.instance_num,
            index,
            &self.channels,
            &self.config,
            &mut tab,
            &mut open,
            auto_range,
        );
        self.inspector = open.then_some((signal, tab));
        if let Some(update) = update
            && let Some(channel) = self.channels.get_mut(update.index)
        {
            channel.color = Color32::from_rgb(update.color[0], update.color[1], update.color[2]);
            if update.style.lane > 0 {
                self.config.stacked = true;
            }
            self.config
                .channels
                .insert(channel.signal_id.clone(), update.style);
            self.config.sanitize();
            self.cached_data = None;
        }
        if let Some((key, axis)) = axis {
            self.config.axes.insert(key, axis);
            self.y_bounds_override = None;
            self.lane_y_overrides.clear();
            self.cached_data = None;
        }
    }
}
