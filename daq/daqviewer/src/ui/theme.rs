const NOMINAL_DARK_THEME_PATH: &str = "themes/nominal_dark.toml";
const NOMINAL_LIGHT_THEME_PATH: &str = "themes/nominal_light.toml";
const ROSE_PINE_MOON_THEME_PATH: &str = "themes/rose_pine_moon.toml";
const NORD_THEME_PATH: &str = "themes/nord.toml";
const CATPPUCCIN_THEME_PATH: &str = "themes/catppuccin.toml";
const ONEDARK_THEME_PATH: &str = "themes/onedark.toml";

const NOMINAL_DARK_THEME_SOURCE: &str = include_str!("../../themes/nominal_dark.toml");
const NOMINAL_LIGHT_THEME_SOURCE: &str = include_str!("../../themes/nominal_light.toml");
const ROSE_PINE_MOON_THEME_SOURCE: &str = include_str!("../../themes/rose_pine_moon.toml");
const NORD_THEME_SOURCE: &str = include_str!("../../themes/nord.toml");
const CATPPUCCIN_THEME_SOURCE: &str = include_str!("../../themes/catppuccin.toml");
const ONEDARK_THEME_SOURCE: &str = include_str!("../../themes/onedark.toml");

#[derive(Copy, Clone, serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Default)]
pub enum ThemeSelection {
    #[default]
    #[serde(alias = "nominal_light", alias = "Light", alias = "light")]
    NominalLight,
    #[serde(
        alias = "NominalDark",
        alias = "nominal_dark",
        alias = "Dark",
        alias = "dark"
    )]
    Default,
    #[serde(alias = "rose_pine_moon", alias = "RosePine")]
    RosePineMoon,
    Nord,
    Catppuccin,
    OneDark,
}

impl ThemeSelection {
    pub const ALL: [Self; 6] = [
        Self::NominalLight,
        Self::Default,
        Self::RosePineMoon,
        Self::Nord,
        Self::Catppuccin,
        Self::OneDark,
    ];

    pub fn get_name(&self) -> &'static str {
        match self {
            Self::Default => "Dark",
            Self::NominalLight => "Light",
            Self::RosePineMoon => "Rosé Pine Moon",
            Self::Nord => "Nord",
            Self::Catppuccin => "Catppuccin",
            Self::OneDark => "One Dark",
        }
    }

    #[cfg(test)]
    pub fn is_light(&self) -> bool {
        matches!(self, Self::NominalLight)
    }

    pub fn get_style(&self) -> eframe::egui::Style {
        self.get_colors().to_egui_style()
    }

    /// Load the colors for this selection for widgets that draw custom content.
    pub fn get_colors(&self) -> ThemeColors {
        match self {
            Self::Default => load_builtin_theme(NOMINAL_DARK_THEME_PATH, NOMINAL_DARK_THEME_SOURCE),
            Self::NominalLight => {
                load_builtin_theme(NOMINAL_LIGHT_THEME_PATH, NOMINAL_LIGHT_THEME_SOURCE)
            }
            Self::RosePineMoon => {
                load_builtin_theme(ROSE_PINE_MOON_THEME_PATH, ROSE_PINE_MOON_THEME_SOURCE)
            }
            Self::Nord => load_builtin_theme(NORD_THEME_PATH, NORD_THEME_SOURCE),
            Self::Catppuccin => load_builtin_theme(CATPPUCCIN_THEME_PATH, CATPPUCCIN_THEME_SOURCE),
            Self::OneDark => load_builtin_theme(ONEDARK_THEME_PATH, ONEDARK_THEME_SOURCE),
        }
    }

    #[cfg(test)]
    pub fn next(&self) -> Self {
        match self {
            Self::Default => Self::NominalLight,
            Self::NominalLight => Self::RosePineMoon,
            Self::RosePineMoon => Self::Nord,
            Self::Nord => Self::Catppuccin,
            Self::Catppuccin => Self::OneDark,
            Self::OneDark => Self::Default,
        }
    }
}

fn load_builtin_theme(path: &str, embedded_source: &str) -> ThemeColors {
    ThemeColors::from_toml(embedded_source).unwrap_or_else(|| {
        log::error!("Embedded theme {path} is invalid; using default colors");
        ThemeColors::default()
    })
}

/// Store the current colors in egui's context for custom widgets to use.
pub fn store_theme(ctx: &eframe::egui::Context, colors: ThemeColors) {
    ctx.data_mut(|data| data.insert_persisted(eframe::egui::Id::new("app_theme"), colors));
}

/// Read the colors currently used by custom widgets.
pub fn get_theme(ctx: &eframe::egui::Context) -> ThemeColors {
    ctx.data_mut(|data| {
        data.get_persisted::<ThemeColors>(eframe::egui::Id::new("app_theme"))
            .unwrap_or_default()
    })
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ThemeColors {
    pub background: String,
    pub panel_bg: String,
    pub text: String,
    pub accent: String,
    pub button: String,
    pub button_hover: String,
    pub button_text: String,
    #[serde(default)]
    pub border: Option<String>,
    #[serde(default)]
    pub muted_text: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub warning: Option<String>,
    #[serde(default)]
    pub success: Option<String>,
    #[serde(default)]
    pub info: Option<String>,
    #[serde(default)]
    pub is_light: bool,
}

impl Default for ThemeColors {
    fn default() -> Self {
        ThemeColors::from_toml(NOMINAL_LIGHT_THEME_SOURCE).unwrap_or(Self {
            background: "#f8fafc".to_string(),
            panel_bg: "#ffffff".to_string(),
            text: "#0f172a".to_string(),
            accent: "#0284c7".to_string(),
            button: "#f1f5f9".to_string(),
            button_hover: "#e2e8f0".to_string(),
            button_text: "#1e293b".to_string(),
            border: Some("#cbd5e1".to_string()),
            muted_text: Some("#64748b".to_string()),
            error: Some("#dc2626".to_string()),
            warning: Some("#d97706".to_string()),
            success: Some("#16a34a".to_string()),
            info: Some("#0284c7".to_string()),
            is_light: true,
        })
    }
}

impl ThemeColors {
    pub fn parse_hex(hex: &str) -> eframe::egui::Color32 {
        let hex = hex.trim().trim_start_matches('#');
        let parse = |part: &str| u8::from_str_radix(part, 16).ok();

        let parsed = (|| -> Option<(u8, u8, u8, u8)> {
            if !hex.is_ascii() {
                return None;
            }

            match hex.len() {
                6 => Some((
                    parse(&hex[0..2])?,
                    parse(&hex[2..4])?,
                    parse(&hex[4..6])?,
                    255,
                )),
                8 => Some((
                    parse(&hex[0..2])?,
                    parse(&hex[2..4])?,
                    parse(&hex[4..6])?,
                    parse(&hex[6..8])?,
                )),
                _ => None,
            }
        })();

        let Some((r, g, b, a)) = parsed else {
            return eframe::egui::Color32::from_rgb(255, 0, 255);
        };

        eframe::egui::Color32::from_rgba_unmultiplied(r, g, b, a)
    }

    pub fn canvas_color(&self) -> eframe::egui::Color32 {
        Self::parse_hex(&self.background)
    }

    pub fn panel_color(&self) -> eframe::egui::Color32 {
        Self::parse_hex(&self.panel_bg)
    }

    pub fn text_color(&self) -> eframe::egui::Color32 {
        Self::parse_hex(&self.text)
    }

    pub fn accent_color(&self) -> eframe::egui::Color32 {
        Self::parse_hex(&self.accent)
    }

    pub fn border_color(&self) -> eframe::egui::Color32 {
        self.border
            .as_deref()
            .map(Self::parse_hex)
            .unwrap_or_else(|| {
                if self.is_light {
                    eframe::egui::Color32::from_rgb(226, 232, 240)
                } else {
                    eframe::egui::Color32::from_rgb(38, 45, 60)
                }
            })
    }

    pub fn muted_text_color(&self) -> eframe::egui::Color32 {
        self.muted_text
            .as_deref()
            .map(Self::parse_hex)
            .unwrap_or_else(|| {
                if self.is_light {
                    eframe::egui::Color32::from_rgb(100, 116, 139)
                } else {
                    eframe::egui::Color32::from_rgb(125, 133, 144)
                }
            })
    }

    pub fn error_color(&self) -> eframe::egui::Color32 {
        self.error
            .as_deref()
            .map(Self::parse_hex)
            .unwrap_or(eframe::egui::Color32::from_rgb(224, 108, 117))
    }

    pub fn warning_color(&self) -> eframe::egui::Color32 {
        self.warning
            .as_deref()
            .map(Self::parse_hex)
            .unwrap_or(eframe::egui::Color32::from_rgb(209, 154, 102))
    }

    /// High-contrast vertical playhead line and marker color for both dark and light themes.
    pub fn playhead_color(&self) -> eframe::egui::Color32 {
        if self.is_light {
            eframe::egui::Color32::from_rgb(217, 119, 6) // Deep amber-orange (#d97706)
        } else {
            eframe::egui::Color32::from_rgb(250, 204, 21) // Crisp gold-yellow (#facc15)
        }
    }

    /// High-contrast linked hover crosshair color for both dark and light themes.
    pub fn hover_cursor_color(&self) -> eframe::egui::Color32 {
        if self.is_light {
            eframe::egui::Color32::from_rgba_unmultiplied(37, 99, 235, 195) // Royal blue (#2563eb)
        } else {
            eframe::egui::Color32::from_rgba_unmultiplied(125, 211, 252, 210) // Sky cyan (#7dd3fc)
        }
    }

    /// Theme-aware semantic color for a [`daqcore::storage::FaultSeverity`] level.
    pub fn fault_severity_color(
        &self,
        severity: daqcore::storage::FaultSeverity,
    ) -> eframe::egui::Color32 {
        match (self.is_light, severity) {
            (true, daqcore::storage::FaultSeverity::Fatal) => {
                eframe::egui::Color32::from_rgb(190, 18, 60) // #be123c
            }
            (false, daqcore::storage::FaultSeverity::Fatal) => {
                eframe::egui::Color32::from_rgb(244, 63, 94) // #f43f5e
            }
            (true, daqcore::storage::FaultSeverity::Error) => {
                eframe::egui::Color32::from_rgb(234, 88, 12) // #ea580c
            }
            (false, daqcore::storage::FaultSeverity::Error) => {
                eframe::egui::Color32::from_rgb(249, 115, 22) // #f97316
            }
            (true, daqcore::storage::FaultSeverity::Warning) => {
                eframe::egui::Color32::from_rgb(217, 119, 6) // #d97706
            }
            (false, daqcore::storage::FaultSeverity::Warning) => {
                eframe::egui::Color32::from_rgb(245, 158, 11) // #f59e0b
            }
        }
    }

    /// Theme-aware default trace palette color.
    pub fn trace_color(&self, index: usize) -> eframe::egui::Color32 {
        const DARK_PALETTE: [eframe::egui::Color32; 8] = [
            eframe::egui::Color32::from_rgb(80, 190, 255),
            eframe::egui::Color32::from_rgb(255, 160, 60),
            eframe::egui::Color32::from_rgb(100, 230, 140),
            eframe::egui::Color32::from_rgb(235, 110, 245),
            eframe::egui::Color32::from_rgb(255, 95, 105),
            eframe::egui::Color32::from_rgb(70, 225, 220),
            eframe::egui::Color32::from_rgb(250, 220, 80),
            eframe::egui::Color32::from_rgb(165, 140, 255),
        ];
        const LIGHT_PALETTE: [eframe::egui::Color32; 8] = [
            eframe::egui::Color32::from_rgb(2, 132, 199),
            eframe::egui::Color32::from_rgb(234, 88, 12),
            eframe::egui::Color32::from_rgb(22, 163, 74),
            eframe::egui::Color32::from_rgb(147, 51, 234),
            eframe::egui::Color32::from_rgb(225, 29, 72),
            eframe::egui::Color32::from_rgb(13, 148, 136),
            eframe::egui::Color32::from_rgb(180, 83, 9),
            eframe::egui::Color32::from_rgb(79, 70, 229),
        ];
        if self.is_light {
            LIGHT_PALETTE[index % LIGHT_PALETTE.len()]
        } else {
            DARK_PALETTE[index % DARK_PALETTE.len()]
        }
    }

    #[cfg(test)]
    pub fn success_color(&self) -> eframe::egui::Color32 {
        self.success
            .as_deref()
            .map(Self::parse_hex)
            .unwrap_or(eframe::egui::Color32::from_rgb(63, 185, 80))
    }

    pub fn to_egui_style(&self) -> eframe::egui::Style {
        let mut style = eframe::egui::Style {
            visuals: if self.is_light {
                eframe::egui::Visuals::light()
            } else {
                eframe::egui::Visuals::dark()
            },
            ..Default::default()
        };

        // Crisp, compact spacing inspired by Nominal.io
        style.spacing.item_spacing = eframe::egui::vec2(5.0, 3.0);
        style.spacing.button_padding = eframe::egui::vec2(5.0, 2.0);
        style.spacing.window_margin = eframe::egui::Margin::same(6);
        style.spacing.indent = 12.0;

        let background = self.canvas_color();
        let panel = self.panel_color();
        let text = self.text_color();
        let accent = self.accent_color();
        let button = Self::parse_hex(&self.button);
        let button_hover = Self::parse_hex(&self.button_hover);
        let button_text = Self::parse_hex(&self.button_text);
        let border = self.border_color();

        style.visuals.window_fill = panel;
        style.visuals.panel_fill = panel;
        style.visuals.faint_bg_color = if self.is_light {
            eframe::egui::Color32::from_rgb(241, 245, 249)
        } else {
            panel.gamma_multiply(1.12)
        };
        style.visuals.extreme_bg_color = background;
        style.visuals.code_bg_color = background;
        style.visuals.override_text_color = Some(text);
        style.visuals.hyperlink_color = accent;
        style.visuals.warn_fg_color = self.warning_color();
        style.visuals.error_fg_color = self.error_color();
        style.visuals.selection.bg_fill =
            accent.gamma_multiply(if self.is_light { 0.18_f32 } else { 0.28_f32 });
        style.visuals.selection.stroke = eframe::egui::Stroke::new(1.0_f32, accent);
        style.visuals.text_edit_bg_color = Some(background);
        style.visuals.window_stroke = eframe::egui::Stroke::new(1.0_f32, border);
        style.visuals.window_corner_radius = eframe::egui::CornerRadius::same(4);

        let cr = eframe::egui::CornerRadius::same(3);
        style.visuals.widgets.noninteractive.bg_fill = panel;
        style.visuals.widgets.noninteractive.weak_bg_fill = panel;
        style.visuals.widgets.noninteractive.fg_stroke = eframe::egui::Stroke::new(1.0_f32, text);
        style.visuals.widgets.noninteractive.bg_stroke = eframe::egui::Stroke::new(1.0_f32, border);
        style.visuals.widgets.noninteractive.corner_radius = cr;

        style.visuals.widgets.inactive.bg_fill = button;
        style.visuals.widgets.inactive.weak_bg_fill = eframe::egui::Color32::TRANSPARENT;
        style.visuals.widgets.inactive.fg_stroke = eframe::egui::Stroke::new(1.0_f32, button_text);
        style.visuals.widgets.inactive.bg_stroke = eframe::egui::Stroke::new(1.0_f32, border);
        style.visuals.widgets.inactive.corner_radius = cr;

        style.visuals.widgets.hovered.bg_fill = button_hover;
        style.visuals.widgets.hovered.weak_bg_fill = button_hover;
        style.visuals.widgets.hovered.fg_stroke = eframe::egui::Stroke::new(1.0_f32, text);
        style.visuals.widgets.hovered.bg_stroke =
            eframe::egui::Stroke::new(1.0_f32, accent.gamma_multiply(0.65_f32));
        style.visuals.widgets.hovered.corner_radius = cr;

        style.visuals.widgets.active.bg_fill = button_hover;
        style.visuals.widgets.active.weak_bg_fill = button_hover;
        style.visuals.widgets.active.fg_stroke = eframe::egui::Stroke::new(1.0_f32, text);
        style.visuals.widgets.active.bg_stroke = eframe::egui::Stroke::new(1.0_f32, accent);
        style.visuals.widgets.active.corner_radius = cr;

        style.visuals.widgets.open.bg_fill = button_hover;
        style.visuals.widgets.open.weak_bg_fill = button_hover;
        style.visuals.widgets.open.fg_stroke = eframe::egui::Stroke::new(1.0_f32, text);
        style.visuals.widgets.open.bg_stroke = eframe::egui::Stroke::new(1.0_f32, border);
        style.visuals.widgets.open.corner_radius = cr;

        style
    }

    fn from_toml(data: &str) -> Option<Self> {
        toml::from_str::<Self>(data).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_themes_load_and_convert_to_style_cleanly() {
        for theme in ThemeSelection::ALL {
            let colors = theme.get_colors();
            let style = theme.get_style();
            assert_eq!(style.visuals.dark_mode, !colors.is_light);
            assert!(!theme.get_name().is_empty());
            let _ = (colors.muted_text_color(), colors.success_color());
        }
        assert_eq!(ThemeSelection::default(), ThemeSelection::NominalLight);
        assert!(!ThemeSelection::Default.is_light());
        assert!(ThemeSelection::NominalLight.is_light());
        assert!(!ThemeSelection::RosePineMoon.is_light());
        assert_eq!(ThemeSelection::Default.get_name(), "Dark");
        assert_eq!(ThemeSelection::NominalLight.get_name(), "Light");
        assert_eq!(ThemeSelection::RosePineMoon.get_name(), "Rosé Pine Moon");
        assert_eq!(ThemeSelection::Default.next(), ThemeSelection::NominalLight);
        assert_eq!(
            ThemeSelection::NominalLight.next(),
            ThemeSelection::RosePineMoon
        );
    }
}
