use crate::{
    action, shortcuts, ui, widget_constructor, widget_ids, widgets, workspace, workspace_io,
    workspace_view,
};
use std::{collections::VecDeque, mem, ops::RangeInclusive, path};

pub use crate::widgets::PlatformWidget;
pub use daqcore::superdbc::DatabaseSelection;

pub const DEFAULT_CLOUD_URL: &str = "http://127.0.0.1:3000";
const UI_SCALE_STEP: f32 = 0.2;

#[derive(Debug, PartialEq, Clone)]
pub enum ConnectionStatus {
    Disconnected,
    Connected,
    Error(String),
}

/// Platform abstraction implemented by `DesktopPlatform` (`daqapp`) and `WebPlatform` (`daqweb`).
pub trait ViewerPlatform {
    fn supports_hardware_widgets(&self) -> bool {
        false
    }

    fn supports_native_file_dialogs(&self) -> bool {
        false
    }

    fn share_base_url(&self, state: &ViewerState) -> String {
        let trimmed = state.cloud_url.trim().trim_end_matches('/');
        if trimmed.is_empty() {
            DEFAULT_CLOUD_URL.to_owned()
        } else {
            trimmed.to_owned()
        }
    }

    fn create_platform_widget(
        &mut self,
        _ctor: &widget_constructor::WidgetConstructor,
        _id: usize,
    ) -> Option<Box<dyn PlatformWidget>> {
        None
    }

    fn instantiate_platform_pane(
        &mut self,
        _spec: &workspace_io::PaneSpec,
        _widget_ids: &mut widget_ids::WidgetIds,
    ) -> Option<Box<dyn PlatformWidget>> {
        None
    }

    fn on_widget_spawned(&mut self, _widget: &mut widgets::Widget) {}

    fn poll_platform(&mut self, _state: &mut ViewerState, _ctx: &eframe::egui::Context) {}

    fn show_hardware_settings_ui(
        &mut self,
        _ui: &mut eframe::egui::Ui,
        _state: &mut ViewerState,
        _muted_color: eframe::egui::Color32,
    ) {
    }

    fn reload_formatter(&mut self) -> Option<daqcore::formatter::Formatter> {
        load_embedded_formatter()
    }

    fn save_settings(&mut self, _state: &ViewerState) {}

    fn on_workspace_changed(&mut self, state: &ViewerState) {
        self.save_settings(state);
    }

    fn on_attach_store(&mut self, state: &mut ViewerState, store_dir: path::PathBuf) {
        match state.router.attach_store(&store_dir) {
            Ok(_) => {
                state.log_folder = Some(store_dir);
                self.save_settings(state);
                if state.connection_status != ConnectionStatus::Connected
                    && let Some((day_min, day_max)) = state.router.timeline().day_bounds_ms()
                {
                    state
                        .router
                        .timeline_mut()
                        .snap_to_range_ms(day_min, day_max);
                }
            }
            Err(err) => {
                state.diagnostic = Some(format!(
                    "Failed to attach store {}: {err}",
                    store_dir.display()
                ));
            }
        }
    }

    fn on_connect_cloud(&mut self, state: &mut ViewerState, url: String) {
        state.cloud_url = url.clone();
        if let Err(err) = state.router.connect_cloud(url) {
            state.diagnostic = Some(format!("Failed to connect to daqcloud: {err}"));
        } else {
            state.diagnostic = None;
            self.save_settings(state);
        }
    }

    fn pick_save_daqview_path(&mut self, _current_path: &str) -> Option<String> {
        None
    }

    fn pick_load_daqview_file(&mut self) -> Option<Result<(String, String), String>> {
        None
    }

    fn save_daqview_content(
        &mut self,
        file_path_or_name: &str,
        content: &str,
    ) -> Result<String, String> {
        let p = path::Path::new(file_path_or_name);
        if let Some(parent) = p.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(p, content).map_err(|e| e.to_string())?;
        Ok(file_path_or_name.to_owned())
    }

    fn pick_export_path(
        &mut self,
        _format: daqcore::export::ExportFormat,
        _current_path: &str,
    ) -> Option<String> {
        None
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
        let table = daqcore::export::materialize_export_table(
            router,
            range,
            signal_patterns,
            sampling_mode,
        )?;
        let out_path = path::PathBuf::from(output_path);
        if let Some(parent) = out_path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        match format {
            daqcore::export::ExportFormat::Csv => daqcore::export::write_csv(&table, &out_path)?,
            daqcore::export::ExportFormat::MatlabMat => {
                daqcore::export::write_matlab_mat(&table, &out_path)?
            }
            daqcore::export::ExportFormat::Parquet => {
                return Err(daqcore::export::ExportError::Format(
                    "Parquet file writing requires native platform backend or daqcloud /api/v1/export".to_owned(),
                ));
            }
        }
        let start_utc_ms = table.timestamps_utc_ms.first().copied().unwrap_or(0);
        let end_utc_ms = table.timestamps_utc_ms.last().copied().unwrap_or(0);
        let signals = table
            .columns
            .iter()
            .map(|c| (c.signal_id.clone(), c.unit.clone()))
            .collect();
        Ok(daqcore::export::ExportSummary {
            output_path: out_path,
            format,
            start_utc_ms,
            end_utc_ms,
            row_count: table.row_count(),
            signal_count: table.column_count(),
            signals,
        })
    }
}

/// Default no-op [`ViewerPlatform`] implementation used in headless unit tests.
#[derive(Default)]
pub struct NoopViewerPlatform;

impl ViewerPlatform for NoopViewerPlatform {}

/// Shared application state for both `daqapp` (Desktop) and `daqweb` (WebAssembly).
pub struct ViewerState {
    pub connection_status: ConnectionStatus,
    pub value_formatter: Option<daqcore::formatter::Formatter>,
    pub is_sidebar_open: bool,
    pub sidebar_tab: ui::sidebar::SidebarTab,
    pub active_workspace_tab: Option<String>,
    pub show_share_popover: bool,
    pub command_palette: ui::command_palette::CommandPalette,
    pub tile_tree: egui_tiles::Tree<widgets::Widget>,
    pub widget_ids: widget_ids::WidgetIds,
    pub action_queue: Vec<action::AppAction>,
    pub theme: eframe::egui::Style,
    pub theme_selection: ui::theme::ThemeSelection,
    pub pixels_per_point: Option<f32>,
    pub database: Option<DatabaseSelection>,
    pub selection_pending: bool,
    pub router: daqcore::router::VirtualTimelineRouter,
    pub bus_load_samples: Vec<workspace_view::BusLoadSample>,
    pub hover_utc_ms: Option<i64>,
    pub hover_distance_m: Option<f64>,
    pub diagnostic: Option<String>,
    pub log_folder: Option<path::PathBuf>,
    pub cloud_url: String,
    pub cloud_auto_connect: bool,
    pub timeline_ui: ui::timeline::TimelineUiState,
    pub signal_browser: ui::signal_browser::SignalBrowserState,
    pub workspace_presets: workspace_io::WorkspacePresetRegistry,
    pub workspace_view_modal: workspace_io::WorkspaceViewModalState,
    pub export_modal: ui::export_modal::ExportModalState,
    frame_times: VecDeque<web_time::Instant>,
    pub fps: f32,
    pub frame_time_ms: f32,
}

impl ViewerState {
    pub fn new(
        router: daqcore::router::VirtualTimelineRouter,
        theme_selection: ui::theme::ThemeSelection,
        pixels_per_point: Option<f32>,
        cloud_url: String,
        cloud_auto_connect: bool,
        log_folder: Option<path::PathBuf>,
        value_formatter: Option<daqcore::formatter::Formatter>,
    ) -> Self {
        let theme = theme_selection.get_style();
        Self {
            connection_status: ConnectionStatus::Disconnected,
            value_formatter,
            is_sidebar_open: true,
            sidebar_tab: ui::sidebar::SidebarTab::default(),
            active_workspace_tab: None,
            show_share_popover: false,
            command_palette: ui::command_palette::CommandPalette::new(),
            tile_tree: egui_tiles::Tree::empty("workspace_tree"),
            widget_ids: widget_ids::WidgetIds::new(),
            action_queue: Vec::new(),
            theme,
            theme_selection,
            pixels_per_point,
            database: None,
            selection_pending: false,
            router,
            bus_load_samples: Vec::new(),
            hover_utc_ms: None,
            hover_distance_m: None,
            diagnostic: None,
            log_folder,
            cloud_url,
            cloud_auto_connect,
            timeline_ui: ui::timeline::TimelineUiState::default(),
            signal_browser: ui::signal_browser::SignalBrowserState::default(),
            workspace_presets: workspace_io::WorkspacePresetRegistry::with_builtin(),
            workspace_view_modal: workspace_io::WorkspaceViewModalState::default(),
            export_modal: ui::export_modal::ExportModalState::default(),
            frame_times: VecDeque::with_capacity(256),
            fps: 60.0,
            frame_time_ms: 16.6,
        }
    }

    pub fn selection_pending(&self) -> bool {
        self.selection_pending
    }

    pub fn add_widget_to_tree(&mut self, widget: widgets::Widget) {
        let new_tile_id = self.tile_tree.tiles.insert_pane(widget);

        let Some(root_id) = self.tile_tree.root else {
            self.tile_tree.root = Some(new_tile_id);
            return;
        };

        let Some(egui_tiles::Tile::Container(egui_tiles::Container::Tabs(tabs))) =
            self.tile_tree.tiles.get_mut(root_id)
        else {
            let tab_container = self
                .tile_tree
                .tiles
                .insert_tab_tile(vec![root_id, new_tile_id]);
            self.tile_tree.root = Some(tab_container);
            return;
        };

        tabs.add_child(new_tile_id);
        tabs.set_active(new_tile_id);
    }

    pub fn handle_action(
        &mut self,
        action: action::AppAction,
        platform: &mut dyn ViewerPlatform,
        ctx: &eframe::egui::Context,
    ) {
        match action {
            action::AppAction::SpawnWidget(widget_type) => {
                let kind = widget_type.kind();
                let existing_count = self
                    .tile_tree
                    .tiles
                    .tiles()
                    .filter(|tile| matches!(tile, egui_tiles::Tile::Pane(w) if w.kind() == kind))
                    .count();
                let created =
                    widget_type.create_all(&mut self.widget_ids, platform, existing_count);
                if created.is_empty() {
                    log::warn!("Maximum number of {:?} widgets already open", kind);
                } else {
                    for mut widget in created {
                        platform.on_widget_spawned(&mut widget);
                        self.add_widget_to_tree(widget);
                    }
                    platform.on_workspace_changed(self);
                }
            }
            action::AppAction::AttachStore(store_dir) => {
                platform.on_attach_store(self, store_dir);
            }
            action::AppAction::ConnectCloud(url) => {
                platform.on_connect_cloud(self, url);
            }
            action::AppAction::DisconnectCloud => {
                self.router.disconnect_cloud();
                platform.save_settings(self);
            }
            action::AppAction::SyncCloudNow => {
                self.router.trigger_cloud_sync();
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
                        platform,
                        apply_time_context,
                    ) {
                        self.diagnostic = Some(format!(
                            "Failed to apply workspace preset '{preset_id}': {err}"
                        ));
                    } else {
                        self.active_workspace_tab = Some(preset_id);
                        platform.on_workspace_changed(self);
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
            action::AppAction::CopyWorkspaceShareUrl => {
                let base_url = platform.share_base_url(self);
                if let Err(err) = self.workspace_view_modal.generate_and_copy_url(
                    ctx,
                    &self.tile_tree,
                    &self.router,
                    &base_url,
                ) {
                    self.diagnostic =
                        Some(format!("Failed to generate workspace share URL: {err}"));
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
                platform.on_workspace_changed(self);
            }
            action::AppAction::ClearWorkspace => {
                self.tile_tree = egui_tiles::Tree::empty("workspace_tree");
                self.widget_ids = widget_ids::WidgetIds::new();
                self.active_workspace_tab = None;
                platform.on_workspace_changed(self);
            }
            action::AppAction::RenamePane { tile_id, title } => {
                if let Some(egui_tiles::Tile::Pane(widget)) = self.tile_tree.tiles.get_mut(tile_id)
                {
                    widget.set_title(title);
                    platform.on_workspace_changed(self);
                }
            }
            action::AppAction::ResetPaneTitle { tile_id } => {
                if let Some(egui_tiles::Tile::Pane(widget)) = self.tile_tree.tiles.get_mut(tile_id)
                {
                    widget.reset_auto_title();
                    platform.on_workspace_changed(self);
                }
            }
            action::AppAction::ClosePane { tile_id } => {
                self.tile_tree.tiles.remove(tile_id);
                platform.on_workspace_changed(self);
            }
            action::AppAction::IncreaseScale => {
                let current_scale = self
                    .pixels_per_point
                    .unwrap_or_else(|| ctx.pixels_per_point());
                self.pixels_per_point = Some((current_scale + UI_SCALE_STEP).clamp(0.5, 3.0));
                platform.save_settings(self);
            }
            action::AppAction::DecreaseScale => {
                let current_scale = self
                    .pixels_per_point
                    .unwrap_or_else(|| ctx.pixels_per_point());
                self.pixels_per_point = Some((current_scale - UI_SCALE_STEP).clamp(0.5, 3.0));
                platform.save_settings(self);
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
                platform.on_workspace_changed(self);
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
        let now = web_time::Instant::now();
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

    pub fn update_frame(&mut self, platform: &mut dyn ViewerPlatform, ctx: &eframe::egui::Context) {
        self.record_frame(ctx);
        self.router.poll_background_tiles();
        self.router.poll_cloud_sync();
        platform.poll_platform(self, ctx);
        if let Some(ppp) = self.pixels_per_point {
            ctx.set_pixels_per_point(ppp.clamp(0.5, 3.0));
        }
        ctx.set_style(self.theme.clone());
        let bg_color = ui::theme::get_theme(ctx).panel_color();
        ctx.layer_painter(eframe::egui::LayerId::background())
            .rect_filled(ctx.content_rect(), 0.0, bg_color);

        // Handle keyboard shortcuts
        self.action_queue
            .extend(shortcuts::ShortcutHandler::check_shortcuts(ctx));

        // Command Palette UI and action generation
        self.action_queue.extend(
            self.command_palette
                .ui_for(ctx, platform.supports_hardware_widgets()),
        );

        // Drain the action queue and handle all actions
        for action in mem::take(&mut self.action_queue) {
            self.handle_action(action, platform, ctx);
        }

        // Render the most recent state of the UI
        ui::sidebar::show(self, platform, ctx);
        workspace::show(self, platform, ctx);
        let base_url = platform.share_base_url(self);
        if self.workspace_view_modal.show(
            ctx,
            &mut self.tile_tree,
            &mut self.widget_ids,
            &mut self.router,
            platform,
            &base_url,
        ) {
            platform.on_workspace_changed(self);
        }
        self.export_modal.show(ctx, &mut self.router, platform);
        for action in mem::take(&mut self.action_queue) {
            self.handle_action(action, platform, ctx);
        }
        ctx.request_repaint();
    }
}

/// Top-level `eframe::App` combining [`ViewerState`] and a platform implementation `P: ViewerPlatform`.
pub struct ViewerApp<P: ViewerPlatform> {
    pub state: ViewerState,
    pub platform: P,
}

impl<P: ViewerPlatform> ViewerApp<P> {
    pub fn new(state: ViewerState, platform: P) -> Self {
        Self { state, platform }
    }
}

impl<P: ViewerPlatform> eframe::App for ViewerApp<P> {
    fn update(&mut self, ctx: &eframe::egui::Context, _frame: &mut eframe::Frame) {
        self.state.update_frame(&mut self.platform, ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.platform.save_settings(&self.state);
    }
}

pub fn load_embedded_formatter() -> Option<daqcore::formatter::Formatter> {
    daqcore::formatter::Formatter::from_json(include_str!("../formatter_config.json"))
        .map_err(|e| log::error!("Invalid embedded formatter: {e}"))
        .ok()
}
