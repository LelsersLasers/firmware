use crate::{action, app, ui, widget_constructor, workspace_io, workspace_view};
use std::any::Any;

/// Platform-provided widget (such as Desktop `Bootloader`, `LogParser`, `SendUi`, or `Hil`).
pub trait PlatformWidget: Send {
    fn kind(&self) -> widget_constructor::WidgetKind;
    fn title(&self) -> &str;
    fn set_title(&mut self, new_title: String);
    fn show(
        &mut self,
        ui: &mut eframe::egui::Ui,
        context: WidgetContext<'_>,
    ) -> egui_tiles::UiResponse;
    fn reset_database(&mut self) {}
    fn capture_spec(&self) -> workspace_io::PaneSpec;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

pub enum Widget {
    ViewerTable(ui::viewer_table::ViewerTable),
    ViewerList(ui::viewer_list::ViewerList),
    Scope(Box<ui::scope::Scope>),
    StateLane(ui::state_lane::StateLane),
    XYScatter(ui::xy_scatter::XYScatter),
    BusLoad(ui::bus_load::BusLoad),
    GpsPlot(ui::gps_plot::GpsPlot),
    Jitter(ui::jitter::Jitter),
    CloudStats(ui::cloud_stats::CloudStats),
    Platform(Box<dyn PlatformWidget>),
}

pub struct WidgetContext<'a> {
    pub view: &'a workspace_view::WorkspaceDataView<'a>,
    pub bus_load: &'a [workspace_view::BusLoadSample],
    pub action_queue: &'a mut Vec<action::AppAction>,
    pub database: Option<&'a app::DatabaseSelection>,
    pub formatter: &'a Option<daqcore::formatter::Formatter>,
}

impl Widget {
    pub fn title(&self) -> &str {
        match self {
            Widget::ViewerTable(w) => &w.title,
            Widget::ViewerList(w) => &w.title,
            Widget::Scope(w) => &w.title,
            Widget::StateLane(w) => &w.title,
            Widget::XYScatter(w) => &w.title,
            Widget::BusLoad(w) => &w.title,
            Widget::GpsPlot(w) => &w.title,
            Widget::Jitter(w) => &w.title,
            Widget::CloudStats(w) => &w.title,
            Widget::Platform(w) => w.title(),
        }
    }

    pub fn set_title(&mut self, new_title: String) {
        match self {
            Widget::ViewerTable(w) => w.title = new_title,
            Widget::ViewerList(w) => w.title = new_title,
            Widget::Scope(w) => w.set_title(new_title),
            Widget::StateLane(w) => w.set_title(new_title),
            Widget::XYScatter(w) => w.set_title(new_title),
            Widget::BusLoad(w) => w.title = new_title,
            Widget::GpsPlot(w) => w.set_title(new_title),
            Widget::Jitter(w) => w.title = new_title,
            Widget::CloudStats(w) => w.title = new_title,
            Widget::Platform(w) => w.set_title(new_title),
        }
    }

    pub fn has_custom_title(&self) -> bool {
        match self {
            Widget::Scope(w) => w.is_custom_title(),
            Widget::StateLane(w) => w.is_custom_title(),
            Widget::XYScatter(w) => w.is_custom_title(),
            Widget::GpsPlot(w) => w.is_custom_title(),
            _ => false,
        }
    }

    pub fn reset_auto_title(&mut self) {
        match self {
            Widget::Scope(w) => w.reset_auto_title(),
            Widget::StateLane(w) => w.reset_auto_title(),
            Widget::XYScatter(w) => w.reset_auto_title(),
            Widget::GpsPlot(w) => w.reset_auto_title(),
            _ => {}
        }
    }

    pub fn kind(&self) -> widget_constructor::WidgetKind {
        match self {
            Widget::ViewerTable(_) => widget_constructor::WidgetKind::ViewerTable,
            Widget::ViewerList(_) => widget_constructor::WidgetKind::ViewerList,
            Widget::Scope(_) => widget_constructor::WidgetKind::Scope,
            Widget::StateLane(_) => widget_constructor::WidgetKind::StateLane,
            Widget::XYScatter(_) => widget_constructor::WidgetKind::XYScatter,
            Widget::BusLoad(_) => widget_constructor::WidgetKind::BusLoad,
            Widget::GpsPlot(_) => widget_constructor::WidgetKind::GpsPlot,
            Widget::Jitter(_) => widget_constructor::WidgetKind::Jitter,
            Widget::CloudStats(_) => widget_constructor::WidgetKind::CloudStats,
            Widget::Platform(w) => w.kind(),
        }
    }

    pub fn show(
        &mut self,
        ui: &mut eframe::egui::Ui,
        context: WidgetContext<'_>,
    ) -> egui_tiles::UiResponse {
        let is_timeseries_plot = matches!(
            self,
            Widget::Scope(_)
                | Widget::StateLane(_)
                | Widget::XYScatter(_)
                | Widget::GpsPlot(_)
                | Widget::BusLoad(_)
                | Widget::Jitter(_)
                | Widget::ViewerTable(_)
                | Widget::ViewerList(_)
        );
        if is_timeseries_plot && context.view.is_all_track_days_unselected() {
            show_all_track_days_placeholder(ui);
            return egui_tiles::UiResponse::None;
        }
        match self {
            Widget::ViewerTable(w) => {
                w.show(ui, context.action_queue, context.formatter, context.view)
            }
            Widget::ViewerList(w) => w.show(ui, context.formatter, context.view),
            Widget::Scope(w) => w.show(ui, context.view),
            Widget::StateLane(w) => w.show(ui, context.view),
            Widget::XYScatter(w) => w.show(ui, context.view),
            Widget::BusLoad(w) => w.show(ui, context.bus_load, context.view),
            Widget::GpsPlot(w) => w.show(ui, context.view),
            Widget::Jitter(w) => w.show(ui, context.view),
            Widget::CloudStats(w) => w.show(ui, context.view, context.action_queue),
            Widget::Platform(w) => w.show(ui, context),
        }
    }

    pub fn reset_database(&mut self) {
        if let Widget::Platform(w) = self {
            w.reset_database();
        }
    }
}

fn show_all_track_days_placeholder(ui: &mut eframe::egui::Ui) {
    let theme = ui::theme::get_theme(ui.ctx());
    let muted = theme.muted_text_color();
    let rect = ui.available_rect_before_wrap();
    ui.allocate_rect(rect, eframe::egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter_at(rect);
    let center = rect.center();
    painter.text(
        center - eframe::egui::vec2(0.0, 9.0),
        eframe::egui::Align2::CENTER_CENTER,
        "All Track Days Selected",
        eframe::egui::FontId::proportional(13.0),
        muted,
    );
    painter.text(
        center + eframe::egui::vec2(0.0, 9.0),
        eframe::egui::Align2::CENTER_CENTER,
        "Select a Track Day, Run, Fault, or Chunk from the sidebar (or Shift+Click runs to compare across days)",
        eframe::egui::FontId::proportional(11.0),
        muted.gamma_multiply(0.75),
    );
}
