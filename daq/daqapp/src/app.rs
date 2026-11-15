use crate::{
    action, paths, settings, shortcuts, ui, util, widget_ids, widgets, workspace, workspace_io,
    workspace_view,
};
use std::{error, mem, path, sync::mpsc};

const UI_SCALE_STEP: f32 = 0.2;

pub struct DatabaseSelection {
    pub database: daqcore::superdbc::database::Database,
    pub selection: daqcore::can_thread::BusSelection,
}

impl DatabaseSelection {
    pub fn active_buses(&self) -> Vec<&daqcore::superdbc::database::Bus> {
        self.selection
            .resolve_buses(&self.database)
            .unwrap_or_default()
    }

    pub fn bus_name(&self, bus_id: u8) -> &str {
        self.database
            .bus_by_id(bus_id)
            .map_or("Unknown", |bus| bus.name())
    }

    pub fn source(&self) -> Option<daqcore::can_thread::DatabaseSource> {
        Some(daqcore::can_thread::DatabaseSource {
            path: self.database.path()?.to_owned(),
            selection: self.selection,
        })
    }
}

#[derive(Debug, PartialEq, Clone)]
pub enum ConnectionStatus {
    Disconnected,
    Connected,
    Error(String),
}

pub struct DAQApp {
    pub connection_status: ConnectionStatus,
    pub value_formatter: Option<daqcore::formatter::Formatter>,
    pub is_sidebar_open: bool,
    pub sidebar_tab: ui::sidebar::SidebarTab,
    pub active_workspace_tab: Option<String>,
    pub show_share_popover: bool,
    pub command_palette: ui::command_palette::CommandPalette,
    pub tile_tree: egui_tiles::Tree<widgets::Widget>,
    pub widget_ids: widget_ids::WidgetIds,
    pub can_to_ui_rx: mpsc::Receiver<daqcore::can_thread::CanThreadEvent>,
    pub ui_to_can_tx: mpsc::Sender<daqcore::can_thread::CanThreadCommand>,
    pub action_queue: Vec<action::AppAction>,
    pub selected_source: Option<daqcore::connection::ConnectionSource>,
    pub theme: eframe::egui::Style,
    pub theme_selection: ui::theme::ThemeSelection,
    pub pixels_per_point: Option<f32>,
    pub serial_ports: Vec<serialport::SerialPortInfo>,
    pub cannector_available: bool,
    pub database: Option<DatabaseSelection>,
    pending_database: Option<u64>,
    selection_request: u64,
    pub can_bus_speed: daqcore::connection::CanBusSpeed,
    pub udp_port: u16,
    pub router: daqcore::router::VirtualTimelineRouter,
    pub bus_load_samples: Vec<workspace_view::BusLoadSample>,
    pub hover_utc_ms: Option<i64>,
    pub hover_distance_m: Option<f64>,
    pub can_thread: daqcore::can_thread::CanThreadHandle,
    hil_snapshot: daqcore::hil::engine::HilSnapshot,
    active_source: Option<daqcore::connection::ConnectionSource>,
    pub diagnostic: Option<String>,
    pub log_folder: Option<path::PathBuf>,
    pub timeline_ui: ui::timeline::TimelineUiState,
    pub signal_browser: ui::signal_browser::SignalBrowserState,
    pub workspace_presets: workspace_io::WorkspacePresetRegistry,
    pub workspace_view_modal: workspace_io::WorkspaceViewModalState,
    pub export_modal: ui::export_modal::ExportModalState,
    frame_times: std::collections::VecDeque<std::time::Instant>,
    pub fps: f32,
    pub frame_time_ms: f32,
}

impl DAQApp {
    pub fn build_settings(&self) -> settings::Settings {
        settings::Settings {
            superdbc_path: self
                .database
                .as_ref()
                .and_then(|selection| selection.source())
                .map(|source| source.path),
            bus_selection: self.database.as_ref().map(|selection| selection.selection),
            selected_source: self.selected_source.clone(),
            selected_speed: self.can_bus_speed,
            udp_port: self.udp_port,
            theme: self.theme_selection,
            pixels_per_point: self.pixels_per_point,
            log_folder: self.log_folder.clone(),
            window_secs: self.router.timeline().window_secs(),
            workspace_view: workspace_io::WorkspaceView::capture_from_app(
                self,
                "Last Session Workspace",
                "Auto-saved workspace",
                true,
            )
            .ok(),
        }
    }

    pub fn save_settings(&self) {
        self.build_settings().save();
    }

    pub fn new(
        can_to_ui_rx: mpsc::Receiver<daqcore::can_thread::CanThreadEvent>,
        can_thread: daqcore::can_thread::CanThreadHandle,
        mut settings: settings::Settings,
        cc: &eframe::CreationContext,
    ) -> Result<Self, Box<dyn error::Error + Send + Sync>> {
        let theme_selection = settings.theme;
        let theme_style = theme_selection.get_style();
        ui::theme::store_theme(&cc.egui_ctx, theme_selection.get_colors());

        egui_extras::install_image_loaders(&cc.egui_ctx);

        let window_secs = if settings.window_secs.is_finite() && settings.window_secs >= 0.0 {
            settings.window_secs
        } else {
            30.0
        };

        let mut router =
            daqcore::router::VirtualTimelineRouter::live(daqcore::time::Time::now(), window_secs)?;
        let egui_ctx = cc.egui_ctx.clone();
        router.set_repaint_callback(Some(std::sync::Arc::new(move || {
            egui_ctx.request_repaint();
        })));
        let candidate_store_dirs = [
            settings.log_folder.clone(),
            Some(path::PathBuf::from(settings::DEFAULT_LOG_FOLDER)),
            Some(path::PathBuf::from("/tmp/per26_store")),
        ];
        if let Some(store_dir) = candidate_store_dirs.into_iter().flatten().find(|dir| {
            dir.join(daqcore::storage::LOCAL_REGISTRY_FILENAME).exists()
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

        let mut app = Self {
            connection_status: ConnectionStatus::Disconnected,
            value_formatter: load_formatter(),
            is_sidebar_open: true,
            sidebar_tab: ui::sidebar::SidebarTab::default(),
            active_workspace_tab: None,
            show_share_popover: false,
            command_palette: ui::command_palette::CommandPalette::new(),
            tile_tree: egui_tiles::Tree::empty("workspace_tree"),
            widget_ids: widget_ids::WidgetIds::new(),
            can_to_ui_rx,
            ui_to_can_tx: can_thread.sender(),
            can_thread,
            action_queue: Vec::new(),
            selected_source: settings.selected_source.clone(),
            theme: theme_style,
            theme_selection,
            pixels_per_point: settings.pixels_per_point,
            serial_ports: util::get_available_serial_ports(),
            cannector_available: util::is_cannector_available(),
            database: None,
            pending_database: None,
            selection_request: 0,
            can_bus_speed: settings.selected_speed,
            udp_port: settings.udp_port,
            router,
            bus_load_samples: Vec::new(),
            hover_utc_ms: None,
            hover_distance_m: None,
            hil_snapshot: daqcore::hil::engine::HilSnapshot::idle(),
            active_source: None,
            diagnostic: None,
            log_folder: settings.log_folder,
            timeline_ui: ui::timeline::TimelineUiState::default(),
            signal_browser: ui::signal_browser::SignalBrowserState::default(),
            workspace_presets: workspace_io::WorkspacePresetRegistry::with_builtin(),
            workspace_view_modal: workspace_io::WorkspaceViewModalState::default(),
            export_modal: ui::export_modal::ExportModalState::default(),
            frame_times: std::collections::VecDeque::with_capacity(256),
            fps: 0.0,
            frame_time_ms: 0.0,
        };
        if let Some(saved_view) = &settings.workspace_view {
            let has_local_store = app.router.partitions().iter().any(|p| p.is_local);
            if let Err(err) = saved_view.apply_to_app(&mut app, has_local_store) {
                log::warn!("Failed to restore workspace view from settings: {err}");
            }
        }
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
            app.request_database(&path, preferred_selection);
        }
        Ok(app)
    }

    #[cfg(test)]
    pub fn new_for_test(ctx: &eframe::egui::Context) -> Self {
        egui_extras::install_image_loaders(ctx);
        let theme_selection = ui::theme::ThemeSelection::default();
        let theme = theme_selection.get_style();
        ui::theme::store_theme(ctx, theme_selection.get_colors());
        let (can_to_ui_tx, can_to_ui_rx) = mpsc::channel();
        let config = daqcore::can_thread::CanThreadConfig {
            log_folder: None,
            hil_dir: path::PathBuf::from("hil_config"),
        };
        let can_thread = daqcore::can_thread::spawn_can_thread(config, can_to_ui_tx)
            .expect("spawn test can_thread");
        let router = daqcore::router::VirtualTimelineRouter::live(
            daqcore::time::Time::from_unix_millis(1_794_500_000_000),
            30.0,
        )
        .expect("live router");

        Self {
            connection_status: ConnectionStatus::Disconnected,
            value_formatter: load_formatter(),
            is_sidebar_open: true,
            sidebar_tab: ui::sidebar::SidebarTab::default(),
            active_workspace_tab: None,
            show_share_popover: false,
            command_palette: ui::command_palette::CommandPalette::new(),
            tile_tree: egui_tiles::Tree::empty("workspace_tree"),
            widget_ids: widget_ids::WidgetIds::new(),
            can_to_ui_rx,
            ui_to_can_tx: can_thread.sender(),
            can_thread,
            action_queue: Vec::new(),
            selected_source: None,
            theme,
            theme_selection,
            pixels_per_point: None,
            serial_ports: Vec::new(),
            cannector_available: false,
            database: None,
            pending_database: None,
            selection_request: 0,
            can_bus_speed: daqcore::connection::CanBusSpeed::Kbps500,
            udp_port: 5000,
            router,
            bus_load_samples: Vec::new(),
            hover_utc_ms: None,
            hover_distance_m: None,
            hil_snapshot: daqcore::hil::engine::HilSnapshot::idle(),
            active_source: None,
            diagnostic: None,
            log_folder: None,
            timeline_ui: ui::timeline::TimelineUiState::default(),
            signal_browser: ui::signal_browser::SignalBrowserState::default(),
            workspace_presets: workspace_io::WorkspacePresetRegistry::with_builtin(),
            workspace_view_modal: workspace_io::WorkspaceViewModalState::default(),
            export_modal: ui::export_modal::ExportModalState::default(),
            frame_times: std::collections::VecDeque::with_capacity(256),
            fps: 60.0,
            frame_time_ms: 16.6,
        }
    }

    pub fn selection_pending(&self) -> bool {
        self.pending_database.is_some()
    }

    pub fn supports_multi_bus(&self) -> bool {
        self.selected_source
            .as_ref()
            .is_none_or(|source| source.supports_multi_bus())
    }

    pub fn request_database(
        &mut self,
        path: &path::Path,
        preferred_selection: Option<daqcore::can_thread::BusSelection>,
    ) {
        let preferred_selection = if !self.supports_multi_bus()
            && preferred_selection.is_none_or(|selection| selection.is_multi_bus())
        {
            Some(daqcore::can_thread::BusSelection::Single(0))
        } else {
            preferred_selection
        };
        self.request_selection(daqcore::can_thread::DatabaseRequest::Load {
            path: path.to_owned(),
            preferred_selection,
        });
    }

    pub fn request_bus_selection(&mut self, new_selection: daqcore::can_thread::BusSelection) {
        let Some(selection) = &self.database else {
            return;
        };
        if selection.selection == new_selection {
            return;
        }
        if new_selection.resolve_buses(&selection.database).is_none() {
            self.diagnostic = Some(format!(
                "SuperDBC does not support selection {}",
                new_selection.display_name(&selection.database)
            ));
            return;
        }
        self.request_selection(daqcore::can_thread::DatabaseRequest::Bus {
            selection: new_selection,
        });
    }

    pub fn enforce_single_bus_if_needed(&mut self) {
        if !self.supports_multi_bus()
            && let Some(db) = &self.database
            && db.selection.is_multi_bus()
            && let Some(fallback) = daqcore::can_thread::BusSelection::default_single(&db.database)
        {
            self.request_bus_selection(fallback);
        }
    }

    fn request_selection(&mut self, selection: daqcore::can_thread::DatabaseRequest) {
        if self.selection_pending() {
            return;
        }
        self.selection_request += 1;
        let request_id = self.selection_request;
        let command = daqcore::can_thread::CanThreadCommand::SelectDatabase {
            request_id,
            selection,
        };
        match self.ui_to_can_tx.send(command) {
            Ok(()) => self.pending_database = Some(request_id),
            Err(error) => self.diagnostic = Some(error.to_string()),
        }
    }

    fn apply_database_result(
        &mut self,
        request_id: u64,
        result: Result<daqcore::can_thread::DatabaseUpdate, String>,
    ) -> bool {
        if self.pending_database != Some(request_id) {
            return false;
        }
        self.pending_database = None;
        match result {
            Ok(daqcore::can_thread::DatabaseUpdate::Loaded {
                database,
                selection,
            }) => {
                self.database = Some(DatabaseSelection {
                    database: *database,
                    selection,
                });
            }
            Ok(daqcore::can_thread::DatabaseUpdate::Bus { selection }) => {
                let Some(db_selection) = &mut self.database else {
                    self.diagnostic = Some("No UI database is loaded".to_owned());
                    return false;
                };
                db_selection.selection = selection;
            }
            Err(error) => {
                self.diagnostic = Some(error);
                return false;
            }
        }
        if self.connection_status == ConnectionStatus::Connected
            || self.router.partitions().is_empty()
        {
            self.router.reset_live(daqcore::time::Time::now());
        } else {
            self.router.live_buffer_mut().clear();
        }
        self.bus_load_samples.clear();
        self.hil_snapshot = daqcore::hil::engine::HilSnapshot::idle();
        self.diagnostic = None;
        for tile in self.tile_tree.tiles.tiles_mut() {
            if let egui_tiles::Tile::Pane(widget) = tile {
                widget.reset_database();
            }
        }
        self.enforce_single_bus_if_needed();
        true
    }

    fn add_widget_to_tree(&mut self, widget: widgets::Widget) {
        let new_tile_id = self.tile_tree.tiles.insert_pane(widget);

        // No root yet, this becomes the root
        let Some(root_id) = self.tile_tree.root else {
            self.tile_tree.root = Some(new_tile_id);
            return;
        };

        // Check if root is already a tab container
        let Some(egui_tiles::Tile::Container(egui_tiles::Container::Tabs(tabs))) =
            self.tile_tree.tiles.get_mut(root_id)
        else {
            // Root is not a tab container, create one
            let tab_container = self
                .tile_tree
                .tiles
                .insert_tab_tile(vec![root_id, new_tile_id]);
            self.tile_tree.root = Some(tab_container);
            return;
        };

        // Root is already a tab container, add to it
        tabs.add_child(new_tile_id);
        tabs.set_active(new_tile_id);
    }

    pub fn connect_can(&mut self) {
        self.enforce_single_bus_if_needed();
        let Some(source) = &self.selected_source else {
            return;
        };

        self.connection_status = ConnectionStatus::Disconnected;

        let _ = self
            .ui_to_can_tx
            .send(daqcore::can_thread::CanThreadCommand::Connect(Some(
                source.clone(),
            )));
    }

    pub fn handle_action(&mut self, action: action::AppAction, ctx: &eframe::egui::Context) {
        match action {
            action::AppAction::SpawnWidget(widget_type) => {
                let kind = widget_type.kind();
                let existing_count = self
                    .tile_tree
                    .tiles
                    .tiles()
                    .filter(|tile| matches!(tile, egui_tiles::Tile::Pane(w) if w.kind() == kind))
                    .count();
                let created = widget_type.create_all(
                    &mut self.widget_ids,
                    self.ui_to_can_tx.clone(),
                    existing_count,
                );
                if created.is_empty() {
                    log::warn!("Maximum number of {:?} widgets already open", kind);
                } else {
                    for mut widget in created {
                        widget.handle_operational_event(&daqcore::can_thread::CanThreadEvent::Hil(
                            self.hil_snapshot.clone(),
                        ));
                        self.add_widget_to_tree(widget);
                    }
                    self.save_settings();
                }
            }
            action::AppAction::AttachStore(store_dir) => {
                match self.router.attach_store(&store_dir) {
                    Ok(_) => {
                        self.log_folder = Some(store_dir);
                        self.save_settings();
                        if self.connection_status != ConnectionStatus::Connected
                            && let Some((day_min, day_max)) = self.router.timeline().day_bounds_ms()
                        {
                            self.router
                                .timeline_mut()
                                .snap_to_range_ms(day_min, day_max);
                        }
                    }
                    Err(err) => {
                        self.diagnostic = Some(format!(
                            "Failed to attach store {}: {err}",
                            store_dir.display()
                        ));
                    }
                }
            }
            action::AppAction::OpenAnnotationModal => {
                self.timeline_ui.open_new_at_playhead(&self.router, true);
            }
            action::AppAction::LoadWorkspacePreset {
                preset_id,
                apply_time_context,
            } => {
                if let Some(preset) = self.workspace_presets.get(&preset_id).cloned() {
                    if let Err(err) = preset.apply_to_workspace(
                        &mut self.tile_tree,
                        &mut self.widget_ids,
                        &mut self.router,
                        self.ui_to_can_tx.clone(),
                        apply_time_context,
                    ) {
                        self.diagnostic = Some(format!(
                            "Failed to apply workspace preset '{preset_id}': {err}"
                        ));
                    } else {
                        self.active_workspace_tab = Some(preset_id);
                        self.save_settings();
                    }
                } else {
                    self.diagnostic = Some(format!("Unknown workspace preset: {preset_id}"));
                }
            }
            action::AppAction::OpenWorkspaceViewModal => {
                self.workspace_view_modal.open();
            }
            action::AppAction::CopyWorkspaceShareString => {
                if let Err(err) =
                    self.workspace_view_modal
                        .generate_and_copy(ctx, &self.tile_tree, &self.router)
                {
                    self.diagnostic =
                        Some(format!("Failed to generate workspace share string: {err}"));
                } else {
                    self.show_share_popover = true;
                }
            }
            action::AppAction::OpenExportDataModal => {
                self.export_modal.open(&self.router);
            }
            action::AppAction::ToggleSidebar => {
                self.is_sidebar_open = !self.is_sidebar_open;
            }
            action::AppAction::FocusSidebarSearch => {
                self.is_sidebar_open = true;
                let search_id = if self.sidebar_tab == ui::sidebar::SidebarTab::Sessions {
                    eframe::egui::Id::new("chunk_navigator_search_input")
                } else {
                    self.sidebar_tab = ui::sidebar::SidebarTab::Channels;
                    eframe::egui::Id::new("signal_browser_search_input")
                };
                ctx.memory_mut(|m| m.request_focus(search_id));
            }
            action::AppAction::ToggleCommandPalette => {
                self.command_palette.toggle();
            }
            action::AppAction::CloseActiveWidget => {
                self.close_active_widget();
                self.save_settings();
            }
            action::AppAction::ClearWorkspace => {
                self.tile_tree = egui_tiles::Tree::empty("workspace_tree");
                self.widget_ids = widget_ids::WidgetIds::new();
                self.active_workspace_tab = None;
                self.save_settings();
            }
            action::AppAction::RenamePane { tile_id, title } => {
                if let Some(egui_tiles::Tile::Pane(widget)) = self.tile_tree.tiles.get_mut(tile_id)
                {
                    widget.set_title(title);
                    self.save_settings();
                }
            }
            action::AppAction::ResetPaneTitle { tile_id } => {
                if let Some(egui_tiles::Tile::Pane(widget)) = self.tile_tree.tiles.get_mut(tile_id)
                {
                    widget.reset_auto_title();
                    self.save_settings();
                }
            }
            action::AppAction::ClosePane { tile_id } => {
                self.tile_tree.tiles.remove(tile_id);
                self.save_settings();
            }
            action::AppAction::IncreaseScale => {
                let current_scale = self
                    .pixels_per_point
                    .unwrap_or_else(|| ctx.pixels_per_point());
                self.pixels_per_point = Some(current_scale + UI_SCALE_STEP);
                self.save_settings();
            }
            action::AppAction::DecreaseScale => {
                let current_scale = self
                    .pixels_per_point
                    .unwrap_or_else(|| ctx.pixels_per_point());
                self.pixels_per_point = Some(current_scale - UI_SCALE_STEP);
                self.save_settings();
            }
            action::AppAction::TogglePlayback => {
                self.router.timeline_mut().toggle_playing();
            }
            action::AppAction::StepPlayhead { direction, fine } => {
                let tl = self.router.timeline_mut();
                let step_ms = if fine {
                    10
                } else {
                    (tl.window_ms() / 100).max(10)
                };
                tl.step_by_ms(direction.saturating_mul(step_ms));
            }
            action::AppAction::PanWindow { direction } => {
                let tl = self.router.timeline_mut();
                let pan_ms = (tl.window_ms() / 10).max(50);
                let new_start = tl
                    .start_utc_ms()
                    .saturating_add(direction.saturating_mul(pan_ms));
                tl.pan_to_start_ms(new_start);
            }
            action::AppAction::ZoomWindow { factor } => {
                let tl = self.router.timeline_mut();
                let anchor_ms = self.hover_utc_ms.unwrap_or_else(|| tl.setpoint_utc_ms());
                tl.zoom_around_ms(anchor_ms, factor);
                self.save_settings();
            }
            action::AppAction::JumpStint { direction } => {
                let active_day_id = self
                    .router
                    .timeline()
                    .active_track_day_id()
                    .map(str::to_owned);
                let mut stints: Vec<daqcore::storage::Annotation> = self
                    .router
                    .query_annotations_in_range(i64::MIN / 2..=i64::MAX / 2)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|a| {
                        a.category != "fault"
                            && active_day_id.as_deref().is_none_or(|d| a.track_day_id == d)
                    })
                    .collect();
                stints.sort_by_key(|a| (a.start_utc_ms, a.end_utc_ms.unwrap_or(a.start_utc_ms)));
                if !stints.is_empty() {
                    let cur_start = self.router.timeline().start_utc_ms();
                    let target = if direction < 0 {
                        stints
                            .iter()
                            .rev()
                            .find(|a| a.start_utc_ms < cur_start - 500)
                            .or_else(|| stints.last())
                    } else {
                        stints
                            .iter()
                            .find(|a| a.start_utc_ms > cur_start + 500)
                            .or_else(|| stints.first())
                    };
                    if let Some(ann) = target {
                        self.router.timeline_mut().select_single_annotation(
                            ann.id.clone(),
                            ann.start_utc_ms,
                            ann.end_utc_ms,
                        );
                    }
                }
            }
            action::AppAction::FitDay => {
                if let Some((day_min, day_max)) = self.router.timeline().day_bounds_ms() {
                    self.router
                        .timeline_mut()
                        .snap_to_range_ms(day_min, day_max);
                }
            }
            action::AppAction::ToggleFoldGaps => {
                let next = !self.router.timeline().collapse_idle_gaps();
                self.router.timeline_mut().set_collapse_idle_gaps(next);
            }
        }
    }

    pub fn set_theme(&mut self, ctx: &eframe::egui::Context, selection: ui::theme::ThemeSelection) {
        self.theme_selection = selection;
        self.theme = self.theme_selection.get_style();
        ui::theme::store_theme(ctx, self.theme_selection.get_colors());
    }

    // Close the currently active widget in the tile tree
    pub fn close_active_widget(&mut self) {
        let active_tiles = self.tile_tree.active_tiles();

        for tile_id in active_tiles {
            if let Some(egui_tiles::Tile::Pane(_)) = self.tile_tree.tiles.get(tile_id) {
                self.tile_tree.tiles.remove(tile_id);
                break;
            }
        }
    }

    pub fn record_frame(&mut self, ctx: &eframe::egui::Context) {
        let now = std::time::Instant::now();
        self.frame_times.push_back(now);
        while self.frame_times.len() > 240
            || self
                .frame_times
                .front()
                .is_some_and(|&front| now.duration_since(front).as_secs_f32() > 0.5)
        {
            if self.frame_times.len() <= 2 {
                break;
            }
            self.frame_times.pop_front();
        }
        if self.frame_times.len() >= 2
            && let (Some(&first), Some(&last)) = (self.frame_times.front(), self.frame_times.back())
        {
            let span_s = last.duration_since(first).as_secs_f32();
            let intervals = (self.frame_times.len() - 1) as f32;
            if span_s > 1e-4 {
                self.fps = intervals / span_s;
                self.frame_time_ms = (span_s * 1000.0) / intervals;
                return;
            }
        }
        let dt = ctx.input(|i| i.predicted_dt).max(1e-4);
        self.fps = 1.0 / dt;
        self.frame_time_ms = dt * 1000.0;
    }
}

impl eframe::App for DAQApp {
    fn update(&mut self, ctx: &eframe::egui::Context, _: &mut eframe::Frame) {
        self.record_frame(ctx);
        while let Ok(event) = self.can_to_ui_rx.try_recv() {
            for tile in self.tile_tree.tiles.tiles_mut() {
                if let egui_tiles::Tile::Pane(widget) = tile {
                    widget.handle_operational_event(&event);
                }
            }

            match event {
                daqcore::can_thread::CanThreadEvent::DatabaseSelected { request_id, result } => {
                    if self.apply_database_result(request_id, result) {
                        self.save_settings();
                    }
                }
                daqcore::can_thread::CanThreadEvent::Frame(frame) => {
                    self.router.ingest_frame(frame);
                }
                daqcore::can_thread::CanThreadEvent::SourceSelected(Some(source)) => {
                    if self.active_source.as_ref() != Some(&source) {
                        self.router.reset_live(daqcore::time::Time::now());
                        self.bus_load_samples.clear();
                    }
                    self.active_source = Some(source);
                }
                daqcore::can_thread::CanThreadEvent::ConnectionFailed(error) => {
                    self.connection_status = ConnectionStatus::Error(error)
                }
                daqcore::can_thread::CanThreadEvent::ConnectionSuccessful => {
                    self.connection_status = ConnectionStatus::Connected
                }
                daqcore::can_thread::CanThreadEvent::Disconnection => {
                    self.connection_status = ConnectionStatus::Disconnected;
                }
                daqcore::can_thread::CanThreadEvent::BusLoad { timestamp, buses } => {
                    let sample = workspace_view::BusLoadSample { timestamp, buses };

                    let index = self
                        .bus_load_samples
                        .partition_point(|s| s.timestamp <= timestamp);
                    self.bus_load_samples.insert(index, sample);
                }
                daqcore::can_thread::CanThreadEvent::Hil(snapshot) => self.hil_snapshot = snapshot,
                daqcore::can_thread::CanThreadEvent::Diagnostic(error)
                | daqcore::can_thread::CanThreadEvent::SendFailed { error, .. } => {
                    log::error!("{error}");
                    self.diagnostic = Some(error);
                }
                _ => {}
            }
        }
        self.router.poll_background_tiles();
        if let Some(ppp) = self.pixels_per_point {
            ctx.set_pixels_per_point(ppp);
        }
        ctx.set_style(self.theme.clone());
        let bg_color = ui::theme::get_theme(ctx).panel_color();
        ctx.layer_painter(eframe::egui::LayerId::background())
            .rect_filled(ctx.content_rect(), 0.0, bg_color);

        // Handle keyboard shortcuts
        self.action_queue
            .extend(shortcuts::ShortcutHandler::check_shortcuts(ctx));

        // Command Palette UI and action generation
        self.action_queue.extend(self.command_palette.ui(ctx));

        // Drain the action queue and handle all actions
        for action in mem::take(&mut self.action_queue) {
            self.handle_action(action, ctx);
        }

        // Render the most recent state of the UI
        ui::sidebar::show(self, ctx);
        workspace::show(self, ctx);
        if self.workspace_view_modal.show(
            ctx,
            &mut self.tile_tree,
            &mut self.widget_ids,
            &mut self.router,
            self.ui_to_can_tx.clone(),
        ) {
            self.save_settings();
        }
        self.export_modal.show(ctx, &mut self.router);
        ctx.request_repaint();
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_settings();
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
    daqcore::formatter::Formatter::from_json(include_str!("../formatter_config.json"))
        .map_err(|e| log::error!("Invalid embedded formatter: {e}"))
        .ok()
}

impl Drop for DAQApp {
    fn drop(&mut self) {
        self.save_settings();
        if self.can_thread.stop().is_err() {
            log::error!("CAN worker panicked during shutdown");
        }
    }
}
