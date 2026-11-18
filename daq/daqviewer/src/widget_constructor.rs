use crate::{app::ViewerPlatform, ui, widget_ids, widgets};

#[derive(Eq, PartialEq, Hash, Clone, Copy, Debug)]
pub enum WidgetKind {
    ViewerTable,
    ViewerList,
    Bootloader,
    Scope,
    StateLane,
    XYScatter,
    LogParser,
    SendUi,
    BusLoad,
    GpsPlot,
    Jitter,
    CloudStats,
    Hil,
}

impl WidgetKind {
    pub fn max_tabs_allowed(&self) -> usize {
        match self {
            WidgetKind::Hil | WidgetKind::CloudStats => 1,
            _ => usize::MAX,
        }
    }
}

#[derive(Eq, PartialEq, Clone, Debug)]
pub enum WidgetConstructor {
    ViewerTable,
    ViewerList,
    Bootloader,
    Scope {
        key: daqcore::superdbc::message::MessageKey,
        msg_name: String,
        signal_name: String,
    },
    ScopeChannel {
        signal_id: String,
        label: String,
        unit: String,
    },
    ScopeEmpty,
    StateLane,
    StateLaneSingle {
        signal_id: String,
        label: String,
    },
    XYScatter,
    PresetGgDiagram,
    PresetHandlingUndersteer,
    PresetBatteryOverview,
    LogParser,
    SendUi,
    BusLoad,
    GpsPlot,
    Jitter,
    CloudStats,
    Hil,
}

impl WidgetConstructor {
    pub fn kind(&self) -> WidgetKind {
        match self {
            WidgetConstructor::ViewerTable => WidgetKind::ViewerTable,
            WidgetConstructor::ViewerList => WidgetKind::ViewerList,
            WidgetConstructor::Bootloader => WidgetKind::Bootloader,
            WidgetConstructor::Scope { .. }
            | WidgetConstructor::ScopeChannel { .. }
            | WidgetConstructor::ScopeEmpty
            | WidgetConstructor::PresetBatteryOverview => WidgetKind::Scope,
            WidgetConstructor::StateLane | WidgetConstructor::StateLaneSingle { .. } => {
                WidgetKind::StateLane
            }
            WidgetConstructor::XYScatter
            | WidgetConstructor::PresetGgDiagram
            | WidgetConstructor::PresetHandlingUndersteer => WidgetKind::XYScatter,
            WidgetConstructor::LogParser => WidgetKind::LogParser,
            WidgetConstructor::SendUi => WidgetKind::SendUi,
            WidgetConstructor::BusLoad => WidgetKind::BusLoad,
            WidgetConstructor::GpsPlot => WidgetKind::GpsPlot,
            WidgetConstructor::Jitter => WidgetKind::Jitter,
            WidgetConstructor::CloudStats => WidgetKind::CloudStats,
            WidgetConstructor::Hil => WidgetKind::Hil,
        }
    }

    pub fn create(
        self,
        widget_ids: &mut widget_ids::WidgetIds,
        platform: &mut dyn ViewerPlatform,
        existing_count: usize,
    ) -> Option<widgets::Widget> {
        self.create_all(widget_ids, platform, existing_count)
            .into_iter()
            .next()
    }

    pub fn create_all(
        self,
        widget_ids: &mut widget_ids::WidgetIds,
        platform: &mut dyn ViewerPlatform,
        existing_count: usize,
    ) -> Vec<widgets::Widget> {
        let kind = self.kind();
        if existing_count >= kind.max_tabs_allowed() {
            return Vec::new();
        }

        let id = widget_ids.next(kind);
        match self {
            WidgetConstructor::ViewerTable => {
                vec![widgets::Widget::ViewerTable(
                    ui::viewer_table::ViewerTable::new(id),
                )]
            }
            WidgetConstructor::ViewerList => {
                vec![widgets::Widget::ViewerList(
                    ui::viewer_list::ViewerList::new(id),
                )]
            }
            WidgetConstructor::Scope {
                key,
                msg_name,
                signal_name,
            } => vec![widgets::Widget::Scope(Box::new(ui::scope::Scope::new(
                id,
                key,
                msg_name,
                signal_name,
            )))],
            WidgetConstructor::ScopeChannel {
                signal_id,
                label,
                unit,
            } => vec![widgets::Widget::Scope(Box::new(
                ui::scope::Scope::new_channel(id, signal_id, label, unit),
            ))],
            WidgetConstructor::ScopeEmpty => {
                vec![widgets::Widget::Scope(Box::new(
                    ui::scope::Scope::new_empty(id),
                ))]
            }
            WidgetConstructor::StateLane => {
                vec![widgets::Widget::StateLane(ui::state_lane::StateLane::new(
                    id,
                ))]
            }
            WidgetConstructor::StateLaneSingle { signal_id, label } => {
                vec![widgets::Widget::StateLane(
                    ui::state_lane::StateLane::new_single(id, signal_id, label),
                )]
            }
            WidgetConstructor::XYScatter => {
                vec![widgets::Widget::XYScatter(ui::xy_scatter::XYScatter::new(
                    id,
                ))]
            }
            WidgetConstructor::PresetGgDiagram => {
                vec![widgets::Widget::XYScatter(
                    ui::xy_scatter::XYScatter::preset_gg_diagram(id),
                )]
            }
            WidgetConstructor::PresetHandlingUndersteer => {
                let scatter =
                    widgets::Widget::XYScatter(ui::xy_scatter::XYScatter::preset_understeer(id));
                let scope_id = widget_ids.next(WidgetKind::Scope);
                let scope = widgets::Widget::Scope(Box::new(ui::scope::Scope::new_multi(
                    scope_id,
                    format!("YT Plot: Handling & Understeer #{scope_id}"),
                    vec![
                        (
                            "LWS_Standard.LWS_ANGLE".to_owned(),
                            "Steering Angle".to_owned(),
                            "deg".to_owned(),
                        ),
                        (
                            "IMU_angular_rate.Z_axis".to_owned(),
                            "Yaw Rate".to_owned(),
                            "deg/s".to_owned(),
                        ),
                        (
                            "vd.understeer_gradient".to_owned(),
                            "Understeer Gradient".to_owned(),
                            "deg/g".to_owned(),
                        ),
                    ],
                )));
                vec![scatter, scope]
            }
            WidgetConstructor::PresetBatteryOverview => {
                let scope = widgets::Widget::Scope(Box::new(ui::scope::Scope::new_multi(
                    id,
                    format!("YT Plot: Battery Pack Overview #{id}"),
                    vec![
                        (
                            "pack_stats.pack_voltage".to_owned(),
                            "Pack Voltage".to_owned(),
                            "V".to_owned(),
                        ),
                        (
                            "battery.voltage_v".to_owned(),
                            "Pack Voltage (ADC)".to_owned(),
                            "V".to_owned(),
                        ),
                        (
                            "pack_stats.pack_current".to_owned(),
                            "Pack Current".to_owned(),
                            "A".to_owned(),
                        ),
                        (
                            "battery.power_kw".to_owned(),
                            "Pack Power".to_owned(),
                            "kW".to_owned(),
                        ),
                        (
                            "pack_stats.max_temp".to_owned(),
                            "Max Cell Temp".to_owned(),
                            "C".to_owned(),
                        ),
                    ],
                )));
                let scatter_id = widget_ids.next(WidgetKind::XYScatter);
                let scatter = widgets::Widget::XYScatter(
                    ui::xy_scatter::XYScatter::preset_battery_scatter(scatter_id),
                );
                let lane_id = widget_ids.next(WidgetKind::StateLane);
                let lanes = widgets::Widget::StateLane(ui::state_lane::StateLane::new_with_lanes(
                    lane_id,
                    format!("State Lanes: BMS & SDC Faults #{lane_id}"),
                    ui::state_lane::BATTERY_FAULT_STATE_LANES
                        .iter()
                        .map(|&(sig, lbl)| (sig.to_owned(), lbl.to_owned()))
                        .collect(),
                ));
                vec![scope, scatter, lanes]
            }
            WidgetConstructor::BusLoad => {
                vec![widgets::Widget::BusLoad(ui::bus_load::BusLoad::new(id))]
            }
            WidgetConstructor::GpsPlot => {
                vec![widgets::Widget::GpsPlot(ui::gps_plot::GpsPlot::new(id))]
            }
            WidgetConstructor::Jitter => {
                vec![widgets::Widget::Jitter(ui::jitter::Jitter::new(id))]
            }
            WidgetConstructor::CloudStats => {
                vec![widgets::Widget::CloudStats(ui::cloud_stats::CloudStats::new(id))]
            }
            ctor @ (WidgetConstructor::Bootloader
            | WidgetConstructor::LogParser
            | WidgetConstructor::SendUi
            | WidgetConstructor::Hil) => platform
                .create_platform_widget(&ctor, id)
                .into_iter()
                .map(widgets::Widget::Platform)
                .collect(),
        }
    }
}
