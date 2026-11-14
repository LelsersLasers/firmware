use crate::widget_constructor;
use std::path;

pub enum AppAction {
    SpawnWidget(widget_constructor::WidgetConstructor),
    AttachStore(path::PathBuf),
    OpenAnnotationModal,
    ToggleSidebar,
    ToggleCommandPalette,
    CloseActiveWidget,
    IncreaseScale,
    DecreaseScale,
}

impl AppAction {
    pub fn cmd_palette_list() -> Vec<(&'static str, widget_constructor::WidgetConstructor)> {
        vec![
            (
                "Spawn Scope",
                widget_constructor::WidgetConstructor::ScopeEmpty,
            ),
            (
                "Spawn State Lanes",
                widget_constructor::WidgetConstructor::StateLane,
            ),
            (
                "Spawn XY Scatter",
                widget_constructor::WidgetConstructor::XYScatter,
            ),
            (
                "Spawn GPS Plot",
                widget_constructor::WidgetConstructor::GpsPlot,
            ),
            (
                "Preset: G-G Diagram",
                widget_constructor::WidgetConstructor::PresetGgDiagram,
            ),
            (
                "Preset: Handling Understeer",
                widget_constructor::WidgetConstructor::PresetHandlingUndersteer,
            ),
            (
                "Preset: Battery Pack Overview",
                widget_constructor::WidgetConstructor::PresetBatteryOverview,
            ),
            (
                "Spawn CAN Table",
                widget_constructor::WidgetConstructor::ViewerTable,
            ),
            (
                "Spawn CAN List",
                widget_constructor::WidgetConstructor::ViewerList,
            ),
            (
                "Spawn Bootloader",
                widget_constructor::WidgetConstructor::Bootloader,
            ),
            (
                "Spawn Log Parser",
                widget_constructor::WidgetConstructor::LogParser,
            ),
            (
                "Spawn Send UI",
                widget_constructor::WidgetConstructor::SendUi,
            ),
            (
                "Spawn Bus Load",
                widget_constructor::WidgetConstructor::BusLoad,
            ),
            (
                "Spawn Jitter",
                widget_constructor::WidgetConstructor::Jitter,
            ),
            ("Spawn HIL", widget_constructor::WidgetConstructor::Hil),
        ]
    }
}
