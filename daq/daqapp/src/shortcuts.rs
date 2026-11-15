use crate::action;

pub struct ShortcutHandler;

impl ShortcutHandler {
    pub fn check_shortcuts(ctx: &eframe::egui::Context) -> Vec<action::AppAction> {
        let mut actions = Vec::new();

        // Get input state
        let input = ctx.input(|i| i.clone());

        // CMD+S = toggle sidebar
        if input.modifiers.command_only() && input.key_pressed(eframe::egui::Key::S) {
            actions.push(action::AppAction::ToggleSidebar);
        }

        // CMD+W = close window
        if input.modifiers.command_only() && input.key_pressed(eframe::egui::Key::W) {
            actions.push(action::AppAction::CloseActiveWidget);
        }

        // CMD+P = command palette
        if input.modifiers.command_only() && input.key_pressed(eframe::egui::Key::P) {
            actions.push(action::AppAction::ToggleCommandPalette);
        }

        // CMD+Plus = increase scale
        if input.modifiers.command_only() && input.key_pressed(eframe::egui::Key::Equals) {
            actions.push(action::AppAction::IncreaseScale);
        }

        // CMD+Minus = decrease scale
        if input.modifiers.command_only() && input.key_pressed(eframe::egui::Key::Minus) {
            actions.push(action::AppAction::DecreaseScale);
        }

        // When a text field or modal input does not have keyboard focus, enable vim/arrow
        // timeline navigation, playback, and search shortcuts:
        if !ctx.wants_keyboard_input() {
            // `/` = open & focus sidebar search bar
            if !input.modifiers.command
                && !input.modifiers.ctrl
                && !input.modifiers.alt
                && input.key_pressed(eframe::egui::Key::Slash)
            {
                // Strip the "/" text event from this frame so it isn't typed into the newly focused search box
                ctx.input_mut(|i| {
                    i.events
                        .retain(|e| !matches!(e, eframe::egui::Event::Text(t) if t == "/"));
                });
                actions.push(action::AppAction::FocusSidebarSearch);
            }

            // `Space` = toggle play / pause
            if !input.modifiers.any() && input.key_pressed(eframe::egui::Key::Space) {
                actions.push(action::AppAction::TogglePlayback);
            }

            // `M` = drop event bookmark / open annotation modal
            if !input.modifiers.any() && input.key_pressed(eframe::egui::Key::M) {
                actions.push(action::AppAction::OpenAnnotationModal);
            }

            // `F` = fit entire track day
            if !input.modifiers.any() && input.key_pressed(eframe::egui::Key::F) {
                actions.push(action::AppAction::FitDay);
            }

            // `G` = toggle fold idle gaps
            if !input.modifiers.any() && input.key_pressed(eframe::egui::Key::G) {
                actions.push(action::AppAction::ToggleFoldGaps);
            }

            // `[` / `]` = jump to previous / next stint
            if !input.modifiers.any() && input.key_pressed(eframe::egui::Key::OpenBracket) {
                actions.push(action::AppAction::JumpStint { direction: -1 });
            }
            if !input.modifiers.any() && input.key_pressed(eframe::egui::Key::CloseBracket) {
                actions.push(action::AppAction::JumpStint { direction: 1 });
            }

            let shift_only = input.modifiers.shift_only();
            let alt_only = input.modifiers.alt
                && !input.modifiers.command
                && !input.modifiers.ctrl
                && !input.modifiers.shift;
            let no_mods = !input.modifiers.any();

            let left_pressed = input.key_pressed(eframe::egui::Key::H)
                || input.key_pressed(eframe::egui::Key::ArrowLeft);
            let right_pressed = input.key_pressed(eframe::egui::Key::L)
                || input.key_pressed(eframe::egui::Key::ArrowRight);
            let up_pressed = input.key_pressed(eframe::egui::Key::K)
                || input.key_pressed(eframe::egui::Key::ArrowUp);
            let down_pressed = input.key_pressed(eframe::egui::Key::J)
                || input.key_pressed(eframe::egui::Key::ArrowDown);

            // `Shift + H/L` or `Shift + Left/Right` = pan viewport window left / right
            if shift_only && left_pressed {
                actions.push(action::AppAction::PanWindow { direction: -1 });
            } else if shift_only && right_pressed {
                actions.push(action::AppAction::PanWindow { direction: 1 });
            } else if (no_mods || alt_only) && left_pressed {
                // `H / Left` = move setpoint (playhead) left (`Alt` for fine 10ms step)
                actions.push(action::AppAction::StepPlayhead {
                    direction: -1,
                    fine: alt_only,
                });
            } else if (no_mods || alt_only) && right_pressed {
                // `L / Right` = move setpoint (playhead) right (`Alt` for fine 10ms step)
                actions.push(action::AppAction::StepPlayhead {
                    direction: 1,
                    fine: alt_only,
                });
            }

            // `Shift + K/J`, `Shift + Up/Down`, `K/J`, `Up/Down`, or `+/-` = shrink / enlarge window around playhead
            if (shift_only || no_mods) && up_pressed
                || (no_mods || shift_only)
                    && (input.key_pressed(eframe::egui::Key::Plus)
                        || input.key_pressed(eframe::egui::Key::Equals))
            {
                actions.push(action::AppAction::ZoomWindow { factor: 0.8 });
            } else if (shift_only || no_mods) && down_pressed
                || no_mods && input.key_pressed(eframe::egui::Key::Minus)
            {
                actions.push(action::AppAction::ZoomWindow { factor: 1.25 });
            }
        }

        actions
    }
}
