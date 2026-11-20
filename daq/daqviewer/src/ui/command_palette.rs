use crate::action;

#[derive(Default)]
pub struct CommandPalette {
    show_command_palette: bool,
    palette_search: String,
    palette_index: usize,
}

impl CommandPalette {
    pub fn new() -> Self {
        Self {
            show_command_palette: false,
            palette_search: String::new(),
            palette_index: 0,
        }
    }

    pub fn toggle(&mut self) {
        self.show_command_palette = !self.show_command_palette;
        if self.show_command_palette {
            self.palette_search.clear();
            self.palette_index = 0;
        }
    }

    pub fn ui(&mut self, ctx: &eframe::egui::Context) -> Vec<action::AppAction> {
        self.ui_for(ctx, true)
    }

    pub fn ui_for(
        &mut self,
        ctx: &eframe::egui::Context,
        supports_hardware_widgets: bool,
    ) -> Vec<action::AppAction> {
        if !self.show_command_palette {
            return Vec::new();
        }

        let mut action_queue = Vec::new();

        let cmd_list = action::AppAction::cmd_palette_actions_for(supports_hardware_widgets);

        // filtering Logic
        let filtered_options: Vec<_> = cmd_list
            .iter()
            .filter(|(label, _)| {
                if self.palette_search.is_empty() {
                    true
                } else {
                    label
                        .to_lowercase()
                        .contains(&self.palette_search.to_lowercase())
                }
            })
            .collect();

        if !filtered_options.is_empty() {
            self.palette_index = self.palette_index.min(filtered_options.len() - 1);
        }

        ctx.input_mut(|i| {
            if i.key_pressed(eframe::egui::Key::ArrowDown) && !filtered_options.is_empty() {
                self.palette_index = (self.palette_index + 1) % filtered_options.len();
            }

            if i.key_pressed(eframe::egui::Key::ArrowUp) && !filtered_options.is_empty() {
                self.palette_index =
                    (self.palette_index + filtered_options.len() - 1) % filtered_options.len();
            }

            if i.key_pressed(eframe::egui::Key::Enter) && !filtered_options.is_empty() {
                action_queue.push(filtered_options[self.palette_index].1.clone());
                self.show_command_palette = false;
            }

            if i.key_pressed(eframe::egui::Key::Escape) {
                self.show_command_palette = false;
            }
        });

        eframe::egui::Window::new("Command Palette")
            .anchor(eframe::egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .collapsible(false)
            .resizable(false)
            .title_bar(false)
            .fixed_size([400.0, 420.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("🔍");
                    let response = ui.add(
                        eframe::egui::TextEdit::singleline(&mut self.palette_search)
                            .desired_width(ui.available_width().max(60.0)),
                    );

                    response.request_focus();

                    if response.changed() {
                        self.palette_index = 0;
                    }
                });
                ui.separator();

                eframe::egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.vertical_centered_justified(|ui| {
                            if filtered_options.is_empty() {
                                ui.label("No commands found");
                            } else {
                                for (i, (label, cmd_action)) in filtered_options.iter().enumerate()
                                {
                                    let is_selected = i == self.palette_index;
                                    let selection = ui.visuals().selection;
                                    let fill = if is_selected {
                                        selection.bg_fill
                                    } else {
                                        eframe::egui::Color32::TRANSPARENT
                                    };

                                    let stroke = if is_selected {
                                        selection.stroke
                                    } else {
                                        eframe::egui::Stroke::NONE
                                    };

                                    let button =
                                        eframe::egui::Button::new(*label).fill(fill).stroke(stroke);

                                    let response = ui.add(button);
                                    if response.clicked() {
                                        action_queue.push(cmd_action.clone());
                                        self.show_command_palette = false;
                                    }
                                }
                            }
                        });
                    });
            });

        action_queue
    }
}
