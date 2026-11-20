use crate::{
    action, paths, settings, ui, util, widget_constructor, widget_ids, widgets, workspace_io,
    workspace_view,
};
use std::{any::Any, error, ops::RangeInclusive, path, sync::mpsc};

pub use daqviewer::{
    ConnectionStatus, DatabaseSelection, PlatformWidget, ViewerApp, ViewerPlatform, ViewerState,
};

pub enum DesktopWidget {
    Bootloader {
        widget: ui::bootloader::Bootloader,
        ui_to_can_tx: mpsc::Sender<daqnative::can_thread::CanThreadCommand>,
    },
    LogParser(ui::log_parser::LogParser),
    SendUi(ui::send::SendUi),
    Hil(ui::hil::Hil),
}

impl DesktopWidget {
    pub fn handle_can_message(&mut self, event: &daqnative::can_thread::CanThreadEvent) {
        match self {
            DesktopWidget::Bootloader { widget, .. } => widget.handle_can_message(event),
            DesktopWidget::SendUi(w) => w.handle_can_message(event),
            DesktopWidget::Hil(w) => w.handle_can_message(event),
            DesktopWidget::LogParser(_) => {}
        }
    }
}

impl Drop for DesktopWidget {
    fn drop(&mut self) {
        if let DesktopWidget::Bootloader {
            widget,
            ui_to_can_tx,
        } = self
            && widget.is_running()
        {
            let _ =
                ui_to_can_tx.send(daqnative::can_thread::CanThreadCommand::CancelFirmwareUpdate);
        }
    }
}

impl PlatformWidget for DesktopWidget {
    fn kind(&self) -> widget_constructor::WidgetKind {
        match self {
            DesktopWidget::Bootloader { .. } => widget_constructor::WidgetKind::Bootloader,
            DesktopWidget::LogParser(_) => widget_constructor::WidgetKind::LogParser,
            DesktopWidget::SendUi(_) => widget_constructor::WidgetKind::SendUi,
            DesktopWidget::Hil(_) => widget_constructor::WidgetKind::Hil,
        }
    }

    fn title(&self) -> &str {
        match self {
            DesktopWidget::Bootloader { widget, .. } => &widget.title,
            DesktopWidget::LogParser(w) => &w.title,
            DesktopWidget::SendUi(w) => &w.title,
            DesktopWidget::Hil(w) => &w.title,
        }
    }

    fn set_title(&mut self, new_title: String) {
        match self {
            DesktopWidget::Bootloader { widget, .. } => widget.title = new_title,
            DesktopWidget::LogParser(w) => w.title = new_title,
            DesktopWidget::SendUi(w) => w.title = new_title,
            DesktopWidget::Hil(w) => w.title = new_title,
        }
    }

    fn show(
        &mut self,
        ui: &mut eframe::egui::Ui,
        context: widgets::WidgetContext<'_>,
    ) -> egui_tiles::UiResponse {
        match self {
            DesktopWidget::Bootloader {
                widget,
                ui_to_can_tx,
            } => widget.show(ui, ui_to_can_tx),
            DesktopWidget::LogParser(w) => {
                w.show(ui, context.database, context.view, context.action_queue)
            }
            DesktopWidget::SendUi(w) => w.show(ui, context.database, context.formatter),
            DesktopWidget::Hil(w) => w.show(ui),
        }
    }

    fn reset_database(&mut self) {
        match self {
            DesktopWidget::Bootloader { widget, .. } => widget.reset_database(),
            DesktopWidget::SendUi(w) => w.reset_database(),
            DesktopWidget::LogParser(_) | DesktopWidget::Hil(_) => {}
        }
    }

    fn capture_spec(&self) -> workspace_io::PaneSpec {
        match self {
            DesktopWidget::Bootloader { widget, .. } => workspace_io::PaneSpec::Bootloader {
                title: widget.title.clone(),
            },
            DesktopWidget::LogParser(w) => workspace_io::PaneSpec::LogParser {
                title: w.title.clone(),
            },
            DesktopWidget::SendUi(w) => workspace_io::PaneSpec::SendUi {
                title: w.title.clone(),
            },
            DesktopWidget::Hil(w) => workspace_io::PaneSpec::Hil {
                title: w.title.clone(),
            },
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

pub struct DesktopPlatform {
    pub can_to_ui_rx: mpsc::Receiver<daqnative::can_thread::CanThreadEvent>,
    pub ui_to_can_tx: mpsc::Sender<daqnative::can_thread::CanThreadCommand>,
    pub can_thread: daqnative::can_thread::CanThreadHandle,
    pub selected_source: Option<daqnative::connection::ConnectionSource>,
    pub serial_ports: Vec<serialport::SerialPortInfo>,
    pub cannector_available: bool,
    pub pending_database: Option<u64>,
    pub selection_request: u64,
    pub can_bus_speed: daqnative::connection::CanBusSpeed,
    pub udp_port: u16,
    pub hil_snapshot: daqnative::hil::engine::HilSnapshot,
    pub active_source: Option<daqnative::connection::ConnectionSource>,
    pub settings_dirty_since: Option<std::time::Instant>,
}

impl DesktopPlatform {
    pub fn build_settings(&self, state: &ViewerState) -> settings::Settings {
        settings::Settings {
            superdbc_path: state
                .database
                .as_ref()
                .and_then(|selection| selection.source())
                .map(|source| source.path),
            bus_selection: state.database.as_ref().map(|selection| selection.selection),
            selected_source: self.selected_source.clone(),
            selected_speed: self.can_bus_speed,
            udp_port: self.udp_port,
            theme: state.theme_selection,
            pixels_per_point: state.pixels_per_point,
            log_folder: state.log_folder.clone(),
            window_secs: state.router.timeline().window_secs(),
            cloud_url: state.cloud_url.clone(),
            cloud_auto_connect: state.cloud_auto_connect,
            workspace_view: workspace_io::WorkspaceView::capture_from_state(
                state,
                "Last Session Workspace",
                "Auto-saved workspace",
                true,
            )
            .ok(),
        }
    }

    pub fn supports_multi_bus(&self) -> bool {
        self.selected_source
            .as_ref()
            .is_none_or(|source| source.supports_multi_bus())
    }

    pub fn request_database(
        &mut self,
        state: &mut ViewerState,
        path: &path::Path,
        preferred_selection: Option<daqnative::can_thread::BusSelection>,
    ) {
        let preferred_selection = if !self.supports_multi_bus()
            && preferred_selection.is_none_or(|selection| selection.is_multi_bus())
        {
            Some(daqnative::can_thread::BusSelection::Single(0))
        } else {
            preferred_selection
        };
        self.request_selection(
            state,
            daqnative::can_thread::DatabaseRequest::Load {
                path: path.to_owned(),
                preferred_selection,
            },
        );
    }

    pub fn request_bus_selection(
        &mut self,
        state: &mut ViewerState,
        new_selection: daqnative::can_thread::BusSelection,
    ) {
        let Some(selection) = &state.database else {
            return;
        };
        if selection.selection == new_selection {
            return;
        }
        if new_selection.resolve_buses(&selection.database).is_none() {
            state.diagnostic = Some(format!(
                "SuperDBC does not support selection {}",
                new_selection.display_name(&selection.database)
            ));
            return;
        }
        self.request_selection(
            state,
            daqnative::can_thread::DatabaseRequest::Bus {
                selection: new_selection,
            },
        );
    }

    pub fn enforce_single_bus_if_needed(&mut self, state: &mut ViewerState) {
        if !self.supports_multi_bus()
            && let Some(db) = &state.database
            && db.selection.is_multi_bus()
            && let Some(fallback) =
                daqnative::can_thread::BusSelection::default_single(&db.database)
        {
            self.request_bus_selection(state, fallback);
        }
    }

    fn request_selection(
        &mut self,
        state: &mut ViewerState,
        selection: daqnative::can_thread::DatabaseRequest,
    ) {
        if self.pending_database.is_some() {
            return;
        }
        self.selection_request += 1;
        let request_id = self.selection_request;
        let command = daqnative::can_thread::CanThreadCommand::SelectDatabase {
            request_id,
            selection,
        };
        match self.ui_to_can_tx.send(command) {
            Ok(()) => {
                self.pending_database = Some(request_id);
                state.selection_pending = true;
            }
            Err(error) => state.diagnostic = Some(error.to_string()),
        }
    }

    fn apply_database_result(
        &mut self,
        state: &mut ViewerState,
        request_id: u64,
        result: Result<daqnative::can_thread::DatabaseUpdate, String>,
    ) -> bool {
        if self.pending_database != Some(request_id) {
            return false;
        }
        self.pending_database = None;
        state.selection_pending = false;
        match result {
            Ok(daqnative::can_thread::DatabaseUpdate::Loaded {
                database,
                selection,
            }) => {
                state.database = Some(DatabaseSelection {
                    database: *database,
                    selection,
                });
            }
            Ok(daqnative::can_thread::DatabaseUpdate::Bus { selection }) => {
                let Some(db_selection) = &mut state.database else {
                    state.diagnostic = Some("No UI database is loaded".to_owned());
                    return false;
                };
                db_selection.selection = selection;
            }
            Err(error) => {
                state.diagnostic = Some(error);
                return false;
            }
        }
        if state.connection_status == ConnectionStatus::Connected
            || (state.router.partitions().is_empty() && state.router.timeline().is_live())
        {
            state.router.reset_live(daqnative::time::Time::now());
        } else {
            state.router.live_buffer_mut().clear();
        }
        state.bus_load_samples.clear();
        self.hil_snapshot = daqnative::hil::engine::HilSnapshot::idle();
        state.diagnostic = None;
        for tile in state.tile_tree.tiles.tiles_mut() {
            if let egui_tiles::Tile::Pane(widget) = tile {
                widget.reset_database();
            }
        }
        self.enforce_single_bus_if_needed(state);
        true
    }

    pub fn connect_can(&mut self, state: &mut ViewerState) {
        self.enforce_single_bus_if_needed(state);
        let Some(source) = &self.selected_source else {
            return;
        };

        state.connection_status = ConnectionStatus::Disconnected;

        let _ = self
            .ui_to_can_tx
            .send(daqnative::can_thread::CanThreadCommand::Connect(Some(
                source.clone(),
            )));
    }

    fn select_database(&mut self, state: &mut ViewerState) {
        let mut dialog = rfd::FileDialog::new().add_filter("SuperDBC JSON", &["json"]);
        if let Some(dir) = settings::database_dir() {
            dialog = dialog.set_directory(dir);
        }
        if let Some(path) = dialog.pick_file() {
            let preferred = state.database.as_ref().map(|selection| selection.selection);
            self.request_database(state, &path, preferred);
        }
    }

    fn show_database_controls(&mut self, ui: &mut eframe::egui::Ui, state: &mut ViewerState) {
        if ui
            .button("📁 Select Live SuperDBC (Decode / TX)")
            .on_hover_text(
                "Select SuperDBC used for live CAN frame decoding and message transmission (past logs use embedded partition metadata)",
            )
            .clicked()
        {
            self.select_database(state);
        }
        let Some(selection) = &state.database else {
            ui.label("Live SuperDBC: None selected");
            return;
        };
        if let Some(source) = selection.source() {
            ui.label(
                eframe::egui::RichText::new(
                    source
                        .path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy(),
                )
                .small()
                .weak(),
            );
        }
        let mut current_selection = selection.selection;
        let multi_bus_allowed = self.supports_multi_bus();
        eframe::egui::ComboBox::from_label("CAN Bus")
            .selected_text(current_selection.display_name(&selection.database))
            .show_ui(ui, |ui| {
                let presets = [
                    daqnative::can_thread::BusSelection::Vehicle,
                    daqnative::can_thread::BusSelection::Charger,
                ];
                for preset in presets {
                    let supported =
                        multi_bus_allowed && preset.resolve_buses(&selection.database).is_some();
                    ui.add_enabled_ui(supported, |ui| {
                        ui.selectable_value(
                            &mut current_selection,
                            preset,
                            preset.display_name(&selection.database),
                        );
                    });
                }
                ui.separator();
                for bus in selection.database.buses() {
                    let single = daqnative::can_thread::BusSelection::Single(bus.bus_id());
                    ui.selectable_value(
                        &mut current_selection,
                        single,
                        single.display_name(&selection.database),
                    );
                }
            });
        let prev_selection = selection.selection;
        if current_selection != prev_selection {
            self.request_bus_selection(state, current_selection);
        }
    }
}

impl Drop for DesktopPlatform {
    fn drop(&mut self) {
        if self.can_thread.stop().is_err() {
            log::error!("CAN worker panicked during shutdown");
        }
    }
}

impl ViewerPlatform for DesktopPlatform {
    fn supports_hardware_widgets(&self) -> bool {
        true
    }

    fn supports_native_file_dialogs(&self) -> bool {
        true
    }

    fn create_platform_widget(
        &mut self,
        ctor: &widget_constructor::WidgetConstructor,
        id: usize,
    ) -> Option<Box<dyn PlatformWidget>> {
        match ctor {
            widget_constructor::WidgetConstructor::Bootloader => {
                Some(Box::new(DesktopWidget::Bootloader {
                    widget: ui::bootloader::Bootloader::new(id),
                    ui_to_can_tx: self.ui_to_can_tx.clone(),
                }))
            }
            widget_constructor::WidgetConstructor::LogParser => Some(Box::new(
                DesktopWidget::LogParser(ui::log_parser::LogParser::new(id)),
            )),
            widget_constructor::WidgetConstructor::SendUi => Some(Box::new(DesktopWidget::SendUi(
                ui::send::SendUi::new(id, self.ui_to_can_tx.clone()),
            ))),
            widget_constructor::WidgetConstructor::Hil => Some(Box::new(DesktopWidget::Hil(
                ui::hil::Hil::new(self.ui_to_can_tx.clone()),
            ))),
            _ => None,
        }
    }

    fn instantiate_platform_pane(
        &mut self,
        spec: &workspace_io::PaneSpec,
        widget_ids: &mut widget_ids::WidgetIds,
    ) -> Option<Box<dyn PlatformWidget>> {
        match spec {
            workspace_io::PaneSpec::LogParser { title } => {
                let id = widget_ids.next(widget_constructor::WidgetKind::LogParser);
                let mut w = ui::log_parser::LogParser::new(id);
                if !title.is_empty() {
                    w.title = title.clone();
                }
                Some(Box::new(DesktopWidget::LogParser(w)))
            }
            workspace_io::PaneSpec::SendUi { title } => {
                let id = widget_ids.next(widget_constructor::WidgetKind::SendUi);
                let mut w = ui::send::SendUi::new(id, self.ui_to_can_tx.clone());
                if !title.is_empty() {
                    w.title = title.clone();
                }
                Some(Box::new(DesktopWidget::SendUi(w)))
            }
            workspace_io::PaneSpec::Bootloader { title } => {
                let id = widget_ids.next(widget_constructor::WidgetKind::Bootloader);
                let mut w = ui::bootloader::Bootloader::new(id);
                if !title.is_empty() {
                    w.title = title.clone();
                }
                Some(Box::new(DesktopWidget::Bootloader {
                    widget: w,
                    ui_to_can_tx: self.ui_to_can_tx.clone(),
                }))
            }
            workspace_io::PaneSpec::Hil { title } => {
                let _ = widget_ids.next(widget_constructor::WidgetKind::Hil);
                let mut w = ui::hil::Hil::new(self.ui_to_can_tx.clone());
                if !title.is_empty() {
                    w.title = title.clone();
                }
                Some(Box::new(DesktopWidget::Hil(w)))
            }
            _ => None,
        }
    }

    fn on_widget_spawned(&mut self, widget: &mut widgets::Widget) {
        if let widgets::Widget::Platform(pw) = widget
            && let Some(dw) = pw.as_any_mut().downcast_mut::<DesktopWidget>()
        {
            dw.handle_can_message(&daqnative::can_thread::CanThreadEvent::Hil(
                self.hil_snapshot.clone(),
            ));
        }
    }

    fn poll_platform(&mut self, state: &mut ViewerState, _ctx: &eframe::egui::Context) {
        if let Some(since) = self.settings_dirty_since
            && since.elapsed() >= std::time::Duration::from_millis(500)
        {
            self.save_settings(state);
        }
        while let Ok(event) = self.can_to_ui_rx.try_recv() {
            for tile in state.tile_tree.tiles.tiles_mut() {
                if let egui_tiles::Tile::Pane(widgets::Widget::Platform(pw)) = tile
                    && let Some(dw) = pw.as_any_mut().downcast_mut::<DesktopWidget>()
                {
                    dw.handle_can_message(&event);
                }
            }

            match event {
                daqnative::can_thread::CanThreadEvent::DatabaseSelected { request_id, result } => {
                    let applied = self.apply_database_result(state, request_id, result);
                    if applied {
                        self.save_settings(state);
                    }
                }
                daqnative::can_thread::CanThreadEvent::Frame(frame) => {
                    state.router.ingest_frame(frame);
                }
                daqnative::can_thread::CanThreadEvent::SourceSelected(Some(source)) => {
                    if self.active_source.as_ref() != Some(&source) {
                        state.router.reset_live(daqnative::time::Time::now());
                        state.bus_load_samples.clear();
                    }
                    self.active_source = Some(source);
                }
                daqnative::can_thread::CanThreadEvent::ConnectionFailed(error) => {
                    state.connection_status = ConnectionStatus::Error(error)
                }
                daqnative::can_thread::CanThreadEvent::ConnectionSuccessful => {
                    state.connection_status = ConnectionStatus::Connected
                }
                daqnative::can_thread::CanThreadEvent::Disconnection => {
                    state.connection_status = ConnectionStatus::Disconnected;
                }
                daqnative::can_thread::CanThreadEvent::BusLoad { timestamp, buses } => {
                    let sample = workspace_view::BusLoadSample { timestamp, buses };
                    let index = state
                        .bus_load_samples
                        .partition_point(|s| s.timestamp <= timestamp);
                    state.bus_load_samples.insert(index, sample);
                }
                daqnative::can_thread::CanThreadEvent::Hil(snapshot) => {
                    self.hil_snapshot = snapshot;
                }
                daqnative::can_thread::CanThreadEvent::Diagnostic(error)
                | daqnative::can_thread::CanThreadEvent::SendFailed { error, .. } => {
                    log::error!("{error}");
                    state.diagnostic = Some(error);
                }
                _ => {}
            }
        }
    }

    fn show_hardware_settings_ui(
        &mut self,
        ui: &mut eframe::egui::Ui,
        state: &mut ViewerState,
        muted_color: eframe::egui::Color32,
    ) {
        ui.label(
            eframe::egui::RichText::new("CONNECTION & HARDWARE")
                .small()
                .strong()
                .color(muted_color),
        );
        ui.add_space(4.0);

        ui.horizontal_wrapped(|ui| {
            ui.label("CAN Speed:");
            let speed_options = daqnative::connection::CanBusSpeed::options();
            let selected_speed = self.can_bus_speed;
            eframe::egui::ComboBox::from_id_salt("can_speed_combo")
                .selected_text(selected_speed.display_name())
                .show_ui(ui, |ui| {
                    for speed in speed_options {
                        if ui
                            .selectable_value(&mut self.can_bus_speed, speed, speed.display_name())
                            .changed()
                        {
                            self.serial_ports = util::get_available_serial_ports();
                            self.save_settings(state);
                        }
                    }
                });
        });

        ui.horizontal_wrapped(|ui| {
            ui.label("UDP Port:");
            if ui
                .add(eframe::egui::DragValue::new(&mut self.udp_port).range(1..=65535))
                .changed()
            {
                self.save_settings(state);
            }
        });

        ui.horizontal_wrapped(|ui| {
            let selected_text = match &self.selected_source {
                Some(daqnative::connection::ConnectionSource::Serial(path, speed)) => {
                    format!("Serial: {} ({})", path, speed.display_name())
                }
                Some(connection_source) => connection_source.display_name(),
                None => "Select Source".to_string(),
            };

            let mut do_connect = false;
            eframe::egui::ComboBox::from_label("Source")
                .selected_text(selected_text)
                .show_ui(ui, |ui| {
                    ui.label("Serial Ports");
                    let ports: Vec<_> = self
                        .serial_ports
                        .iter()
                        .map(|p| p.port_name.clone())
                        .collect();
                    for port_name in ports {
                        let source = daqnative::connection::ConnectionSource::Serial(
                            port_name.clone(),
                            self.can_bus_speed,
                        );
                        if ui
                            .selectable_value(
                                &mut self.selected_source,
                                Some(source),
                                format!("{} ({})", port_name, self.can_bus_speed.display_name()),
                            )
                            .changed()
                        {
                            do_connect = true;
                        }
                    }
                    ui.separator();
                    ui.label("CANnector");
                    if self.cannector_available
                        && ui
                            .selectable_value(
                                &mut self.selected_source,
                                Some(daqnative::connection::ConnectionSource::Cannector),
                                "CANnector (USB)",
                            )
                            .changed()
                    {
                        do_connect = true;
                    }
                    ui.separator();
                    ui.label("Network");
                    let udp_source = daqnative::connection::ConnectionSource::Udp(self.udp_port);
                    if ui
                        .selectable_value(
                            &mut self.selected_source,
                            Some(udp_source),
                            format!("UDP ({})", self.udp_port),
                        )
                        .changed()
                    {
                        do_connect = true;
                    }
                    ui.separator();
                    ui.label("Simulated");
                    for (connected, label) in [
                        (true, "Simulated (connected)"),
                        (false, "Simulated (disconnected)"),
                    ] {
                        let sim_source =
                            daqnative::connection::ConnectionSource::Simulated(connected);
                        if ui
                            .selectable_value(&mut self.selected_source, Some(sim_source), label)
                            .changed()
                        {
                            do_connect = true;
                        }
                    }
                    ui.separator();
                    ui.label("Development");
                    let loopback_source = daqnative::connection::ConnectionSource::Loopback;
                    if ui
                        .selectable_value(
                            &mut self.selected_source,
                            Some(loopback_source),
                            "Loopback",
                        )
                        .changed()
                    {
                        do_connect = true;
                    }
                });

            if do_connect {
                self.connect_can(state);
                self.save_settings(state);
            }

            if ui
                .button("🔄")
                .on_hover_text("Refresh serial & USB ports")
                .clicked()
            {
                self.serial_ports = util::get_available_serial_ports();
                self.cannector_available = util::is_cannector_available();
            }
        });

        ui.horizontal_wrapped(|ui| {
            let (status_icon, status_color) = match &state.connection_status {
                ConnectionStatus::Disconnected => {
                    ("■ Disconnected".to_string(), eframe::egui::Color32::GRAY)
                }
                ConnectionStatus::Connected => {
                    ("✔ Connected".to_string(), eframe::egui::Color32::GREEN)
                }
                ConnectionStatus::Error(e) => {
                    (format!("🔴 Error: {}", e), eframe::egui::Color32::RED)
                }
            };
            ui.label(eframe::egui::RichText::new(status_icon).color(status_color));
        });

        ui.separator();
        ui.label(
            eframe::egui::RichText::new("DATABASE & STORAGE")
                .small()
                .strong()
                .color(muted_color),
        );
        ui.add_space(2.0);

        let pending = self.pending_database.is_some();
        ui.add_enabled_ui(!pending, |ui| {
            self.show_database_controls(ui, state);
        });
        if pending {
            ui.label("Applying database selection…");
        }

        ui.horizontal_wrapped(|ui| {
            if ui.button("📁 Select Log Folder").clicked()
                && let Some(path) = rfd::FileDialog::new().pick_folder()
            {
                let command =
                    daqnative::can_thread::CanThreadCommand::UpdateLogFolder(path.clone());
                if let Err(error) = self.ui_to_can_tx.send(command) {
                    log::error!("Failed to send log folder update: {error}");
                    state.diagnostic = Some(error.to_string());
                    return;
                }
                state.log_folder = Some(path.clone());
                state
                    .action_queue
                    .push(action::AppAction::AttachStore(path));
                self.save_settings(state);
            }

            let log_display = state
                .log_folder
                .clone()
                .unwrap_or_else(|| path::PathBuf::from(settings::DEFAULT_LOG_FOLDER));

            ui.add(
                eframe::egui::Label::new(
                    eframe::egui::RichText::new(log_display.display().to_string())
                        .small()
                        .weak(),
                )
                .wrap(),
            );
        });

        ui.separator();
    }

    fn reload_formatter(&mut self) -> Option<daqcore::formatter::Formatter> {
        load_formatter()
    }

    fn save_settings(&mut self, state: &ViewerState) {
        self.settings_dirty_since = None;
        self.build_settings(state).save();
    }

    fn on_workspace_changed(&mut self, _state: &ViewerState) {
        if self.settings_dirty_since.is_none() {
            self.settings_dirty_since = Some(std::time::Instant::now());
        }
    }

    fn on_connect_cloud(&mut self, state: &mut ViewerState, url: String) {
        if state.router.store_dir().is_none() {
            let fallback_dir = state
                .log_folder
                .clone()
                .unwrap_or_else(|| path::PathBuf::from(settings::DEFAULT_LOG_FOLDER));
            if let Err(err) = state.router.attach_store(&fallback_dir) {
                state.diagnostic = Some(format!(
                    "Failed to initialize local store {} for daqcloud sync: {err}",
                    fallback_dir.display()
                ));
                return;
            }
            state.log_folder = Some(fallback_dir);
        }
        state.cloud_url = url.clone();
        if let Err(err) = state.router.connect_cloud(url) {
            state.diagnostic = Some(format!("Failed to connect to daqcloud: {err}"));
        } else {
            state.diagnostic = None;
            self.save_settings(state);
        }
    }

    fn pick_save_daqview_path(&mut self, _current_path: &str) -> Option<String> {
        rfd::FileDialog::new()
            .add_filter("DAQ Workspace View (.daqview.json)", &["json"])
            .set_file_name("view.daqview.json")
            .save_file()
            .map(|p| p.to_string_lossy().into_owned())
    }

    fn pick_load_daqview_file(&mut self) -> Option<Result<(String, String), String>> {
        let picked = rfd::FileDialog::new()
            .add_filter("DAQ Workspace View", &["json"])
            .pick_file()?;
        Some(
            workspace_io::WorkspaceView::load_from_file(&picked)
                .and_then(|v| v.to_json_pretty().map(|s| (v.name, s)))
                .map_err(|err| format!("Failed to load {}: {err}", picked.display())),
        )
    }

    fn pick_export_path(
        &mut self,
        format: daqcore::export::ExportFormat,
        _current_path: &str,
    ) -> Option<String> {
        let ext = format.extension();
        rfd::FileDialog::new()
            .add_filter(format.label(), &[ext])
            .save_file()
            .map(|p| p.to_string_lossy().into_owned())
    }

    fn execute_dataset_export(
        &mut self,
        router: &mut daqcore::router::VirtualTimelineRouter,
        range: RangeInclusive<i64>,
        signal_patterns: &[String],
        sampling_mode: daqcore::export::SamplingMode,
        format: daqcore::export::ExportFormat,
        output_path: &str,
    ) -> Result<daqcore::export::ExportSummary, daqcore::export::ExportError> {
        daqnative::export::export_dataset(
            router,
            range,
            signal_patterns,
            sampling_mode,
            format,
            output_path,
        )
    }
}

pub struct DAQApp {
    pub inner: ViewerApp<DesktopPlatform>,
}

impl DAQApp {
    pub fn new(
        can_to_ui_rx: mpsc::Receiver<daqnative::can_thread::CanThreadEvent>,
        can_thread: daqnative::can_thread::CanThreadHandle,
        mut settings: settings::Settings,
        cc: &eframe::CreationContext,
    ) -> Result<Self, Box<dyn error::Error + Send + Sync>> {
        let theme_selection = settings.theme;
        ui::theme::store_theme(&cc.egui_ctx, theme_selection.get_colors());

        egui_extras::install_image_loaders(&cc.egui_ctx);

        let window_secs = if settings.window_secs.is_finite() && settings.window_secs >= 0.0 {
            settings.window_secs
        } else {
            30.0
        };

        let mut router =
            daqnative::storage::live_router(daqnative::time::Time::now(), window_secs)?;
        let egui_ctx = cc.egui_ctx.clone();
        router.set_repaint_callback(Some(std::sync::Arc::new(move || {
            egui_ctx.request_repaint();
        })));
        let candidate_store_dirs = [
            settings.log_folder.clone(),
            Some(path::PathBuf::from(settings::DEFAULT_LOG_FOLDER)),
            Some(
                path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("..")
                    .join(settings::DEFAULT_LOG_FOLDER),
            ),
        ];
        if let Some(store_dir) = candidate_store_dirs.into_iter().flatten().find(|dir| {
            dir.join(daqnative::storage::LOCAL_REGISTRY_FILENAME)
                .exists()
                || dir.join("partitions").is_dir()
        }) {
            match router.attach_store(&store_dir) {
                Ok(_) => {
                    if let Some((day_min, day_max)) = router.timeline().day_bounds_ms() {
                        router.timeline_mut().snap_to_range_ms(day_min, day_max);
                    }
                    if settings.log_folder.is_none() {
                        settings.log_folder = Some(store_dir);
                    }
                }
                Err(err) => {
                    log::warn!(
                        "Failed to attach store directory {}: {err}",
                        store_dir.display()
                    );
                }
            }
        }
        let preferred_selection = settings.bus_selection;

        let ui_to_can_tx = can_thread.sender();
        let mut platform = DesktopPlatform {
            can_to_ui_rx,
            ui_to_can_tx,
            can_thread,
            selected_source: settings.selected_source.clone(),
            serial_ports: util::get_available_serial_ports(),
            cannector_available: util::is_cannector_available(),
            pending_database: None,
            selection_request: 0,
            can_bus_speed: settings.selected_speed,
            udp_port: settings.udp_port,
            hil_snapshot: daqnative::hil::engine::HilSnapshot::idle(),
            active_source: None,
            settings_dirty_since: None,
        };

        let mut state = ViewerState::new(
            router,
            theme_selection,
            settings.pixels_per_point,
            settings.cloud_url.clone(),
            settings.cloud_auto_connect,
            settings.log_folder,
            load_formatter(),
        );

        if let Some(saved_view) = &settings.workspace_view {
            let has_local_store = state.router.partitions().iter().any(|p| p.is_local);
            if let Err(err) = saved_view.apply_to_state(&mut state, &mut platform, has_local_store)
            {
                log::warn!("Failed to restore workspace view from settings: {err}");
            }
        }
        state.reset_undo_history();
        let default_dbc_path = settings.superdbc_path.or_else(|| {
            let db_dir = settings::database_dir()?;
            for candidate in ["superdbc_per26.json", "superdbc_1e6fa38.json"] {
                let path = db_dir.join(candidate);
                if path.is_file() {
                    return Some(path);
                }
            }
            None
        });
        if let Some(path) = default_dbc_path {
            platform.request_database(&mut state, &path, preferred_selection);
        }
        if state.cloud_auto_connect && !state.cloud_url.trim().is_empty() {
            state
                .action_queue
                .push(action::AppAction::ConnectCloud(state.cloud_url.clone()));
        }

        Ok(Self {
            inner: ViewerApp::new(state, platform),
        })
    }
}

impl eframe::App for DAQApp {
    fn update(&mut self, ctx: &eframe::egui::Context, frame: &mut eframe::Frame) {
        self.inner.update(ctx, frame);
    }

    fn on_exit(&mut self, gl: Option<&eframe::glow::Context>) {
        self.inner.on_exit(gl);
    }
}

impl Drop for DAQApp {
    fn drop(&mut self) {
        self.inner.platform.save_settings(&self.inner.state);
    }
}

pub fn load_formatter() -> Option<daqcore::formatter::Formatter> {
    let local = paths::read_file("formatter_config.json");
    if let Some(config) = local {
        match daqcore::formatter::Formatter::from_json(&config) {
            Ok(f) => return Some(f),
            Err(e) => log::warn!("Invalid formatter configuration: {e}"),
        }
    }
    daqviewer::load_embedded_formatter()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::WorkspaceTileBehavior;
    use daqcore::{analysis::DerivedSignalDef, time::Time};
    use daqnative::storage::NativeRouterExt as _;
    use std::path::PathBuf;

    fn sample_database() -> Option<DatabaseSelection> {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let dbc_path = manifest_dir.join("../../superdbc/superdbc_per26.json");
        let database = daqnative::superdbc::database::Database::load(&dbc_path).ok()?;
        let selection = daqnative::can_thread::BusSelection::default_single(&database)
            .unwrap_or(daqnative::can_thread::BusSelection::Single(0));
        Some(DatabaseSelection {
            database,
            selection,
        })
    }

    fn test_desktop_platform() -> DesktopPlatform {
        let (can_to_ui_tx, can_to_ui_rx) = mpsc::channel();
        let config = daqnative::can_thread::CanThreadConfig {
            log_folder: None,
            hil_dir: PathBuf::from("hil_config"),
        };
        let can_thread = daqnative::can_thread::spawn_can_thread(config, can_to_ui_tx)
            .expect("spawn test can_thread");
        let ui_to_can_tx = can_thread.sender();
        DesktopPlatform {
            can_to_ui_rx,
            ui_to_can_tx,
            can_thread,
            selected_source: None,
            serial_ports: Vec::new(),
            cannector_available: false,
            pending_database: None,
            selection_request: 0,
            can_bus_speed: daqnative::connection::CanBusSpeed::Kbps500,
            udp_port: 5000,
            hil_snapshot: daqnative::hil::engine::HilSnapshot::idle(),
            active_source: None,
            settings_dirty_since: None,
        }
    }

    fn build_all_widgets(
        db: Option<&DatabaseSelection>,
        platform: &mut DesktopPlatform,
    ) -> Vec<widgets::Widget> {
        let mut ids = widget_ids::WidgetIds::new();
        let constructors = [
            widget_constructor::WidgetConstructor::ViewerTable,
            widget_constructor::WidgetConstructor::ViewerList,
            widget_constructor::WidgetConstructor::Bootloader,
            widget_constructor::WidgetConstructor::ScopeEmpty,
            widget_constructor::WidgetConstructor::StateLane,
            widget_constructor::WidgetConstructor::XYScatter,
            widget_constructor::WidgetConstructor::LogParser,
            widget_constructor::WidgetConstructor::SendUi,
            widget_constructor::WidgetConstructor::BusLoad,
            widget_constructor::WidgetConstructor::GpsPlot,
            widget_constructor::WidgetConstructor::Jitter,
            widget_constructor::WidgetConstructor::Hil,
        ];
        let mut list: Vec<widgets::Widget> = constructors
            .into_iter()
            .flat_map(|c| c.create_all(&mut ids, platform, 0))
            .collect();

        if let Some(db) = db
            && let Some(msg) = db.database.messages().find(|m| !m.signals().is_empty())
            && let Some(sig) = msg.signals().first()
            && let Some(configured_scope) = (widget_constructor::WidgetConstructor::Scope {
                key: msg.key(),
                msg_name: msg.name().to_owned(),
                signal_name: sig.name().to_owned(),
            })
            .create(&mut ids, platform, 1)
        {
            list.push(configured_scope);
        }
        list
    }

    #[test]
    fn all_widgets_render_cleanly_across_extreme_tile_resizes() {
        let t0 = daqnative::time::Time::from_unix_millis(1_794_500_000_000);
        let mut router =
            daqnative::router::VirtualTimelineRouter::live(t0, 30.0).expect("live router");
        let db = sample_database();

        if let Some(db_sel) = &db {
            for step in 0..10 {
                let ts = t0.offset(step * 100);
                for msg in db_sel.database.messages().take(8) {
                    let key = msg.key();
                    let raw = vec![0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0];
                    let decoded = db_sel.database.decode(key.bus_id, key.identity, &raw).ok();
                    router.ingest_frame(daqnative::frame::ParsedFrame {
                        timestamp: ts,
                        bus_id: key.bus_id,
                        identity: key.identity,
                        kind: daqnative::frame::FrameKind::Data,
                        dlc: 8,
                        raw_bytes: raw,
                        decoded,
                    });
                }
            }
        }

        let bus_load = vec![workspace_view::BusLoadSample {
            timestamp: t0.offset(500),
            buses: vec![daqnative::can::bus_load::BusLoadReading {
                bus_id: 0,
                bus_name: "Main".to_owned(),
                values: [14.2, 12.8, 11.5, 10.1],
            }],
        }];
        let formatter = load_formatter();
        let mut platform = test_desktop_platform();

        let sizes = [
            eframe::egui::vec2(200.0, 700.0),
            eframe::egui::vec2(900.0, 120.0),
            eframe::egui::vec2(160.0, 110.0),
            eframe::egui::vec2(1280.0, 800.0),
        ];

        let mut all_widgets = build_all_widgets(db.as_ref(), &mut platform);
        assert!(all_widgets.len() >= 12);

        let ctx = eframe::egui::Context::default();
        ui::theme::store_theme(&ctx, ui::theme::ThemeSelection::default().get_colors());

        for widget in &mut all_widgets {
            let mut tiles = egui_tiles::Tiles::default();
            let pane_id = tiles.insert_pane(std::mem::replace(
                widget,
                widgets::Widget::ViewerList(ui::viewer_list::ViewerList::new(999)),
            ));
            let mut tree = egui_tiles::Tree::new("single_widget_test", pane_id, tiles);

            for &size in &sizes {
                let screen_rect = eframe::egui::Rect::from_min_size(eframe::egui::Pos2::ZERO, size);
                for _pass in 0..2 {
                    let mut raw_input = eframe::egui::RawInput {
                        screen_rect: Some(screen_rect),
                        ..Default::default()
                    };
                    raw_input.viewport_id = eframe::egui::ViewportId::ROOT;
                    let view = workspace_view::WorkspaceDataView::new(&router);
                    let mut action_queue = Vec::new();
                    let full_output = ctx.run(raw_input, |ctx| {
                        eframe::egui::CentralPanel::default()
                            .frame(eframe::egui::Frame::NONE)
                            .show(ctx, |ui| {
                                let mut behavior = WorkspaceTileBehavior {
                                    view: &view,
                                    bus_load: &bus_load,
                                    action_queue: &mut action_queue,
                                    database: db.as_ref(),
                                    formatter: &formatter,
                                    canvas_color: eframe::egui::Color32::BLACK,
                                    accent_color: eframe::egui::Color32::LIGHT_BLUE,
                                    muted_color: eframe::egui::Color32::GRAY,
                                };
                                tree.ui(&mut behavior, ui);
                            });
                    });

                    for prim in &full_output.shapes {
                        assert!(
                            prim.clip_rect.min.x >= -1.0
                                && prim.clip_rect.min.y >= -1.0
                                && prim.clip_rect.max.x <= size.x + 1.0
                                && prim.clip_rect.max.y <= size.y + 1.0,
                            "Clip rect {:?} exceeded screen rect {:?}",
                            prim.clip_rect,
                            screen_rect
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn per26_m6_analysis_widgets_v2_multi_axis_distance_overlay_state_lane_xy_scatter_and_gps_e2e()
    {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let dbc_path = manifest_dir.join("../../superdbc/superdbc_per26.json");
        let log_1 = manifest_dir
            .join("../../PER26/2026-09-06/1_no_cooling/raw/log-2026-09-06--18-36-02.log");
        let log_2 =
            manifest_dir.join("../../PER26/2026-09-06/2_regen/raw/log-2026-09-06--20-16-37.log");
        if !dbc_path.is_file() || !log_1.is_file() || !log_2.is_file() {
            return;
        }

        let db = sample_database().expect("load PER26 database");
        let tmp_raw = tempfile::tempdir().expect("temp raw dir");
        std::fs::copy(&log_1, tmp_raw.path().join("log-2026-09-06--18-36-02.log"))
            .expect("copy log 1");
        std::fs::copy(&log_2, tmp_raw.path().join("log-2026-09-06--20-16-37.log"))
            .expect("copy log 2");

        let tmp_store = tempfile::tempdir().expect("temp store dir");
        let summary = daqnative::log_parse::import_logs_to_store(
            tmp_raw.path(),
            tmp_store.path(),
            &db.database,
        )
        .expect("import PER26 logs");
        assert!(summary.partitions_written >= 2);

        let m1 = &summary.manifests[0];
        let m2 = &summary.manifests[1];
        let mut router = daqnative::storage::live_router(
            daqnative::time::Time::from_unix_millis(m2.end_utc_ms),
            30.0,
        )
        .expect("live router");
        router
            .attach_store(tmp_store.path())
            .expect("attach PER26 store");

        let mut ids = widget_ids::WidgetIds::new();
        let mut platform = test_desktop_platform();
        let gg_widgets = widget_constructor::WidgetConstructor::PresetGgDiagram.create_all(
            &mut ids,
            &mut platform,
            0,
        );
        assert_eq!(gg_widgets.len(), 1);
        let understeer_widgets = widget_constructor::WidgetConstructor::PresetHandlingUndersteer
            .create_all(&mut ids, &mut platform, 0);
        assert_eq!(understeer_widgets.len(), 2);
        let battery_widgets = widget_constructor::WidgetConstructor::PresetBatteryOverview
            .create_all(&mut ids, &mut platform, 0);
        assert_eq!(battery_widgets.len(), 3);

        router
            .timeline_mut()
            .snap_to_range_ms(m1.start_utc_ms, m1.end_utc_ms);
    }

    #[test]
    fn per26_m7_workspace_presets_daqview_share_strings_and_export_modal_e2e() {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let dbc_path = manifest_dir.join("../../superdbc/superdbc_per26.json");
        let log_1 = manifest_dir
            .join("../../PER26/2026-09-06/1_no_cooling/raw/log-2026-09-06--18-36-02.log");
        let log_2 =
            manifest_dir.join("../../PER26/2026-09-06/2_regen/raw/log-2026-09-06--20-16-37.log");
        if !dbc_path.is_file() || !log_1.is_file() || !log_2.is_file() {
            return;
        }

        let db_sel = sample_database().expect("load PER26 database");

        let tmp_raw = tempfile::tempdir().expect("temp raw dir");
        std::fs::copy(&log_1, tmp_raw.path().join("log-2026-09-06--18-36-02.log"))
            .expect("copy log 1");
        std::fs::copy(&log_2, tmp_raw.path().join("log-2026-09-06--20-16-37.log"))
            .expect("copy log 2");

        let tmp_store = tempfile::tempdir().expect("temp store dir");
        let summary = daqnative::log_parse::import_logs_to_store(
            tmp_raw.path(),
            tmp_store.path(),
            &db_sel.database,
        )
        .expect("import PER26 logs");
        assert!(summary.partitions_written >= 2);
        let m1 = &summary.manifests[0];
        let m2 = &summary.manifests[1];

        let mut router =
            daqnative::storage::live_router(Time::from_unix_millis(m2.end_utc_ms), 30.0)
                .expect("live router");
        router
            .attach_store(tmp_store.path())
            .expect("attach PER26 store");
        router
            .timeline_mut()
            .snap_to_range_ms(m1.start_utc_ms, m1.start_utc_ms + 15_000);
        router
            .timeline_mut()
            .set_setpoint(Time::from_unix_millis(m1.start_utc_ms + 7_500));

        let registry = workspace_io::WorkspacePresetRegistry::with_builtin();
        let mut ids = widget_ids::WidgetIds::new();
        let mut platform = test_desktop_platform();
        let mut tree = egui_tiles::Tree::empty("per26_m7_tree");
        let ctx = eframe::egui::Context::default();
        ui::theme::store_theme(&ctx, ui::theme::ThemeSelection::default().get_colors());

        for preset in registry.presets() {
            preset
                .apply_to_workspace(&mut tree, &mut ids, &mut router, &mut platform, false)
                .expect("apply subteam preset");
            let pane_count = tree
                .tiles
                .tiles()
                .filter(|t| matches!(t, egui_tiles::Tile::Pane(_)))
                .count();
            assert!(pane_count >= 4);
        }

        let vd_preset = registry
            .get("vd_handling_suspension")
            .expect("vd preset")
            .clone();
        vd_preset
            .apply_to_workspace(&mut tree, &mut ids, &mut router, &mut platform, false)
            .expect("apply vd preset");
        router
            .formula_engine_mut()
            .merge_defs(&[DerivedSignalDef {
                id: "vd.custom_shock_avg_mm".to_owned(),
                label: "Custom Shock Avg".to_owned(),
                expression: "0.25 * (front_shockpots.left + front_shockpots.right + rear_shockpots.left + rear_shockpots.right)".to_owned(),
                unit: "mm".to_owned(),
                ..Default::default()
            }])
            .expect("register custom formula");

        let saved_start = m1.start_utc_ms + 1_000;
        let saved_end = m1.start_utc_ms + 11_000;
        let saved_sp = m1.start_utc_ms + 5_500;
        router
            .timeline_mut()
            .snap_to_range_ms(saved_start, saved_end);
        router
            .timeline_mut()
            .set_setpoint(Time::from_unix_millis(saved_sp));

        let captured = workspace_io::WorkspaceView::capture_from_workspace(
            &tree,
            &router,
            "PER26 Stint 1 VD Analysis",
            "Saved during M7 E2E verification",
            true,
        )
        .expect("capture workspace");
        let share_str = captured.to_share_string().expect("encode share string");

        router
            .timeline_mut()
            .snap_to_range_ms(m2.start_utc_ms, m2.start_utc_ms + 20_000);
        tree = egui_tiles::Tree::empty("cleared");

        let decoded = workspace_io::WorkspaceView::from_str_flexible(&share_str)
            .expect("decode share string");
        decoded
            .apply_to_workspace(&mut tree, &mut ids, &mut router, &mut platform, false)
            .expect("apply layout only");
        assert_eq!(router.timeline().start_utc_ms(), m2.start_utc_ms);

        decoded
            .apply_to_workspace(&mut tree, &mut ids, &mut router, &mut platform, true)
            .expect("apply layout and time context");
        assert_eq!(router.timeline().start_utc_ms(), saved_start);
        assert_eq!(router.timeline().end_utc_ms(), saved_end);
        assert_eq!(router.timeline().setpoint_utc_ms(), saved_sp);

        let mut export_modal = ui::export_modal::ExportModalState::default();
        export_modal.open(&router);
        export_modal.apply_preset("vd", &router);
        export_modal.range_mode = ui::export_modal::UiExportRangeMode::CurrentViewport;

        let mut view_modal = workspace_io::WorkspaceViewModalState::default();
        view_modal.open();

        let screen_rect = eframe::egui::Rect::from_min_size(
            eframe::egui::Pos2::ZERO,
            eframe::egui::vec2(1400.0, 900.0),
        );
        let raw_input = eframe::egui::RawInput {
            screen_rect: Some(screen_rect),
            ..Default::default()
        };
        let _ = ctx.run(raw_input, |ctx| {
            export_modal.show(ctx, &mut router, &mut platform);
            view_modal.show(
                ctx,
                &mut tree,
                &mut ids,
                &mut router,
                &mut platform,
                "http://127.0.0.1:3000",
            );
        });

        let export_dir = tempfile::tempdir().expect("export tempdir");
        for (fmt, ext) in [
            (daqcore::export::ExportFormat::Parquet, "parquet"),
            (daqcore::export::ExportFormat::Csv, "csv"),
            (daqcore::export::ExportFormat::MatlabMat, "mat"),
        ] {
            export_modal.set_format(fmt);
            let out_path = export_dir.path().join(format!("per26_vd.{ext}"));
            export_modal.output_path = out_path.to_string_lossy().into_owned();
            let summary = export_modal
                .execute_export(&mut router, &mut platform)
                .expect("execute modal export");
            assert!(summary.row_count > 500);
            assert!(out_path.is_file());
        }
    }

    #[test]
    fn settings_persists_and_reopens_workspace_across_sessions() {
        let registry = workspace_io::WorkspacePresetRegistry::with_builtin();
        let preset = registry
            .get("powertrain_accumulator")
            .expect("powertrain preset");

        let mut router = daqnative::router::VirtualTimelineRouter::live(
            Time::from_unix_millis(1_788_719_800_000),
            30.0,
        )
        .expect("live router");
        router
            .timeline_mut()
            .snap_to_range_ms(1_788_719_762_000, 1_788_719_782_000);
        router
            .timeline_mut()
            .set_setpoint(Time::from_unix_millis(1_788_719_770_000));
        router
            .formula_engine_mut()
            .merge_defs(&[DerivedSignalDef {
                id: "battery.custom_resistance_mohm".to_owned(),
                label: "Pack Internal Resistance".to_owned(),
                expression: "abs(pack_stats.pack_current) * 10.0".to_owned(),
                unit: "mOhm".to_owned(),
                ..Default::default()
            }])
            .expect("merge custom formula");

        let mut ids = widget_ids::WidgetIds::new();
        let mut platform = test_desktop_platform();
        let mut tree = egui_tiles::Tree::empty("session_1");
        preset
            .apply_to_workspace(&mut tree, &mut ids, &mut router, &mut platform, false)
            .expect("apply preset in session 1");
        let expected_panes = tree
            .tiles
            .tiles()
            .filter(|t| matches!(t, egui_tiles::Tile::Pane(_)))
            .count();
        assert!(expected_panes >= 4);

        let tmp_dir = tempfile::tempdir().expect("tempdir");
        let settings_path = tmp_dir.path().join("settings.json");
        let settings = crate::settings::Settings {
            window_secs: router.timeline().window_secs(),
            workspace_view: workspace_io::WorkspaceView::capture_from_workspace(
                &tree,
                &router,
                "Last Session Workspace",
                "Auto-saved workspace",
                true,
            )
            .ok(),
            ..Default::default()
        };
        assert!(settings.workspace_view.is_some());
        settings.save_to(&settings_path);

        let reopened_settings =
            crate::settings::Settings::load_from(&settings_path).expect("load saved settings.json");
        let saved_view = reopened_settings
            .workspace_view
            .expect("workspace_view present in settings.json");

        let mut reopened_router = daqnative::router::VirtualTimelineRouter::live(
            Time::from_unix_millis(1_788_720_000_000),
            reopened_settings.window_secs,
        )
        .expect("reopened router");
        let mut reopened_ids = widget_ids::WidgetIds::new();
        let mut reopened_tree = egui_tiles::Tree::empty("session_2");
        saved_view
            .apply_to_workspace(
                &mut reopened_tree,
                &mut reopened_ids,
                &mut reopened_router,
                &mut platform,
                true,
            )
            .expect("restore workspace view from settings");

        let restored_panes = reopened_tree
            .tiles
            .tiles()
            .filter(|t| matches!(t, egui_tiles::Tile::Pane(_)))
            .count();
        assert_eq!(restored_panes, expected_panes);
        assert_eq!(reopened_router.timeline().start_utc_ms(), 1_788_719_762_000);
        assert_eq!(reopened_router.timeline().end_utc_ms(), 1_788_719_782_000);
        assert_eq!(
            reopened_router.timeline().setpoint_utc_ms(),
            1_788_719_770_000
        );
        assert!(
            reopened_router
                .formula_engine()
                .get("battery.custom_resistance_mohm")
                .is_some()
        );
    }

    #[test]
    fn per26_multi_stint_day_timeline_gap_folding_comparison_and_signal_browser_e2e() {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let repo_root = manifest_dir.join("../..");
        let dbc_path = repo_root.join("superdbc/superdbc_per26.json");
        let log1 = repo_root.join("PER26/2026-09-06/1_no_cooling/raw/log-2026-09-06--18-36-02.log");
        let log2 = repo_root.join("PER26/2026-09-06/2_regen/raw/log-2026-09-06--20-21-44.log");
        if !log1.exists() || !log2.exists() || !dbc_path.exists() {
            return;
        }

        let database =
            daqnative::superdbc::database::Database::load(&dbc_path).expect("load PER26");
        let tmp_store = tempfile::tempdir().expect("temp store");
        let tmp_raw1 = tempfile::tempdir().expect("temp raw 1");
        let tmp_raw2 = tempfile::tempdir().expect("temp raw 2");
        std::fs::copy(&log1, tmp_raw1.path().join("log-2026-09-06--18-36-02.log"))
            .expect("copy log1");
        std::fs::copy(&log2, tmp_raw2.path().join("log-2026-09-06--20-21-44.log"))
            .expect("copy log2");

        let summary_1 = daqnative::log_parse::import_logs_to_store(
            tmp_raw1.path(),
            tmp_store.path(),
            &database,
        )
        .expect("import stint 1");
        let summary_2 = daqnative::log_parse::import_logs_to_store(
            tmp_raw2.path(),
            tmp_store.path(),
            &database,
        )
        .expect("import stint 2");
        assert!(!summary_1.manifests.is_empty() && !summary_2.manifests.is_empty());

        let m1_start = summary_1.manifests[0].start_utc_ms;
        let mut router = daqnative::router::VirtualTimelineRouter::open_store(
            tmp_store.path(),
            daqnative::time::Time::from_unix_millis(m1_start),
            30.0,
        )
        .expect("open_store router");

        assert_eq!(router.partitions().len(), 2);
        let track_days = router.list_track_days().expect("track_days");
        assert!(track_days.iter().any(|td| td.id == "2026-09-06"));
        router
            .select_track_day(Some("2026-09-06".to_owned()))
            .expect("select 2026-09-06");
    }
}
