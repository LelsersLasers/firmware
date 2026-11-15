use crate::widget_constructor;
use std::path;

#[derive(Clone)]
pub enum AppAction {
    SpawnWidget(widget_constructor::WidgetConstructor),
    AttachStore(path::PathBuf),
    OpenAnnotationModal,
    LoadWorkspacePreset {
        preset_id: String,
        apply_time_context: bool,
    },
    OpenWorkspaceViewModal,
    CopyWorkspaceShareString,
    OpenExportDataModal,
    ToggleSidebar,
    FocusSidebarSearch,
    ToggleCommandPalette,
    CloseActiveWidget,
    IncreaseScale,
    DecreaseScale,
    TogglePlayback,
    StepPlayhead { direction: i64, fine: bool },
    PanWindow { direction: i64 },
    ZoomWindow { factor: f64 },
    JumpStint { direction: i64 },
    FitDay,
    ToggleFoldGaps,
}

impl AppAction {
    pub fn cmd_palette_actions() -> Vec<(&'static str, Self)> {
        vec![
            (
                "Workspace: VD — Handling, Suspension & G-G",
                Self::LoadWorkspacePreset {
                    preset_id: "vd_handling_suspension".to_owned(),
                    apply_time_context: true,
                },
            ),
            (
                "Workspace: Powertrain — Accumulator, Torque & Motor Speeds",
                Self::LoadWorkspacePreset {
                    preset_id: "powertrain_accumulator".to_owned(),
                    apply_time_context: true,
                },
            ),
            (
                "Workspace: Thermals — Inverters, Motors, IGBTs & Pack",
                Self::LoadWorkspacePreset {
                    preset_id: "thermals_cooling".to_owned(),
                    apply_time_context: true,
                },
            ),
            (
                "Workspace: Aerodynamics — Heave, Roll, Pitch & Ride Height",
                Self::LoadWorkspacePreset {
                    preset_id: "aero_ride_height".to_owned(),
                    apply_time_context: true,
                },
            ),
            (
                "Workspace: Sensors & Controls — IMU, GPS, Pedals & Steering",
                Self::LoadWorkspacePreset {
                    preset_id: "sensors_controls".to_owned(),
                    apply_time_context: true,
                },
            ),
            (
                "Workspace: Trackside Live Debug — States, Faults & CAN Bus",
                Self::LoadWorkspacePreset {
                    preset_id: "trackside_live_debug".to_owned(),
                    apply_time_context: true,
                },
            ),
            (
                "Export Dataset (Parquet / CSV / MATLAB .mat)...",
                Self::OpenExportDataModal,
            ),
            (
                "Save / Import Workspace View (.daqview.json / Share String)...",
                Self::OpenWorkspaceViewModal,
            ),
            (
                "Copy Workspace Share String (daqview1:...)",
                Self::CopyWorkspaceShareString,
            ),
            (
                "Spawn Scope",
                Self::SpawnWidget(widget_constructor::WidgetConstructor::ScopeEmpty),
            ),
            (
                "Spawn State Lanes",
                Self::SpawnWidget(widget_constructor::WidgetConstructor::StateLane),
            ),
            (
                "Spawn XY Scatter",
                Self::SpawnWidget(widget_constructor::WidgetConstructor::XYScatter),
            ),
            (
                "Spawn GPS Plot",
                Self::SpawnWidget(widget_constructor::WidgetConstructor::GpsPlot),
            ),
            (
                "Preset: G-G Diagram",
                Self::SpawnWidget(widget_constructor::WidgetConstructor::PresetGgDiagram),
            ),
            (
                "Preset: Handling Understeer",
                Self::SpawnWidget(widget_constructor::WidgetConstructor::PresetHandlingUndersteer),
            ),
            (
                "Preset: Battery Pack Overview",
                Self::SpawnWidget(widget_constructor::WidgetConstructor::PresetBatteryOverview),
            ),
            (
                "Spawn CAN Table",
                Self::SpawnWidget(widget_constructor::WidgetConstructor::ViewerTable),
            ),
            (
                "Spawn CAN List",
                Self::SpawnWidget(widget_constructor::WidgetConstructor::ViewerList),
            ),
            (
                "Spawn Bootloader",
                Self::SpawnWidget(widget_constructor::WidgetConstructor::Bootloader),
            ),
            (
                "Spawn Log Parser",
                Self::SpawnWidget(widget_constructor::WidgetConstructor::LogParser),
            ),
            (
                "Spawn Send UI",
                Self::SpawnWidget(widget_constructor::WidgetConstructor::SendUi),
            ),
            (
                "Spawn Bus Load",
                Self::SpawnWidget(widget_constructor::WidgetConstructor::BusLoad),
            ),
            (
                "Spawn Jitter",
                Self::SpawnWidget(widget_constructor::WidgetConstructor::Jitter),
            ),
            (
                "Spawn HIL",
                Self::SpawnWidget(widget_constructor::WidgetConstructor::Hil),
            ),
        ]
    }
}
