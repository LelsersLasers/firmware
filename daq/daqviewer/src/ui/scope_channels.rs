//! Search and assignment panel opened by the channels (ƒ) icon, plus inline discrete state strips.

use super::*;
use eframe::egui;
use ui::scope_settings::Destination;

/// State rows share the exact horizontal transform of the parent plot.
#[allow(clippy::too_many_arguments)]
pub(super) fn state_strips(
    ui: &mut egui::Ui,
    view: &workspace_view::WorkspaceDataView<'_>,
    channels: &[ScopeChannel],
    config: &ui::scope_settings::ScopeConfig,
    lane: usize,
    scope_instance: usize,
    frame: egui::Rect,
    to_x: &impl Fn(i64) -> Option<f64>,
    to_ms: &impl Fn(f64) -> Option<i64>,
    max_x: f64,
    comparison: bool,
) -> egui::Rect {
    use egui::Color32;
    let mut bounds = egui::Rect::NOTHING;
    let selected: Vec<_> = channels
        .iter()
        .filter(|ch| config.lane(&ch.signal_id) == lane && config.style(&ch.signal_id).discrete)
        .collect();
    for channel in selected
        .iter()
        .map(|ch| Some(*ch))
        .chain(selected.is_empty().then_some(None))
    {
        let (allocated, response) = ui
            .push_id(
                (
                    "discrete",
                    scope_instance,
                    lane,
                    channel.map(|ch| &ch.signal_id),
                ),
                |ui| {
                    ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 34.0),
                        if channel.is_some() {
                            egui::Sense::click_and_drag()
                        } else {
                            egui::Sense::hover()
                        },
                    )
                },
            )
            .inner;
        let row = egui::Rect::from_min_max(
            egui::pos2(frame.left(), allocated.top()),
            egui::pos2(frame.right(), allocated.bottom()),
        );
        bounds = bounds.union(row);
        let Some(channel) = channel else { continue };
        response.dnd_set_drag_payload(ui::scope_legend::SignalDrag::new(
            scope_instance,
            channel,
            config,
        ));
        let painter = ui.painter().with_clip_rect(row.intersect(ui.clip_rect()));
        painter.text(
            row.left_top(),
            egui::Align2::LEFT_TOP,
            ui::scope_legend::short_label(channel),
            egui::FontId::proportional(10.0),
            channel.color,
        );
        let strip =
            egui::Rect::from_min_max(row.left_top() + egui::vec2(0.0, 13.0), row.right_bottom());
        painter.rect_filled(strip, 2.0, ui.visuals().faint_bg_color);
        if comparison {
            ui.put(
                egui::Rect::from_center_size(strip.center(), egui::vec2(18.0, 18.0)),
                egui::Label::new("!").sense(egui::Sense::hover()),
            )
            .on_hover_text("Unavailable in comparison mode");
            continue;
        }
        let intervals = view.query_states(
            &channel.signal_id,
            view.timeline.start_utc_ms()..=view.timeline.end_utc_ms(),
        );
        let fault = ui::state_lane::is_likely_fault_signal(&channel.signal_id);
        if intervals.is_empty() {
            painter.text(
                strip.center(),
                egui::Align2::CENTER_CENTER,
                "No state data",
                egui::FontId::proportional(10.0),
                ui.visuals().weak_text_color(),
            );
        }
        let x_pixel = |x: f64| frame.left() + (x / max_x.max(0.001)) as f32 * frame.width();
        for interval in intervals {
            let (Some(start), Some(end)) = (to_x(interval.start_utc_ms), to_x(interval.end_utc_ms))
            else {
                continue;
            };
            let rect = egui::Rect::from_min_max(
                egui::pos2(x_pixel(start.max(0.0)), strip.top()),
                egui::pos2(x_pixel(end.min(max_x)), strip.bottom()),
            );
            if !rect.is_positive() {
                continue;
            }
            let label = ui::state_lane::resolve_state_label(&channel.signal_id, &interval, fault);
            let color = ui::state_lane::state_color_for_interval(&label, interval.raw_int, fault);
            painter.rect_filled(rect, 1.0, color.gamma_multiply(0.65));
            let text = painter.layout_no_wrap(
                label.clone(),
                egui::FontId::proportional(10.0),
                Color32::WHITE,
            );
            if text.size().x + 6.0 < rect.width() {
                painter.galley(rect.center() - text.size() * 0.5, text, Color32::WHITE);
            }
        }
        let theme = ui::theme::get_theme(ui.ctx());
        if let Some(x) = to_x(view.active_cursor_utc_ms())
            && (0.0..=max_x).contains(&x)
        {
            painter.line_segment(
                [
                    egui::pos2(x_pixel(x), strip.top()),
                    egui::pos2(x_pixel(x), strip.bottom()),
                ],
                egui::Stroke::new(1.5_f32, theme.playhead_color()),
            );
        }
        if let Some(pos) = response.hover_pos()
            && let Some(time) =
                to_ms(((pos.x - frame.left()) / frame.width().max(1.0)) as f64 * max_x)
        {
            view.set_hover_utc_ms(Some(time));
            if response.clicked() {
                view.request_setpoint_utc_ms(time);
            }
        }
    }
    bounds
}

const PENDING_MOVE_SCOPES_KEY: &str = "scope_signal_move_pending_scopes";

pub(super) fn has_pending_signal_moves(ctx: &egui::Context) -> bool {
    ctx.data(|data| {
        data.get_temp::<std::collections::BTreeSet<usize>>(egui::Id::new(PENDING_MOVE_SCOPES_KEY))
            .is_some_and(|pending| !pending.is_empty())
    })
}

impl Scope {
    pub(super) fn process_signal_moves(&mut self, ctx: &egui::Context) {
        let id = egui::Id::new(("scope_signal_moves", self.instance_num));
        let signals = ctx
            .data_mut(|data| {
                let pending_id = egui::Id::new(PENDING_MOVE_SCOPES_KEY);
                if let Some(mut pending) =
                    data.get_temp::<std::collections::BTreeSet<usize>>(pending_id)
                {
                    pending.remove(&self.instance_num);
                    if pending.is_empty() {
                        data.remove::<std::collections::BTreeSet<usize>>(pending_id);
                    } else {
                        data.insert_temp(pending_id, pending);
                    }
                }
                data.remove_temp::<Vec<String>>(id)
            })
            .unwrap_or_default();
        for signal in signals {
            if let Some(index) = self
                .channels
                .iter()
                .position(|channel| channel.signal_id == signal)
            {
                self.remove_channel(index);
                self.cached_data = None;
            }
        }
    }

    pub(super) fn accept_scope_signal(
        &mut self,
        ctx: &egui::Context,
        payload: ui::scope_legend::SignalDrag,
        lane: usize,
        destination: Destination,
    ) {
        let signal = payload.channel.signal_id.clone();
        let existing = self
            .channels
            .iter()
            .position(|channel| channel.signal_id == signal);
        let index = existing.unwrap_or_else(|| {
            self.channels.push(payload.channel.clone());
            self.config
                .channels
                .insert(signal.clone(), payload.style.clone());
            self.channels.len() - 1
        });
        self.config.assign(&self.channels[index], lane, destination);
        // A transferred custom scale gets its own axis rather than changing
        // the scale of signals already assigned at the destination.
        if (existing.is_none() || payload.scope == self.instance_num)
            && destination != Destination::Discrete
            && (payload.axis.range.is_some()
                || payload.axis.color.is_some()
                || !payload.axis.label.is_empty())
        {
            let side = if destination == Destination::Right {
                "right"
            } else {
                "left"
            };
            let axis = format!("{side}:{signal}");
            self.config.channels.entry(signal.clone()).or_default().axis = axis.clone();
            let mut settings = payload.axis;
            settings.side = if destination == Destination::Right {
                ui::scope_settings::AxisSide::Right
            } else {
                ui::scope_settings::AxisSide::Left
            };
            self.config.axes.insert(format!("{lane}|{axis}"), settings);
        }
        self.hidden_channels.remove(&signal);
        self.cached_data = None;
        self.refresh_title();
        if payload.scope != self.instance_num {
            let id = egui::Id::new(("scope_signal_moves", payload.scope));
            let pending_id = egui::Id::new(PENDING_MOVE_SCOPES_KEY);
            ctx.data_mut(|data| {
                let mut signals = data.get_temp::<Vec<String>>(id).unwrap_or_default();
                if !signals.contains(&signal) {
                    signals.push(signal);
                }
                data.insert_temp(id, signals);
                let mut pending = data
                    .get_temp::<std::collections::BTreeSet<usize>>(pending_id)
                    .unwrap_or_default();
                pending.insert(payload.scope);
                data.insert_temp(pending_id, pending);
            });
            ctx.request_repaint();
        }
    }

    pub(super) fn show_channel_selector(
        &mut self,
        ctx: &egui::Context,
        view: &workspace_view::WorkspaceDataView<'_>,
    ) {
        let Some(target) = self.picker_target else {
            return;
        };
        let speed = target == ScopePickerTarget::SpeedSignal;
        let (mut lane, mut destination) = match target {
            ScopePickerTarget::Channels(lane, destination) => (lane, destination),
            _ => (0, Destination::Left),
        };
        let mut open = !ctx.input(|input| input.key_pressed(egui::Key::Escape));
        let mut action = ui::message_picker::SignalPickerAction::None;
        let sides = self.config.resolved_axis_sides(&self.channels);
        let mut remove = None;
        egui::Window::new(if speed { "Speed signal" } else { "Channels" })
            .id(egui::Id::new(("scope_channels", self.instance_num)))
            .open(&mut open)
            .default_size(egui::vec2(520.0, 440.0))
            .show(ctx, |ui| {
                if !speed {
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("plot")
                            .selected_text(self.plot_name(lane))
                            .show_ui(ui, |ui| {
                                let count = if self.config.stacked {
                                    self.config.lanes
                                } else {
                                    1
                                };
                                for index in 0..count {
                                    ui.selectable_value(&mut lane, index, self.plot_name(index));
                                }
                            });
                        for side in [Destination::Left, Destination::Right, Destination::Discrete] {
                            ui.selectable_value(&mut destination, side, side.label());
                        }
                    });
                    egui::ScrollArea::vertical()
                        .id_salt("assigned_channels")
                        .max_height(130.0)
                        .show(ui, |ui| {
                            for (index, channel) in self.channels.iter().enumerate() {
                                if self.config.lane(&channel.signal_id) != lane {
                                    continue;
                                }
                                ui.push_id(index, |ui| {
                                    ui.horizontal(|ui| {
                                        if ui
                                            .small_button("×")
                                            .on_hover_text("Remove signal from plot")
                                            .clicked()
                                        {
                                            remove = Some(index);
                                        }
                                        let current = if sides.get(&self.config.axis_key(channel))
                                            == Some(&ui::scope_settings::AxisSide::Right)
                                        {
                                            Destination::Right
                                        } else {
                                            self.config.destination(channel)
                                        };
                                        let mut assigned = current;
                                        egui::ComboBox::from_id_salt("assignment")
                                            .selected_text(assigned.label())
                                            .show_ui(ui, |ui| {
                                                for side in [
                                                    Destination::Left,
                                                    Destination::Right,
                                                    Destination::Discrete,
                                                ] {
                                                    ui.selectable_value(
                                                        &mut assigned,
                                                        side,
                                                        side.label(),
                                                    );
                                                }
                                            });
                                        if assigned != current {
                                            self.config.assign(channel, lane, assigned);
                                        }
                                        ui.add(egui::Label::new(&channel.signal_id).truncate());
                                    });
                                });
                            }
                        });
                    ui.separator();
                }
                action = ui::message_picker::show_signal_picker(
                    ui,
                    ("scope_channel_search", self.instance_num),
                    "Search",
                    false,
                    !speed && destination == Destination::Discrete,
                    &mut self.search_query,
                    view,
                );
            });
        if let Some(index) = remove {
            self.remove_channel(index);
        }
        self.picker_target = if open {
            Some(if speed {
                ScopePickerTarget::SpeedSignal
            } else {
                ScopePickerTarget::Channels(lane, destination)
            })
        } else {
            None
        };
        if let ui::message_picker::SignalPickerAction::Picked(picked) = action {
            if speed {
                self.speed_signal_id = picked.signal_id;
                self.picker_target = None;
            } else {
                let target = self.picker_target;
                let signal = picked.signal_id.clone();
                self.add_channel(
                    picked.signal_id.clone(),
                    picked.signal_id,
                    picked.unit,
                    picked.key,
                    picked.is_derived,
                );
                if let Some(channel) = self
                    .channels
                    .iter()
                    .find(|channel| channel.signal_id == signal)
                {
                    self.config.assign(channel, lane, destination);
                }
                self.picker_target = target;
            }
            self.cached_data = None;
        }
    }

    pub(super) fn plot_name(&self, lane: usize) -> String {
        self.config
            .plot_names
            .get(&lane)
            .filter(|name| !name.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| format!("Plot {}", lane + 1))
    }
}
