use daqcore::{
    export::{
        DatasetExportRequest, ExportError, ExportFormat, ExportSummary, SamplingMode,
        materialize_export_table, write_csv_bytes, write_matlab_mat_bytes,
    },
    router::VirtualTimelineRouter,
};
use daqviewer::{
    app::{DEFAULT_CLOUD_URL, ViewerPlatform, ViewerState},
    ui::theme::ThemeSelection,
    workspace_io::{WorkspaceTimeContext, WorkspaceView},
};
use std::{
    collections::BTreeMap,
    ops::RangeInclusive,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
};

pub const WEB_SETTINGS_STORAGE_KEY: &str = "per_daqweb_settings_v1";

/// Browser environment abstraction implemented by `WasmBrowserEnv` (`web_sys` in the browser)
/// and [`MemoryBrowserEnv`] (in headless/integration tests).
pub trait BrowserEnv: Send + Sync {
    /// Origin URL of the web page (e.g. `"http://127.0.0.1:3000"`).
    fn origin_url(&self) -> String;

    /// Current URL pathname (e.g. `"/"` or `"/daqview/daqview1:..."`).
    fn pathname(&self) -> String;

    /// Replace the browser address bar path without triggering a page reload (`history.replaceState`).
    fn replace_url_path(&self, new_path: &str);

    /// Read a string value from persistent browser storage (`localStorage`).
    fn load_storage(&self, key: &str) -> Option<String>;

    /// Write a string value to persistent browser storage (`localStorage`).
    fn save_storage(&self, key: &str, value: &str);

    /// Trigger a browser file download (`Blob` + `<a download="filename">`).
    fn trigger_file_download(&self, filename: &str, mime_type: &str, bytes: &[u8]);
}

/// Recorded file download triggered via [`MemoryBrowserEnv`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedDownload {
    pub filename: String,
    pub mime_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Default)]
struct MemoryBrowserState {
    origin: String,
    pathname: String,
    storage: BTreeMap<String, String>,
    downloads: Vec<RecordedDownload>,
}

/// In-memory [`BrowserEnv`] implementation for headless and E2E tests.
#[derive(Clone)]
pub struct MemoryBrowserEnv {
    inner: Arc<Mutex<MemoryBrowserState>>,
}

impl Default for MemoryBrowserEnv {
    fn default() -> Self {
        Self::new(DEFAULT_CLOUD_URL, "/")
    }
}

impl MemoryBrowserEnv {
    pub fn new(origin: impl Into<String>, pathname: impl Into<String>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(MemoryBrowserState {
                origin: origin.into(),
                pathname: pathname.into(),
                storage: BTreeMap::new(),
                downloads: Vec::new(),
            })),
        }
    }

    pub fn set_pathname(&self, pathname: impl Into<String>) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.pathname = pathname.into();
        }
    }

    pub fn downloads(&self) -> Vec<RecordedDownload> {
        self.inner
            .lock()
            .map(|g| g.downloads.clone())
            .unwrap_or_default()
    }
}

impl BrowserEnv for MemoryBrowserEnv {
    fn origin_url(&self) -> String {
        self.inner
            .lock()
            .map(|g| g.origin.clone())
            .unwrap_or_else(|_| DEFAULT_CLOUD_URL.to_owned())
    }

    fn pathname(&self) -> String {
        self.inner
            .lock()
            .map(|g| g.pathname.clone())
            .unwrap_or_else(|_| "/".to_owned())
    }

    fn replace_url_path(&self, new_path: &str) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.pathname = new_path.to_owned();
        }
    }

    fn load_storage(&self, key: &str) -> Option<String> {
        self.inner
            .lock()
            .ok()
            .and_then(|g| g.storage.get(key).cloned())
    }

    fn save_storage(&self, key: &str, value: &str) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.storage.insert(key.to_owned(), value.to_owned());
        }
    }

    fn trigger_file_download(&self, filename: &str, mime_type: &str, bytes: &[u8]) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.downloads.push(RecordedDownload {
                filename: filename.to_owned(),
                mime_type: mime_type.to_owned(),
                bytes: bytes.to_vec(),
            });
        }
    }
}

/// Persistent browser settings stored in `localStorage` under [`WEB_SETTINGS_STORAGE_KEY`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WebSettings {
    pub cloud_url: String,
    pub pixels_per_point: Option<f32>,
    pub theme: ThemeSelection,
    #[serde(default = "default_true")]
    pub collapse_idle_gaps: bool,
    #[serde(default = "default_daqview_filename")]
    pub daqview_file_path: String,
    #[serde(default)]
    pub active_workspace_tab: Option<String>,
    #[serde(default)]
    pub last_workspace_share: Option<String>,
}

fn default_true() -> bool {
    true
}

fn default_daqview_filename() -> String {
    "per_workspace.daqview.json".to_owned()
}

impl Default for WebSettings {
    fn default() -> Self {
        Self {
            cloud_url: DEFAULT_CLOUD_URL.to_owned(),
            pixels_per_point: None,
            theme: ThemeSelection::Default,
            collapse_idle_gaps: true,
            daqview_file_path: default_daqview_filename(),
            active_workspace_tab: Some("vd_handling_suspension".to_owned()),
            last_workspace_share: None,
        }
    }
}

struct PendingDownload {
    filename: String,
    mime_type: String,
    result: Result<Vec<u8>, String>,
}

/// `ViewerPlatform` implementation for the WebAssembly viewer (`daqweb`).
pub struct WebPlatform<B: BrowserEnv> {
    browser: B,
    download_tx: mpsc::Sender<PendingDownload>,
    download_rx: mpsc::Receiver<PendingDownload>,
    last_pushed_url_path: String,
    last_url_sync_instant: Option<web_time::Instant>,
    pending_initial_time_context: Option<WorkspaceTimeContext>,
    had_partitions_loaded: bool,
}

impl<B: BrowserEnv> WebPlatform<B> {
    pub fn new(browser: B) -> Self {
        let initial_path = browser.pathname();
        let (download_tx, download_rx) = mpsc::channel();
        Self {
            browser,
            download_tx,
            download_rx,
            last_pushed_url_path: initial_path,
            last_url_sync_instant: None,
            pending_initial_time_context: None,
            had_partitions_loaded: false,
        }
    }

    pub fn browser(&self) -> &B {
        &self.browser
    }

    pub fn set_pending_initial_time_context(&mut self, ctx: Option<WorkspaceTimeContext>) {
        self.pending_initial_time_context = ctx;
    }

    pub fn load_settings(&self) -> WebSettings {
        let origin = self.browser.origin_url();
        let mut settings = self
            .browser
            .load_storage(WEB_SETTINGS_STORAGE_KEY)
            .and_then(|raw| serde_json::from_str::<WebSettings>(&raw).ok())
            .unwrap_or_default();
        if !origin.trim().is_empty() && settings.cloud_url.trim().is_empty() {
            settings.cloud_url = origin;
        }
        settings
    }

    /// Synchronize the browser address bar (`/daqview/daqview1:...`) with the current workspace state.
    pub fn sync_browser_url(&mut self, state: &ViewerState) -> Option<String> {
        let view = WorkspaceView::capture_from_state(
            state,
            state
                .active_workspace_tab
                .as_deref()
                .unwrap_or("Shared Workspace"),
            "Captured from PER DaqWeb",
            true,
        )
        .ok()?;
        let share = view.to_share_string().ok()?;
        let new_path = format!("/daqview/{share}");
        if new_path != self.last_pushed_url_path {
            self.browser.replace_url_path(&new_path);
            self.last_pushed_url_path = new_path;
        }
        self.last_url_sync_instant = Some(web_time::Instant::now());
        Some(share)
    }
}

impl<B: BrowserEnv> ViewerPlatform for WebPlatform<B> {
    fn supports_hardware_widgets(&self) -> bool {
        false
    }

    fn supports_native_file_dialogs(&self) -> bool {
        false
    }

    fn share_base_url(&self, state: &ViewerState) -> String {
        let origin = self.browser.origin_url();
        let trimmed_origin = origin.trim().trim_end_matches('/');
        if !trimmed_origin.is_empty() {
            return trimmed_origin.to_owned();
        }
        let trimmed_cloud = state.cloud_url.trim().trim_end_matches('/');
        if !trimmed_cloud.is_empty() {
            trimmed_cloud.to_owned()
        } else {
            DEFAULT_CLOUD_URL.to_owned()
        }
    }

    fn poll_platform(&mut self, state: &mut ViewerState, ctx: &eframe::egui::Context) {
        // 1. Deliver any completed async Parquet dataset exports as browser file downloads.
        while let Ok(pending) = self.download_rx.try_recv() {
            match pending.result {
                Ok(bytes) => {
                    self.browser.trigger_file_download(
                        &pending.filename,
                        &pending.mime_type,
                        &bytes,
                    );
                    state.export_modal.status_banner = Some(Ok(format!(
                        "Downloaded {} ({:.1} KiB)",
                        pending.filename,
                        bytes.len() as f64 / 1024.0
                    )));
                }
                Err(err) => {
                    state.export_modal.status_banner = Some(Err(err));
                }
            }
        }

        // 2. If the page was opened via a `/daqview/daqview1:...` deep link before the cloud catalog
        // finished loading, re-apply its timeline viewport once partitions arrive so `reload_registry_state`
        // day-bounds initialization does not override the shared viewport window.
        let has_partitions_now = state.router.captured_time_span().is_some();
        if has_partitions_now && !self.had_partitions_loaded {
            self.had_partitions_loaded = true;
            if let Some(time_ctx) = self.pending_initial_time_context.take() {
                apply_time_context_to_router(&mut state.router, &time_ctx);
            }
        }

        // 3. Periodically keep the browser URL bar in sync with timeline pan/zoom changes
        // (debounced to at most once every 500ms when not actively dragging).
        let pointer_down = ctx.input(|i| i.pointer.any_down());
        let should_sync_url = !pointer_down
            && self
                .last_url_sync_instant
                .is_none_or(|t| t.elapsed().as_millis() >= 500);
        if should_sync_url {
            let _ = self.sync_browser_url(state);
        }
    }

    fn show_hardware_settings_ui(
        &mut self,
        ui: &mut eframe::egui::Ui,
        state: &mut ViewerState,
        muted_color: eframe::egui::Color32,
    ) {
        ui.label(
            eframe::egui::RichText::new("WEB TELEMETRY VIEWER (DAQWEB)")
                .size(10.0)
                .color(muted_color)
                .strong(),
        );
        ui.add_space(4.0);
        ui.label(
            eframe::egui::RichText::new(
                "Connected to daqcloud via WebAssembly. Copy the browser URL or click '🔗 Copy Share URL' to share this exact layout and timeline window.",
            )
            .size(11.0)
            .color(muted_color),
        );
        ui.add_space(4.0);
        if ui
            .small_button("🔗 Update Browser URL Now")
            .on_hover_text(
                "Push the current workspace layout and timeline window to the browser address bar",
            )
            .clicked()
        {
            let _ = self.sync_browser_url(state);
        }
        ui.add_space(8.0);
    }

    fn save_settings(&mut self, state: &ViewerState) {
        let share = WorkspaceView::capture_from_state(
            state,
            state
                .active_workspace_tab
                .as_deref()
                .unwrap_or("Shared Workspace"),
            "Captured from PER DaqWeb",
            true,
        )
        .ok()
        .and_then(|v| v.to_share_string().ok());

        let settings = WebSettings {
            cloud_url: state.cloud_url.clone(),
            pixels_per_point: state.pixels_per_point,
            theme: state.theme_selection,
            collapse_idle_gaps: state.router.timeline().collapse_idle_gaps(),
            daqview_file_path: extract_filename(&state.workspace_view_modal.save_file_path),
            active_workspace_tab: state.active_workspace_tab.clone(),
            last_workspace_share: share,
        };
        if let Ok(json) = serde_json::to_string(&settings) {
            self.browser.save_storage(WEB_SETTINGS_STORAGE_KEY, &json);
        }
    }

    fn on_workspace_changed(&mut self, state: &ViewerState) {
        let _ = self.sync_browser_url(state);
        self.save_settings(state);
    }

    fn save_daqview_content(
        &mut self,
        file_path_or_name: &str,
        content: &str,
    ) -> Result<String, String> {
        let filename = extract_filename(file_path_or_name);
        let filename = if filename.is_empty() {
            default_daqview_filename()
        } else {
            filename
        };
        self.browser
            .trigger_file_download(&filename, "application/json", content.as_bytes());
        Ok(format!("{filename} (browser download)"))
    }

    fn execute_dataset_export(
        &mut self,
        router: &mut VirtualTimelineRouter,
        range: RangeInclusive<i64>,
        signal_patterns: &[String],
        sampling_mode: SamplingMode,
        format: ExportFormat,
        output_path: &str,
    ) -> Result<ExportSummary, ExportError> {
        let filename = {
            let base = extract_filename(output_path);
            if base.is_empty() {
                format!("per_daq_export.{}", format.extension())
            } else {
                base
            }
        };

        // Materialize the table locally from `TileCache` first for CSV and MATLAB `.mat`
        // (or to validate range/signals for Parquet).
        match format {
            ExportFormat::Csv | ExportFormat::MatlabMat => {
                let table =
                    materialize_export_table(router, range, signal_patterns, sampling_mode)?;
                let (bytes, mime) = if format == ExportFormat::Csv {
                    (write_csv_bytes(&table)?, "text/csv")
                } else {
                    (write_matlab_mat_bytes(&table)?, "application/octet-stream")
                };
                self.browser.trigger_file_download(&filename, mime, &bytes);
                let start_utc_ms = table.timestamps_utc_ms.first().copied().unwrap_or(0);
                let end_utc_ms = table.timestamps_utc_ms.last().copied().unwrap_or(0);
                let signals = table
                    .columns
                    .iter()
                    .map(|c| (c.signal_id.clone(), c.unit.clone()))
                    .collect();
                Ok(ExportSummary {
                    output_path: PathBuf::from(filename),
                    format,
                    start_utc_ms,
                    end_utc_ms,
                    row_count: table.row_count(),
                    signal_count: table.column_count(),
                    signals,
                })
            }
            ExportFormat::Parquet => {
                let origin = self.browser.origin_url();
                let base_url = if !origin.trim().is_empty() {
                    origin
                } else {
                    DEFAULT_CLOUD_URL.to_owned()
                };
                let trimmed = base_url.trim().trim_end_matches('/');
                let export_url = format!("{trimmed}/api/v1/export");

                let custom_formulas = router
                    .formula_engine()
                    .definitions()
                    .iter()
                    .map(|c| c.def.clone())
                    .collect();
                let req_payload = DatasetExportRequest {
                    start_utc_ms: *range.start(),
                    end_utc_ms: *range.end(),
                    signal_patterns: signal_patterns.to_vec(),
                    sampling_mode,
                    format: ExportFormat::Parquet,
                    output_path: filename.clone(),
                    custom_formulas,
                };
                let body = serde_json::to_vec(&req_payload)
                    .map_err(|e| ExportError::Format(e.to_string()))?;
                let mut req = ehttp::Request::post(export_url, body);
                req.headers.insert("Content-Type", "application/json");
                let tx = self.download_tx.clone();
                let dl_filename = filename.clone();
                ehttp::fetch(req, move |resp_res| {
                    let result = match resp_res {
                        Ok(resp) if resp.ok => Ok(resp.bytes),
                        Ok(resp) => Err(format!(
                            "Parquet export failed (HTTP {}): {}",
                            resp.status,
                            String::from_utf8_lossy(&resp.bytes)
                        )),
                        Err(err) => Err(format!("Parquet export network error: {err}")),
                    };
                    let _ = tx.send(PendingDownload {
                        filename: dl_filename,
                        mime_type: "application/vnd.apache.parquet".to_owned(),
                        result,
                    });
                });

                Ok(ExportSummary {
                    output_path: PathBuf::from(filename),
                    format: ExportFormat::Parquet,
                    start_utc_ms: *range.start(),
                    end_utc_ms: *range.end(),
                    row_count: 0,
                    signal_count: signal_patterns.len(),
                    signals: signal_patterns
                        .iter()
                        .map(|s| (s.clone(), String::new()))
                        .collect(),
                })
            }
        }
    }
}

pub fn apply_time_context_to_router(
    router: &mut VirtualTimelineRouter,
    ctx: &WorkspaceTimeContext,
) {
    let tl = router.timeline_mut();
    tl.set_collapse_idle_gaps(ctx.gap_folding_enabled);
    if let Some(day_id) = &ctx.track_day_id {
        tl.set_active_track_day_id(Some(day_id.clone()));
    }
    if ctx.viewport_end_utc_ms >= ctx.viewport_start_utc_ms {
        tl.snap_to_range_ms(ctx.viewport_start_utc_ms, ctx.viewport_end_utc_ms);
    }
    if let Some(sp_ms) = ctx.setpoint_utc_ms {
        tl.set_setpoint(daqcore::time::Time::from_unix_millis(sp_ms));
    }
    tl.clear_selected_annotations();
    for (idx, ann_id) in ctx.compare_annotation_ids.iter().enumerate() {
        if idx == 0 {
            tl.select_single_annotation(
                ann_id.clone(),
                ctx.viewport_start_utc_ms,
                Some(ctx.viewport_end_utc_ms),
            );
            if let Some(sp_ms) = ctx.setpoint_utc_ms {
                tl.set_setpoint(daqcore::time::Time::from_unix_millis(sp_ms));
            }
        } else {
            tl.toggle_comparison_annotation(
                ann_id.clone(),
                ctx.viewport_start_utc_ms,
                Some(ctx.viewport_end_utc_ms),
            );
        }
    }
}

fn extract_filename(path_or_name: &str) -> String {
    Path::new(path_or_name.trim())
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path_or_name.trim().to_owned())
}
