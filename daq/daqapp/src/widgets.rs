use crate::{action, app, ui, widget_constructor, workspace_view};
use std::sync::mpsc;

pub enum Widget {
    ViewerTable(ui::viewer_table::ViewerTable),
    ViewerList(ui::viewer_list::ViewerList),
    Bootloader(ui::bootloader::Bootloader),
    Scope(ui::scope::Scope),
    StateLane(ui::state_lane::StateLane),
    XYScatter(ui::xy_scatter::XYScatter),
    LogParser(ui::log_parser::LogParser),
    SendUi(ui::send::SendUi),
    BusLoad(ui::bus_load::BusLoad),
    GpsPlot(ui::gps_plot::GpsPlot),
    Jitter(ui::jitter::Jitter),
    Hil(ui::hil::Hil),
}

pub struct WidgetContext<'a> {
    pub view: &'a workspace_view::WorkspaceDataView<'a>,
    pub bus_load: &'a [workspace_view::BusLoadSample],
    pub action_queue: &'a mut Vec<action::AppAction>,
    pub database: Option<&'a app::DatabaseSelection>,
    pub ui_to_can_tx: mpsc::Sender<daqcore::can_thread::CanThreadCommand>,
    pub formatter: &'a Option<daqcore::formatter::Formatter>,
}

impl Widget {
    pub fn title(&self) -> &str {
        match self {
            Widget::ViewerTable(w) => &w.title,
            Widget::ViewerList(w) => &w.title,
            Widget::Bootloader(w) => &w.title,
            Widget::Scope(w) => &w.title,
            Widget::StateLane(w) => &w.title,
            Widget::XYScatter(w) => &w.title,
            Widget::LogParser(w) => &w.title,
            Widget::SendUi(w) => &w.title,
            Widget::BusLoad(w) => &w.title,
            Widget::GpsPlot(w) => &w.title,
            Widget::Jitter(w) => &w.title,
            Widget::Hil(w) => &w.title,
        }
    }

    pub fn kind(&self) -> widget_constructor::WidgetKind {
        match self {
            Widget::ViewerTable(_) => widget_constructor::WidgetKind::ViewerTable,
            Widget::ViewerList(_) => widget_constructor::WidgetKind::ViewerList,
            Widget::Bootloader(_) => widget_constructor::WidgetKind::Bootloader,
            Widget::Scope(_) => widget_constructor::WidgetKind::Scope,
            Widget::StateLane(_) => widget_constructor::WidgetKind::StateLane,
            Widget::XYScatter(_) => widget_constructor::WidgetKind::XYScatter,
            Widget::LogParser(_) => widget_constructor::WidgetKind::LogParser,
            Widget::SendUi(_) => widget_constructor::WidgetKind::SendUi,
            Widget::BusLoad(_) => widget_constructor::WidgetKind::BusLoad,
            Widget::GpsPlot(_) => widget_constructor::WidgetKind::GpsPlot,
            Widget::Jitter(_) => widget_constructor::WidgetKind::Jitter,
            Widget::Hil(_) => widget_constructor::WidgetKind::Hil,
        }
    }

    pub fn show(
        &mut self,
        ui: &mut eframe::egui::Ui,
        context: WidgetContext<'_>,
    ) -> egui_tiles::UiResponse {
        match self {
            Widget::ViewerTable(w) => w.show(
                ui,
                context.action_queue,
                context.formatter,
                context.database,
                context.view,
            ),
            Widget::ViewerList(w) => w.show(ui, context.formatter, context.database, context.view),
            Widget::Bootloader(w) => w.show(ui, &context.ui_to_can_tx),
            Widget::Scope(w) => w.show(ui, context.database, context.view),
            Widget::StateLane(w) => w.show(ui, context.database, context.view),
            Widget::XYScatter(w) => w.show(ui, context.database, context.view),
            Widget::LogParser(w) => {
                w.show(ui, context.database, context.view, context.action_queue)
            }
            Widget::SendUi(w) => w.show(ui, context.database, context.formatter),
            Widget::BusLoad(w) => w.show(ui, context.bus_load, context.view),
            Widget::GpsPlot(w) => w.show(ui, context.view),
            Widget::Jitter(w) => w.show(ui, context.database, context.view),
            Widget::Hil(w) => w.show(ui),
        }
    }

    pub fn reset_database(&mut self) {
        match self {
            Widget::SendUi(widget) => widget.reset_database(),
            Widget::Bootloader(widget) => widget.reset_database(),
            Widget::Scope(widget) => widget.reset_database(),
            Widget::StateLane(widget) => widget.reset_database(),
            Widget::XYScatter(widget) => widget.reset_database(),
            Widget::Jitter(widget) => widget.reset_database(),
            _ => {}
        }
    }

    pub fn handle_operational_event(&mut self, event: &daqcore::can_thread::CanThreadEvent) {
        match self {
            Widget::Bootloader(w) => w.handle_can_message(event),
            Widget::SendUi(w) => w.handle_can_message(event),
            Widget::Hil(w) => w.handle_can_message(event),
            _ => {}
        }
    }
}
