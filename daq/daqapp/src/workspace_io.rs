use crate::{app, ui, widget_constructor, widget_ids, widgets};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use daqcore::{analysis::DerivedSignalDef, can_thread::CanThreadCommand, time::Time};
use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};
use indexmap::IndexMap;
use std::{
    fmt, fs,
    io::{Read, Write},
    path::Path,
    sync::mpsc,
};

/// Embedded JSON configuration from `daq/config/workspace_presets.json`.
pub const BUILTIN_WORKSPACE_PRESETS_JSON: &str =
    include_str!("../../config/workspace_presets.json");

/// Current schema version for `.daqview.json` files.
pub const DAQVIEW_SCHEMA_VERSION: u32 = 1;

/// Prefix for compressed Slack/Discord share strings (`daqview1:<zlib+base64url>`).
pub const SHARE_STRING_PREFIX: &str = "daqview1:";

#[derive(Debug)]
pub enum WorkspaceIoError {
    Io(std::io::Error),
    Json(serde_json::Error),
    ShareStringDecode(String),
    FormulaError(String),
    EmptyLayout,
}

impl fmt::Display for WorkspaceIoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error reading/writing workspace view: {e}"),
            Self::Json(e) => write!(f, "JSON error in .daqview.json: {e}"),
            Self::ShareStringDecode(msg) => write!(f, "invalid share string: {msg}"),
            Self::FormulaError(msg) => write!(f, "custom formula error in workspace view: {msg}"),
            Self::EmptyLayout => write!(f, "workspace view does not define any layout panes"),
        }
    }
}

impl std::error::Error for WorkspaceIoError {}

impl From<std::io::Error> for WorkspaceIoError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for WorkspaceIoError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

/// Optional timeline context saved inside a `.daqview.json` file so teammates can jump
/// directly to the exact `[viewport_start_utc_ms, viewport_end_utc_ms]` window and playhead.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceTimeContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_day_id: Option<String>,
    pub viewport_start_utc_ms: i64,
    pub viewport_end_utc_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setpoint_utc_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compare_annotation_ids: Vec<String>,
    #[serde(default = "default_true")]
    pub gap_folding_enabled: bool,
}

fn default_true() -> bool {
    true
}

fn default_schema_version() -> u32 {
    DAQVIEW_SCHEMA_VERSION
}

fn default_speed_signal() -> String {
    "gps_speed_heading.gps_speed".to_owned()
}

fn default_gps_max_dist() -> f64 {
    500.0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinearSplitDir {
    Horizontal,
    Vertical,
}

/// Serialized channel entry inside a `Scope` pane.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ScopeChannelSpec {
    pub signal_id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub unit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_rgb: Option<[u8; 3]>,
}

/// Shorthand axis grouping (`axes: [{ unit, signals: [...] }]`) supported by the Master Plan v2 Section 4.3 schema.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ScopeAxisSpec {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub unit: String,
    #[serde(default)]
    pub position: String,
    #[serde(default)]
    pub signals: Vec<String>,
}

/// Serialized lane entry inside a `StateLane` pane.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StateLaneSpec {
    pub signal_id: String,
    #[serde(default)]
    pub label: String,
}

/// Configuration for a single workspace pane widget.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "widget", rename_all = "snake_case")]
pub enum PaneSpec {
    Scope {
        #[serde(default)]
        title: String,
        #[serde(default = "default_true")]
        custom_title: bool,
        #[serde(default)]
        x_mode: ui::scope::ScopeXAxisMode,
        #[serde(default = "default_speed_signal")]
        speed_signal_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        y_bounds: Option<[f64; 2]>,
        #[serde(default)]
        channels: Vec<ScopeChannelSpec>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        axes: Vec<ScopeAxisSpec>,
    },
    #[serde(alias = "state_lanes")]
    StateLane {
        #[serde(default)]
        title: String,
        #[serde(default = "default_true")]
        custom_title: bool,
        #[serde(default)]
        lanes: Vec<StateLaneSpec>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        signals: Vec<String>,
    },
    #[serde(alias = "xy_scatter")]
    XYScatter {
        #[serde(default)]
        title: String,
        #[serde(default = "default_true")]
        custom_title: bool,
        x_signal: String,
        y_signal: String,
        #[serde(
            default,
            alias = "color_signal",
            skip_serializing_if = "Option::is_none"
        )]
        z_color_signal: Option<String>,
        #[serde(default)]
        equal_aspect: bool,
        #[serde(default)]
        show_g_rings: bool,
    },
    #[serde(alias = "gps_map")]
    GpsPlot {
        #[serde(default)]
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color_signal: Option<String>,
        #[serde(default)]
        color_unit: String,
        #[serde(default = "default_gps_max_dist")]
        max_distance_meters: f64,
    },
    ViewerTable {
        #[serde(default)]
        title: String,
    },
    ViewerList {
        #[serde(default)]
        title: String,
    },
    BusLoad {
        #[serde(default)]
        title: String,
    },
    Jitter {
        #[serde(default)]
        title: String,
    },
    LogParser {
        #[serde(default)]
        title: String,
    },
    SendUi {
        #[serde(default)]
        title: String,
    },
    Bootloader {
        #[serde(default)]
        title: String,
    },
    Hil {
        #[serde(default)]
        title: String,
    },
}

/// Recursive `egui_tiles` layout tree representation in `.daqview.json`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkspaceLayoutNode {
    Pane {
        #[serde(flatten)]
        spec: PaneSpec,
    },
    Linear {
        dir: LinearSplitDir,
        children: Vec<WorkspaceLayoutNode>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        shares: Vec<f32>,
    },
    Tabs {
        children: Vec<WorkspaceLayoutNode>,
        #[serde(default)]
        active: usize,
    },
    Grid {
        children: Vec<WorkspaceLayoutNode>,
    },
}

/// Shorthand `layout.root_split` node from Master Plan v2 Section 4.3.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ShorthandSplitSpec {
    pub dir: LinearSplitDir,
    #[serde(default)]
    pub shares: Vec<f32>,
    #[serde(default)]
    pub children: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ShorthandLayout {
    pub root_split: ShorthandSplitSpec,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum FlexibleLayout {
    Tree(WorkspaceLayoutNode),
    Shorthand(ShorthandLayout),
}

/// Serializable `.daqview.json` workspace view document.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<WorkspaceTimeContext>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub custom_formulas: Vec<DerivedSignalDef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<FlexibleLayout>,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub panes: IndexMap<String, PaneSpec>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkspacePresetsFile {
    pub presets: Vec<WorkspaceView>,
}

/// Registry of built-in and repo-bundled `.daqview.json` workspace presets.
#[derive(Debug, Clone)]
pub struct WorkspacePresetRegistry {
    presets: Vec<WorkspaceView>,
}

impl Default for WorkspacePresetRegistry {
    fn default() -> Self {
        Self::with_builtin()
    }
}

impl WorkspacePresetRegistry {
    pub fn with_builtin() -> Self {
        let parsed: WorkspacePresetsFile = serde_json::from_str(BUILTIN_WORKSPACE_PRESETS_JSON)
            .expect("embedded daq/config/workspace_presets.json must be valid JSON");
        Self {
            presets: parsed.presets,
        }
    }

    pub fn presets(&self) -> &[WorkspaceView] {
        &self.presets
    }

    pub fn get(&self, id_or_name: &str) -> Option<&WorkspaceView> {
        let q = id_or_name.trim();
        self.presets.iter().find(|p| {
            p.id.as_deref().is_some_and(|id| id.eq_ignore_ascii_case(q))
                || p.name.eq_ignore_ascii_case(q)
        })
    }
}

impl WorkspaceView {
    /// Resolve either a recursive `layout` tree or the Master Plan v2 `root_split` + `panes` shorthand
    /// into a normalized [`WorkspaceLayoutNode`].
    pub fn resolved_layout(&self) -> Result<WorkspaceLayoutNode, WorkspaceIoError> {
        match &self.layout {
            Some(FlexibleLayout::Tree(node)) => Ok(node.clone()),
            Some(FlexibleLayout::Shorthand(short)) => {
                let mut children = Vec::with_capacity(short.root_split.children.len());
                for child_key in &short.root_split.children {
                    if let Some(pane_spec) = self.panes.get(child_key) {
                        children.push(WorkspaceLayoutNode::Pane {
                            spec: pane_spec.clone(),
                        });
                    }
                }
                if children.is_empty() {
                    return Err(WorkspaceIoError::EmptyLayout);
                }
                Ok(WorkspaceLayoutNode::Linear {
                    dir: short.root_split.dir,
                    children,
                    shares: short.root_split.shares.clone(),
                })
            }
            None => {
                if self.panes.is_empty() {
                    return Err(WorkspaceIoError::EmptyLayout);
                }
                let children: Vec<WorkspaceLayoutNode> = self
                    .panes
                    .values()
                    .cloned()
                    .map(|spec| WorkspaceLayoutNode::Pane { spec })
                    .collect();
                if children.len() == 1 {
                    return Ok(children
                        .into_iter()
                        .next()
                        .unwrap_or(WorkspaceLayoutNode::Grid {
                            children: Vec::new(),
                        }));
                }
                Ok(WorkspaceLayoutNode::Linear {
                    dir: LinearSplitDir::Vertical,
                    children,
                    shares: Vec::new(),
                })
            }
        }
    }

    /// Serialize to pretty-printed `.daqview.json` text.
    pub fn to_json_pretty(&self) -> Result<String, WorkspaceIoError> {
        Ok(serde_json::to_string_pretty(self)? + "\n")
    }

    /// Write to a `.daqview.json` file on disk.
    pub fn save_to_file(&self, path: impl AsRef<Path>) -> Result<(), WorkspaceIoError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }
        let text = self.to_json_pretty()?;
        fs::write(path, text)?;
        Ok(())
    }

    /// Load a `.daqview.json` file from disk.
    pub fn load_from_file(path: impl AsRef<Path>) -> Result<Self, WorkspaceIoError> {
        let text = fs::read_to_string(path.as_ref())?;
        Self::from_str_flexible(&text)
    }

    /// Encode this view into a compact, URL-safe Slack/Discord share string (`daqview1:<zlib+base64url>`).
    pub fn to_share_string(&self) -> Result<String, WorkspaceIoError> {
        let compact_json = serde_json::to_vec(self)?;
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(&compact_json)?;
        let compressed = encoder.finish()?;
        let encoded = URL_SAFE_NO_PAD.encode(&compressed);
        Ok(format!("{SHARE_STRING_PREFIX}{encoded}"))
    }

    /// Parse a [`WorkspaceView`] from a `daqview1:...` share string, raw JSON, or base64-encoded JSON.
    pub fn from_str_flexible(input: &str) -> Result<Self, WorkspaceIoError> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(WorkspaceIoError::EmptyLayout);
        }

        if let Some(payload) = trimmed.strip_prefix(SHARE_STRING_PREFIX) {
            let compressed = URL_SAFE_NO_PAD
                .decode(payload.trim())
                .map_err(|e| WorkspaceIoError::ShareStringDecode(e.to_string()))?;
            let mut decoder = ZlibDecoder::new(compressed.as_slice());
            let mut json_bytes = Vec::new();
            decoder
                .read_to_end(&mut json_bytes)
                .map_err(|e| WorkspaceIoError::ShareStringDecode(e.to_string()))?;
            let view: Self = serde_json::from_slice(&json_bytes)?;
            let _ = view.resolved_layout()?;
            return Ok(view);
        }

        if trimmed.starts_with('{') {
            let view: Self = serde_json::from_str(trimmed)?;
            let _ = view.resolved_layout()?;
            return Ok(view);
        }

        // Try base64url or standard base64 decoding (either raw JSON or zlib-compressed JSON)
        let bytes = URL_SAFE_NO_PAD
            .decode(trimmed)
            .or_else(|_| base64::engine::general_purpose::STANDARD.decode(trimmed))
            .map_err(|e| WorkspaceIoError::ShareStringDecode(e.to_string()))?;

        if bytes.starts_with(b"{") {
            let view: Self = serde_json::from_slice(&bytes)?;
            let _ = view.resolved_layout()?;
            return Ok(view);
        }

        let mut decoder = ZlibDecoder::new(bytes.as_slice());
        let mut decompressed = Vec::new();
        decoder
            .read_to_end(&mut decompressed)
            .map_err(|e| WorkspaceIoError::ShareStringDecode(e.to_string()))?;
        let view: Self = serde_json::from_slice(&decompressed)?;
        let _ = view.resolved_layout()?;
        Ok(view)
    }

    /// Capture the current `DAQApp` workspace layout, custom formulas, and timeline state into a [`WorkspaceView`].
    #[allow(dead_code)]
    pub fn capture_from_app(
        app: &app::DAQApp,
        name: impl Into<String>,
        description: impl Into<String>,
        include_time_context: bool,
    ) -> Result<Self, WorkspaceIoError> {
        Self::capture_from_workspace(
            &app.tile_tree,
            &app.router,
            name,
            description,
            include_time_context,
        )
    }

    /// Capture a tile tree, custom formulas from `router`, and optional timeline state into a [`WorkspaceView`].
    pub fn capture_from_workspace(
        tile_tree: &egui_tiles::Tree<widgets::Widget>,
        router: &daqcore::router::VirtualTimelineRouter,
        name: impl Into<String>,
        description: impl Into<String>,
        include_time_context: bool,
    ) -> Result<Self, WorkspaceIoError> {
        let root_id = tile_tree.root.ok_or(WorkspaceIoError::EmptyLayout)?;
        let layout_node =
            capture_tile_node(&tile_tree.tiles, root_id).ok_or(WorkspaceIoError::EmptyLayout)?;

        let builtin_engine = daqcore::analysis::FormulaEngine::with_builtin_config();
        let custom_formulas: Vec<DerivedSignalDef> = router
            .formula_engine()
            .definitions()
            .iter()
            .filter(|compiled| {
                builtin_engine
                    .get(&compiled.def.id)
                    .is_none_or(|b| b.def != compiled.def)
            })
            .map(|c| c.def.clone())
            .collect();

        let context = if include_time_context {
            let tl = router.timeline();
            Some(WorkspaceTimeContext {
                track_day_id: tl.active_track_day_id().map(str::to_owned),
                viewport_start_utc_ms: tl.start_utc_ms(),
                viewport_end_utc_ms: tl.end_utc_ms(),
                setpoint_utc_ms: Some(tl.setpoint_utc_ms()),
                compare_annotation_ids: tl.selected_annotation_ids().to_vec(),
                gap_folding_enabled: tl.collapse_idle_gaps(),
            })
        } else {
            None
        };

        Ok(Self {
            id: None,
            schema_version: DAQVIEW_SCHEMA_VERSION,
            name: name.into(),
            description: description.into(),
            context,
            custom_formulas,
            layout: Some(FlexibleLayout::Tree(layout_node)),
            panes: IndexMap::new(),
        })
    }

    /// Apply this [`WorkspaceView`] to `app`, rebuilding `app.tile_tree`, merging `custom_formulas`,
    /// and optionally jumping the timeline to `self.context` when `apply_time_context` is true.
    #[allow(dead_code)]
    pub fn apply_to_app(
        &self,
        app: &mut app::DAQApp,
        apply_time_context: bool,
    ) -> Result<(), WorkspaceIoError> {
        self.apply_to_workspace(
            &mut app.tile_tree,
            &mut app.widget_ids,
            &mut app.router,
            app.ui_to_can_tx.clone(),
            apply_time_context,
        )
    }

    /// Apply this [`WorkspaceView`] to a workspace's `tile_tree`, `widget_ids`, and `router`.
    pub fn apply_to_workspace(
        &self,
        tile_tree: &mut egui_tiles::Tree<widgets::Widget>,
        widget_ids: &mut widget_ids::WidgetIds,
        router: &mut daqcore::router::VirtualTimelineRouter,
        ui_to_can_tx: mpsc::Sender<CanThreadCommand>,
        apply_time_context: bool,
    ) -> Result<(), WorkspaceIoError> {
        let root_node = self.resolved_layout()?;

        if !self.custom_formulas.is_empty() {
            router
                .formula_engine_mut()
                .merge_defs(&self.custom_formulas)
                .map_err(|e| WorkspaceIoError::FormulaError(e.to_string()))?;
        }

        let mut new_widget_ids = widget_ids::WidgetIds::new();
        let mut tiles = egui_tiles::Tiles::default();
        let root_id = build_tile_node(&root_node, &mut tiles, &mut new_widget_ids, ui_to_can_tx)
            .ok_or(WorkspaceIoError::EmptyLayout)?;

        *tile_tree = egui_tiles::Tree::new("workspace_tree", root_id, tiles);
        *widget_ids = new_widget_ids;

        if apply_time_context && let Some(ctx) = &self.context {
            let tl = router.timeline_mut();
            tl.set_collapse_idle_gaps(ctx.gap_folding_enabled);
            if let Some(day_id) = &ctx.track_day_id {
                tl.set_active_track_day_id(Some(day_id.clone()));
            }
            if ctx.viewport_end_utc_ms >= ctx.viewport_start_utc_ms {
                tl.snap_to_range_ms(ctx.viewport_start_utc_ms, ctx.viewport_end_utc_ms);
            }
            if let Some(sp_ms) = ctx.setpoint_utc_ms {
                tl.set_setpoint(Time::from_unix_millis(sp_ms));
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
                        tl.set_setpoint(Time::from_unix_millis(sp_ms));
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

        Ok(())
    }
}

fn capture_tile_node(
    tiles: &egui_tiles::Tiles<widgets::Widget>,
    tile_id: egui_tiles::TileId,
) -> Option<WorkspaceLayoutNode> {
    let tile = tiles.get(tile_id)?;
    match tile {
        egui_tiles::Tile::Pane(widget) => Some(WorkspaceLayoutNode::Pane {
            spec: capture_widget_spec(widget),
        }),
        egui_tiles::Tile::Container(container) => match container {
            egui_tiles::Container::Linear(linear) => {
                let mut children = Vec::new();
                let mut shares = Vec::new();
                for &child_id in &linear.children {
                    if let Some(node) = capture_tile_node(tiles, child_id) {
                        children.push(node);
                        shares.push(linear.shares[child_id]);
                    }
                }
                if children.is_empty() {
                    return None;
                }
                let dir = match linear.dir {
                    egui_tiles::LinearDir::Horizontal => LinearSplitDir::Horizontal,
                    egui_tiles::LinearDir::Vertical => LinearSplitDir::Vertical,
                };
                Some(WorkspaceLayoutNode::Linear {
                    dir,
                    children,
                    shares,
                })
            }
            egui_tiles::Container::Tabs(tabs) => {
                let mut children = Vec::new();
                let mut active_idx = 0usize;
                for &child_id in &tabs.children {
                    if let Some(node) = capture_tile_node(tiles, child_id) {
                        if tabs.active == Some(child_id) {
                            active_idx = children.len();
                        }
                        children.push(node);
                    }
                }
                if children.is_empty() {
                    return None;
                }
                if children.len() == 1 {
                    return children.into_iter().next();
                }
                Some(WorkspaceLayoutNode::Tabs {
                    children,
                    active: active_idx,
                })
            }
            egui_tiles::Container::Grid(grid) => {
                let mut children = Vec::new();
                for child_id in grid.children() {
                    if let Some(node) = capture_tile_node(tiles, *child_id) {
                        children.push(node);
                    }
                }
                if children.is_empty() {
                    return None;
                }
                Some(WorkspaceLayoutNode::Grid { children })
            }
        },
    }
}

fn capture_widget_spec(widget: &widgets::Widget) -> PaneSpec {
    match widget {
        widgets::Widget::Scope(scope) => {
            let channels = scope
                .channels()
                .iter()
                .map(|ch| {
                    let [r, g, b, _] = ch.color.to_srgba_unmultiplied();
                    ScopeChannelSpec {
                        signal_id: ch.signal_id.clone(),
                        label: ch.label.clone(),
                        unit: ch.unit.clone(),
                        color_rgb: Some([r, g, b]),
                    }
                })
                .collect();
            let y_bounds = scope.y_bounds_override().map(|(lo, hi)| [lo, hi]);
            PaneSpec::Scope {
                title: scope.title.clone(),
                custom_title: scope.is_custom_title(),
                x_mode: scope.x_axis_mode,
                speed_signal_id: scope.speed_signal_id.clone(),
                y_bounds,
                channels,
                axes: Vec::new(),
            }
        }
        widgets::Widget::StateLane(lane) => {
            let lanes = lane
                .lanes()
                .iter()
                .map(|l| StateLaneSpec {
                    signal_id: l.signal_id.clone(),
                    label: l.label.clone(),
                })
                .collect();
            PaneSpec::StateLane {
                title: lane.title.clone(),
                custom_title: lane.is_custom_title(),
                lanes,
                signals: Vec::new(),
            }
        }
        widgets::Widget::XYScatter(scatter) => PaneSpec::XYScatter {
            title: scatter.title.clone(),
            custom_title: scatter.is_custom_title(),
            x_signal: scatter.x_signal.clone(),
            y_signal: scatter.y_signal.clone(),
            z_color_signal: scatter.z_color_signal.clone(),
            equal_aspect: scatter.equal_aspect,
            show_g_rings: scatter.show_g_rings,
        },
        widgets::Widget::GpsPlot(gps) => {
            let (color_signal, color_unit) = match gps.overlay_signal() {
                Some((sig, unit)) => (Some(sig.clone()), unit.clone()),
                None => (None, String::new()),
            };
            PaneSpec::GpsPlot {
                title: gps.title.clone(),
                color_signal,
                color_unit,
                max_distance_meters: gps.max_distance_meters(),
            }
        }
        widgets::Widget::ViewerTable(w) => PaneSpec::ViewerTable {
            title: w.title.clone(),
        },
        widgets::Widget::ViewerList(w) => PaneSpec::ViewerList {
            title: w.title.clone(),
        },
        widgets::Widget::BusLoad(w) => PaneSpec::BusLoad {
            title: w.title.clone(),
        },
        widgets::Widget::Jitter(w) => PaneSpec::Jitter {
            title: w.title.clone(),
        },
        widgets::Widget::LogParser(w) => PaneSpec::LogParser {
            title: w.title.clone(),
        },
        widgets::Widget::SendUi(w) => PaneSpec::SendUi {
            title: w.title.clone(),
        },
        widgets::Widget::Bootloader(w) => PaneSpec::Bootloader {
            title: w.title.clone(),
        },
        widgets::Widget::Hil(w) => PaneSpec::Hil {
            title: w.title.clone(),
        },
    }
}

pub fn build_tile_node(
    node: &WorkspaceLayoutNode,
    tiles: &mut egui_tiles::Tiles<widgets::Widget>,
    widget_ids: &mut widget_ids::WidgetIds,
    ui_to_can_tx: mpsc::Sender<CanThreadCommand>,
) -> Option<egui_tiles::TileId> {
    match node {
        WorkspaceLayoutNode::Pane { spec } => {
            let widget = instantiate_pane_spec(spec, widget_ids, ui_to_can_tx);
            Some(tiles.insert_pane(widget))
        }
        WorkspaceLayoutNode::Linear {
            dir,
            children,
            shares,
        } => {
            let mut child_ids = Vec::with_capacity(children.len());
            let mut child_shares = Vec::with_capacity(children.len());
            for (idx, child) in children.iter().enumerate() {
                if let Some(cid) = build_tile_node(child, tiles, widget_ids, ui_to_can_tx.clone()) {
                    child_ids.push(cid);
                    let share = shares
                        .get(idx)
                        .copied()
                        .filter(|s| s.is_finite() && *s > 0.0)
                        .unwrap_or(1.0);
                    child_shares.push((cid, share));
                }
            }
            if child_ids.is_empty() {
                return None;
            }
            let egui_dir = match dir {
                LinearSplitDir::Horizontal => egui_tiles::LinearDir::Horizontal,
                LinearSplitDir::Vertical => egui_tiles::LinearDir::Vertical,
            };
            let mut linear = egui_tiles::Linear::new(egui_dir, child_ids);
            for (cid, share) in child_shares {
                linear.shares.set_share(cid, share);
            }
            Some(tiles.insert_container(egui_tiles::Container::Linear(linear)))
        }
        WorkspaceLayoutNode::Tabs { children, active } => {
            let mut child_ids = Vec::with_capacity(children.len());
            for child in children {
                if let Some(cid) = build_tile_node(child, tiles, widget_ids, ui_to_can_tx.clone()) {
                    child_ids.push(cid);
                }
            }
            if child_ids.is_empty() {
                return None;
            }
            let active_id = child_ids
                .get(*active)
                .copied()
                .or_else(|| child_ids.first().copied());
            let mut tabs = egui_tiles::Tabs::new(child_ids);
            if let Some(aid) = active_id {
                tabs.set_active(aid);
            }
            Some(tiles.insert_container(egui_tiles::Container::Tabs(tabs)))
        }
        WorkspaceLayoutNode::Grid { children } => {
            let mut child_ids = Vec::with_capacity(children.len());
            for child in children {
                if let Some(cid) = build_tile_node(child, tiles, widget_ids, ui_to_can_tx.clone()) {
                    child_ids.push(cid);
                }
            }
            if child_ids.is_empty() {
                return None;
            }
            Some(tiles.insert_grid_tile(child_ids))
        }
    }
}

fn instantiate_pane_spec(
    spec: &PaneSpec,
    widget_ids: &mut widget_ids::WidgetIds,
    ui_to_can_tx: mpsc::Sender<CanThreadCommand>,
) -> widgets::Widget {
    match spec {
        PaneSpec::Scope {
            title,
            custom_title,
            x_mode,
            speed_signal_id,
            y_bounds,
            channels,
            axes,
        } => {
            let id = widget_ids.next(widget_constructor::WidgetKind::Scope);
            let mut resolved_channels: Vec<ui::scope::ScopeChannel> = Vec::new();
            if !channels.is_empty() {
                for (idx, ch) in channels.iter().enumerate() {
                    let sig_id = daqcore::analysis::canonicalize_signal_ref(&ch.signal_id);
                    let color = match ch.color_rgb {
                        Some([r, g, b]) => eframe::egui::Color32::from_rgb(r, g, b),
                        None => ui::scope::CHANNEL_PALETTE[idx % ui::scope::CHANNEL_PALETTE.len()],
                    };
                    let label = if ch.label.is_empty() {
                        sig_id.clone()
                    } else {
                        ch.label.clone()
                    };
                    resolved_channels.push(ui::scope::ScopeChannel {
                        is_derived: !sig_id.contains('.')
                            || sig_id.starts_with("battery.")
                            || sig_id.starts_with("vd.")
                            || sig_id.starts_with("thermals."),
                        signal_id: sig_id,
                        label,
                        unit: ch.unit.clone(),
                        color,
                        key: None,
                    });
                }
            } else {
                let mut idx = 0usize;
                for axis in axes {
                    for raw_sig in &axis.signals {
                        let sig_id = daqcore::analysis::canonicalize_signal_ref(raw_sig);
                        let color =
                            ui::scope::CHANNEL_PALETTE[idx % ui::scope::CHANNEL_PALETTE.len()];
                        idx += 1;
                        resolved_channels.push(ui::scope::ScopeChannel {
                            is_derived: !sig_id.contains('.')
                                || sig_id.starts_with("battery.")
                                || sig_id.starts_with("vd.")
                                || sig_id.starts_with("thermals."),
                            label: sig_id.clone(),
                            signal_id: sig_id,
                            unit: axis.unit.clone(),
                            color,
                            key: None,
                        });
                    }
                }
            }
            let effective_title = if title.is_empty() {
                format!("Scope #{id}")
            } else {
                title.clone()
            };
            let y_bounds_override = y_bounds.map(|[lo, hi]| (lo, hi));
            let speed_sig = if speed_signal_id.is_empty() {
                default_speed_signal()
            } else {
                daqcore::analysis::canonicalize_signal_ref(speed_signal_id)
            };
            widgets::Widget::Scope(ui::scope::Scope::new_configured(
                id,
                effective_title,
                *custom_title && !title.is_empty(),
                resolved_channels,
                *x_mode,
                speed_sig,
                y_bounds_override,
            ))
        }
        PaneSpec::StateLane {
            title,
            custom_title,
            lanes,
            signals,
        } => {
            let id = widget_ids.next(widget_constructor::WidgetKind::StateLane);
            let mut pairs: Vec<(String, String)> = Vec::new();
            if !lanes.is_empty() {
                for l in lanes {
                    let sig_id = daqcore::analysis::canonicalize_signal_ref(&l.signal_id);
                    let lbl = if l.label.is_empty() {
                        sig_id.clone()
                    } else {
                        l.label.clone()
                    };
                    pairs.push((sig_id, lbl));
                }
            } else {
                for raw_sig in signals {
                    let sig_id = daqcore::analysis::canonicalize_signal_ref(raw_sig);
                    pairs.push((sig_id.clone(), sig_id));
                }
            }
            let effective_title = if title.is_empty() {
                format!("State Lanes #{id}")
            } else {
                title.clone()
            };
            let mut w = ui::state_lane::StateLane::new_with_lanes(id, effective_title, pairs);
            if !*custom_title && title.is_empty() {
                let _ = &mut w;
            }
            widgets::Widget::StateLane(w)
        }
        PaneSpec::XYScatter {
            title,
            custom_title,
            x_signal,
            y_signal,
            z_color_signal,
            equal_aspect,
            show_g_rings,
        } => {
            let id = widget_ids.next(widget_constructor::WidgetKind::XYScatter);
            let effective_title = if title.is_empty() {
                format!("XY Scatter #{id}")
            } else {
                title.clone()
            };
            let x_sig = daqcore::analysis::canonicalize_signal_ref(x_signal);
            let y_sig = daqcore::analysis::canonicalize_signal_ref(y_signal);
            let z_sig = z_color_signal
                .as_deref()
                .map(daqcore::analysis::canonicalize_signal_ref);
            let mut w = ui::xy_scatter::XYScatter::new_configured(
                id,
                effective_title,
                x_sig,
                y_sig,
                z_sig,
                *equal_aspect,
                *show_g_rings,
            );
            w.set_custom_title(*custom_title && !title.is_empty());
            widgets::Widget::XYScatter(w)
        }
        PaneSpec::GpsPlot {
            title,
            color_signal,
            color_unit,
            max_distance_meters,
        } => {
            let id = widget_ids.next(widget_constructor::WidgetKind::GpsPlot);
            let mut gps = ui::gps_plot::GpsPlot::new(id);
            if !title.is_empty() {
                gps.title = title.clone();
            }
            match color_signal {
                Some(sig) if !sig.trim().is_empty() => {
                    gps.set_overlay_signal(
                        daqcore::analysis::canonicalize_signal_ref(sig),
                        color_unit.clone(),
                    );
                }
                _ => gps.clear_overlay_signal(),
            }
            gps.set_max_distance_meters(*max_distance_meters);
            widgets::Widget::GpsPlot(gps)
        }
        PaneSpec::ViewerTable { title } => {
            let id = widget_ids.next(widget_constructor::WidgetKind::ViewerTable);
            let mut w = ui::viewer_table::ViewerTable::new(id);
            if !title.is_empty() {
                w.title = title.clone();
            }
            widgets::Widget::ViewerTable(w)
        }
        PaneSpec::ViewerList { title } => {
            let id = widget_ids.next(widget_constructor::WidgetKind::ViewerList);
            let mut w = ui::viewer_list::ViewerList::new(id);
            if !title.is_empty() {
                w.title = title.clone();
            }
            widgets::Widget::ViewerList(w)
        }
        PaneSpec::BusLoad { title } => {
            let id = widget_ids.next(widget_constructor::WidgetKind::BusLoad);
            let mut w = ui::bus_load::BusLoad::new(id);
            if !title.is_empty() {
                w.title = title.clone();
            }
            widgets::Widget::BusLoad(w)
        }
        PaneSpec::Jitter { title } => {
            let id = widget_ids.next(widget_constructor::WidgetKind::Jitter);
            let mut w = ui::jitter::Jitter::new(id);
            if !title.is_empty() {
                w.title = title.clone();
            }
            widgets::Widget::Jitter(w)
        }
        PaneSpec::LogParser { title } => {
            let id = widget_ids.next(widget_constructor::WidgetKind::LogParser);
            let mut w = ui::log_parser::LogParser::new(id);
            if !title.is_empty() {
                w.title = title.clone();
            }
            widgets::Widget::LogParser(w)
        }
        PaneSpec::SendUi { title } => {
            let id = widget_ids.next(widget_constructor::WidgetKind::SendUi);
            let mut w = ui::send::SendUi::new(id, ui_to_can_tx);
            if !title.is_empty() {
                w.title = title.clone();
            }
            widgets::Widget::SendUi(w)
        }
        PaneSpec::Bootloader { title } => {
            let id = widget_ids.next(widget_constructor::WidgetKind::Bootloader);
            let mut w = ui::bootloader::Bootloader::new(id);
            if !title.is_empty() {
                w.title = title.clone();
            }
            widgets::Widget::Bootloader(w)
        }
        PaneSpec::Hil { title } => {
            let _ = widget_ids.next(widget_constructor::WidgetKind::Hil);
            let mut w = ui::hil::Hil::new(ui_to_can_tx);
            if !title.is_empty() {
                w.title = title.clone();
            }
            widgets::Widget::Hil(w)
        }
    }
}

pub struct WorkspaceViewModalState {
    pub is_open: bool,
    pub view_name: String,
    pub view_description: String,
    pub include_time_context: bool,
    pub jump_to_saved_time_range: bool,
    pub save_file_path: String,
    pub import_payload: String,
    pub generated_share_string: String,
    pub status_banner: Option<Result<String, String>>,
}

impl Default for WorkspaceViewModalState {
    fn default() -> Self {
        Self {
            is_open: false,
            view_name: "Custom Analysis View".to_owned(),
            view_description: "Saved from PER DAQApp workspace".to_owned(),
            include_time_context: true,
            jump_to_saved_time_range: true,
            save_file_path: "/tmp/custom_view.daqview.json".to_owned(),
            import_payload: String::new(),
            generated_share_string: String::new(),
            status_banner: None,
        }
    }
}

impl WorkspaceViewModalState {
    pub fn open(&mut self) {
        self.is_open = true;
        self.status_banner = None;
    }

    pub fn show(
        &mut self,
        ctx: &eframe::egui::Context,
        tile_tree: &mut egui_tiles::Tree<widgets::Widget>,
        widget_ids: &mut widget_ids::WidgetIds,
        router: &mut daqcore::router::VirtualTimelineRouter,
        ui_to_can_tx: mpsc::Sender<CanThreadCommand>,
    ) {
        if !self.is_open {
            return;
        }

        let mut keep_open = self.is_open;
        let mut do_save_file = false;
        let mut do_copy_share = false;
        let mut do_import_with_context: Option<bool> = None;

        eframe::egui::Window::new("📁 Workspace Views (.daqview.json & Share Strings)")
            .open(&mut keep_open)
            .collapsible(false)
            .resizable(true)
            .default_width(540.0)
            .show(ctx, |ui| {
                ui.heading("Export / Share Current Workspace");
                ui.horizontal(|ui| {
                    ui.label("View Name:");
                    ui.add(
                        eframe::egui::TextEdit::singleline(&mut self.view_name)
                            .desired_width(340.0),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("Description:");
                    ui.add(
                        eframe::egui::TextEdit::singleline(&mut self.view_description)
                            .desired_width(340.0),
                    );
                });
                ui.checkbox(
                    &mut self.include_time_context,
                    "Include timeline window, playhead timestamp & comparison stints",
                );

                ui.horizontal(|ui| {
                    ui.label("File Path:");
                    ui.add(
                        eframe::egui::TextEdit::singleline(&mut self.save_file_path)
                            .desired_width(270.0),
                    );
                    if ui.button("Browse...").clicked()
                        && let Some(picked) = rfd::FileDialog::new()
                            .add_filter("DAQ Workspace View (.daqview.json)", &["json"])
                            .set_file_name("view.daqview.json")
                            .save_file()
                    {
                        self.save_file_path = picked.to_string_lossy().into_owned();
                    }
                    if ui.button("💾 Save .daqview.json").clicked() {
                        do_save_file = true;
                    }
                });

                ui.horizontal(|ui| {
                    if ui
                        .button("📋 Generate & Copy Slack/Discord Share String")
                        .clicked()
                    {
                        do_copy_share = true;
                    }
                });

                if !self.generated_share_string.is_empty() {
                    ui.horizontal(|ui| {
                        ui.label("Share String:");
                        let mut preview = self.generated_share_string.clone();
                        ui.add(
                            eframe::egui::TextEdit::singleline(&mut preview)
                                .desired_width(380.0),
                        );
                    });
                }

                ui.separator();
                ui.heading("Import Workspace View (.daqview.json or daqview1:... String)");
                ui.horizontal(|ui| {
                    if ui.button("📂 Load .daqview.json File...").clicked()
                        && let Some(picked) = rfd::FileDialog::new()
                            .add_filter("DAQ Workspace View", &["json"])
                            .pick_file()
                    {
                        match WorkspaceView::load_from_file(&picked)
                            .and_then(|v| v.to_json_pretty().map(|s| (v.name, s)))
                        {
                            Ok((name, contents)) => {
                                self.import_payload = contents;
                                self.status_banner = Some(Ok(format!(
                                    "Loaded '{name}' from {} — choose import mode below.",
                                    picked.display()
                                )));
                            }
                            Err(err) => {
                                self.status_banner = Some(Err(format!(
                                    "Failed to load {}: {err}",
                                    picked.display()
                                )));
                            }
                        }
                    }
                    if ui.button("Clear Input").clicked() {
                        self.import_payload.clear();
                    }
                });

                ui.add(
                    eframe::egui::TextEdit::multiline(&mut self.import_payload)
                        .hint_text(
                            "Paste a `daqview1:...` share string or `.daqview.json` contents here...",
                        )
                        .desired_rows(4)
                        .desired_width(f32::INFINITY),
                );

                let has_payload = !self.import_payload.trim().is_empty();
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(
                            has_payload,
                            eframe::egui::Button::new(
                                eframe::egui::RichText::new(
                                    "🚀 Load Layout + Jump to Saved Time Range",
                                )
                                .strong(),
                            ),
                        )
                        .clicked()
                    {
                        do_import_with_context = Some(true);
                    }
                    if ui
                        .add_enabled(
                            has_payload,
                            eframe::egui::Button::new("📁 Load Layout Only (Keep Timeline)"),
                        )
                        .clicked()
                    {
                        do_import_with_context = Some(false);
                    }
                });

                if let Some(status) = &self.status_banner {
                    ui.add_space(4.0);
                    match status {
                        Ok(msg) => {
                            ui.colored_label(
                                eframe::egui::Color32::from_rgb(34, 197, 94),
                                format!("OK: {msg}"),
                            );
                        }
                        Err(err) => {
                            ui.colored_label(
                                eframe::egui::Color32::from_rgb(239, 68, 68),
                                format!("Error: {err}"),
                            );
                        }
                    }
                }
            });

        if !keep_open {
            self.is_open = false;
        }

        if do_save_file {
            match WorkspaceView::capture_from_workspace(
                tile_tree,
                router,
                self.view_name.trim(),
                self.view_description.trim(),
                self.include_time_context,
            )
            .and_then(|view| view.save_to_file(self.save_file_path.trim()))
            {
                Ok(()) => {
                    self.status_banner = Some(Ok(format!(
                        "Saved workspace view to {}",
                        self.save_file_path.trim()
                    )));
                }
                Err(err) => {
                    self.status_banner = Some(Err(err.to_string()));
                }
            }
        }

        if do_copy_share {
            match WorkspaceView::capture_from_workspace(
                tile_tree,
                router,
                self.view_name.trim(),
                self.view_description.trim(),
                self.include_time_context,
            )
            .and_then(|view| view.to_share_string())
            {
                Ok(share) => {
                    ctx.copy_text(share.clone());
                    self.generated_share_string = share;
                    self.status_banner = Some(Ok(
                        "Copied `daqview1:...` share string to clipboard.".to_owned(),
                    ));
                }
                Err(err) => {
                    self.status_banner = Some(Err(err.to_string()));
                }
            }
        }

        if let Some(apply_time_context) = do_import_with_context {
            match WorkspaceView::from_str_flexible(self.import_payload.trim()).and_then(|view| {
                let name = view.name.clone();
                view.apply_to_workspace(
                    tile_tree,
                    widget_ids,
                    router,
                    ui_to_can_tx,
                    apply_time_context,
                )
                .map(|()| name)
            }) {
                Ok(name) => {
                    self.status_banner = Some(Ok(format!(
                        "Loaded workspace view '{name}' ({}).",
                        if apply_time_context {
                            "layout + saved time range"
                        } else {
                            "layout only"
                        }
                    )));
                }
                Err(err) => {
                    self.status_banner = Some(Err(err.to_string()));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_workspace_presets_load_and_build_tile_trees() {
        let registry = WorkspacePresetRegistry::with_builtin();
        assert!(registry.presets().len() >= 6);

        for preset_id in [
            "vd_handling_suspension",
            "powertrain_accumulator",
            "thermals_cooling",
            "aero_ride_height",
            "sensors_controls",
            "trackside_live_debug",
        ] {
            let preset = registry.get(preset_id).expect("preset must exist");
            let root = preset.resolved_layout().expect("resolve preset layout");
            let mut tiles = egui_tiles::Tiles::default();
            let mut ids = widget_ids::WidgetIds::new();
            let (tx, _rx) = mpsc::channel();
            let root_id = build_tile_node(&root, &mut tiles, &mut ids, tx)
                .expect("build tile node for preset");
            let tree = egui_tiles::Tree::new("preset_test", root_id, tiles);
            let pane_count = tree
                .tiles
                .tiles()
                .filter(|t| matches!(t, egui_tiles::Tile::Pane(_)))
                .count();
            assert!(
                pane_count >= 4,
                "preset {preset_id} should have at least 4 panes, got {pane_count}"
            );
        }
    }

    #[test]
    fn daqview_json_and_share_string_round_trip_preserves_layout_formulas_and_time_context() {
        let registry = WorkspacePresetRegistry::with_builtin();
        let mut view = registry
            .get("vd_handling_suspension")
            .expect("vd preset")
            .clone();
        view.context = Some(WorkspaceTimeContext {
            track_day_id: Some("2026-09-06".to_owned()),
            viewport_start_utc_ms: 1_788_719_762_000,
            viewport_end_utc_ms: 1_788_719_777_000,
            setpoint_utc_ms: Some(1_788_719_770_000),
            compare_annotation_ids: vec!["stint_1".to_owned(), "stint_2".to_owned()],
            gap_folding_enabled: true,
        });
        view.custom_formulas.push(DerivedSignalDef {
            id: "vd.custom_slip_ratio".to_owned(),
            label: "Custom Slip Ratio".to_owned(),
            expression: "vd.wheel_speed_avg_rpm / max(gps_speed_heading.gps_speed, 1.0)".to_owned(),
            unit: "rpm/(m/s)".to_owned(),
        });

        let json = view.to_json_pretty().expect("serialize json");
        let from_json = WorkspaceView::from_str_flexible(&json).expect("parse json");
        assert_eq!(from_json.name, view.name);
        assert_eq!(from_json.context, view.context);
        assert_eq!(from_json.custom_formulas, view.custom_formulas);

        let share_str = view.to_share_string().expect("encode share string");
        assert!(share_str.starts_with(SHARE_STRING_PREFIX));
        let from_share = WorkspaceView::from_str_flexible(&share_str).expect("decode share string");
        assert_eq!(from_share.name, view.name);
        assert_eq!(from_share.context, view.context);
        assert_eq!(from_share.custom_formulas, view.custom_formulas);
        assert_eq!(
            from_share.resolved_layout().expect("share layout"),
            view.resolved_layout().expect("orig layout")
        );

        let mut router = daqcore::router::VirtualTimelineRouter::live(
            Time::from_unix_millis(1_788_719_800_000),
            60.0,
        )
        .expect("live router");
        let mut ids = widget_ids::WidgetIds::new();
        let (tx, _rx) = mpsc::channel();
        let mut tree = egui_tiles::Tree::empty("test_tree");

        from_share
            .apply_to_workspace(&mut tree, &mut ids, &mut router, tx, true)
            .expect("apply workspace view");
        assert_eq!(router.timeline().start_utc_ms(), 1_788_719_762_000);
        assert_eq!(router.timeline().end_utc_ms(), 1_788_719_777_000);
        assert_eq!(router.timeline().setpoint_utc_ms(), 1_788_719_770_000);
        assert!(
            router
                .formula_engine()
                .get("vd.custom_slip_ratio")
                .is_some()
        );

        let captured = WorkspaceView::capture_from_workspace(
            &tree,
            &router,
            "Captured VD",
            "Round-tripped from live tile tree",
            true,
        )
        .expect("capture workspace view");
        let tmp_dir = tempfile::tempdir().expect("tempdir");
        let file_path = tmp_dir.path().join("test.daqview.json");
        captured
            .save_to_file(&file_path)
            .expect("save .daqview.json file");
        let loaded = WorkspaceView::load_from_file(&file_path).expect("load .daqview.json file");
        assert_eq!(loaded.name, "Captured VD");
        assert_eq!(loaded.context, captured.context);
        assert_eq!(loaded.custom_formulas, view.custom_formulas);
    }

    #[test]
    fn master_plan_v2_section_4_3_daqview_schema_deserializes_cleanly() {
        let master_plan_json = r#"{
          "schema_version": 1,
          "name": "VD — Suspension & Handling",
          "context": {
            "track_day_id": "2026-09-06",
            "viewport_start_utc_ms": 1725648012000,
            "viewport_end_utc_ms": 1725648027000,
            "setpoint_utc_ms": 1725648019450,
            "compare_annotation_ids": [],
            "gap_folding_enabled": true
          },
          "custom_formulas": [
            {
              "id": "vd.roll_deg",
              "label": "Roll Angle",
              "expression": "(front_shockpots.left - front_shockpots.right) * 0.085",
              "unit": "deg"
            }
          ],
          "layout": {
            "root_split": {
              "dir": "vertical",
              "shares": [0.45, 0.35, 0.20],
              "children": ["pane_scope_1", "pane_scatter_1", "pane_states_1"]
            }
          },
          "panes": {
            "pane_scope_1": {
              "widget": "scope",
              "x_mode": "time",
              "axes": [
                { "id": "mm", "unit": "mm", "position": "left", "signals": ["front_shockpots.left", "front_shockpots.right"] },
                { "id": "deg", "unit": "deg", "position": "right", "signals": ["LWS_Standard.LWS_ANGLE", "vd.roll_deg"] }
              ]
            },
            "pane_scatter_1": {
              "widget": "xy_scatter",
              "x_signal": "IMU_acceleration.Y_axis",
              "y_signal": "IMU_acceleration.X_axis",
              "color_signal": "gps_speed_heading.gps_speed"
            },
            "pane_states_1": {
              "widget": "state_lanes",
              "signals": ["main_hb.car_state", "vcu_driver_request.vcu_mode", "a_box_fault_sync.IMD"]
            }
          }
        }"#;

        let view =
            WorkspaceView::from_str_flexible(master_plan_json).expect("parse section 4.3 schema");
        assert_eq!(view.name, "VD — Suspension & Handling");
        assert_eq!(view.custom_formulas.len(), 1);
        let root = view.resolved_layout().expect("resolve shorthand layout");
        let mut tiles = egui_tiles::Tiles::default();
        let mut ids = widget_ids::WidgetIds::new();
        let (tx, _rx) = mpsc::channel();
        let root_id =
            build_tile_node(&root, &mut tiles, &mut ids, tx).expect("build shorthand tree");
        let tree = egui_tiles::Tree::new("mp_tree", root_id, tiles);
        assert_eq!(
            tree.tiles
                .tiles()
                .filter(|t| matches!(t, egui_tiles::Tile::Pane(_)))
                .count(),
            3
        );
    }

    #[test]
    fn per26_m7_workspace_presets_daqview_share_strings_and_export_modal_e2e() {
        let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let dbc_path = manifest_dir.join("../../superdbc/superdbc_per26.json");
        let log_1 = manifest_dir
            .join("../../PER26/2026-09-06/1_no_cooling/raw/log-2026-09-06--18-36-02.log");
        let log_2 =
            manifest_dir.join("../../PER26/2026-09-06/2_regen/raw/log-2026-09-06--20-16-37.log");
        if !dbc_path.is_file() || !log_1.is_file() || !log_2.is_file() {
            return;
        }

        let database =
            daqcore::superdbc::database::Database::load(&dbc_path).expect("load PER26 database");
        let selection = daqcore::can_thread::BusSelection::default_single(&database)
            .expect("default bus selection");
        let db_sel = app::DatabaseSelection {
            database,
            selection,
        };

        let tmp_raw = tempfile::tempdir().expect("temp raw dir");
        std::fs::copy(&log_1, tmp_raw.path().join("log-2026-09-06--18-36-02.log"))
            .expect("copy log 1");
        std::fs::copy(&log_2, tmp_raw.path().join("log-2026-09-06--20-16-37.log"))
            .expect("copy log 2");

        let tmp_store = tempfile::tempdir().expect("temp store dir");
        let summary = daqcore::log_parse::import_logs_to_store(
            tmp_raw.path(),
            tmp_store.path(),
            &db_sel.database,
        )
        .expect("import PER26 logs");
        assert!(summary.partitions_written >= 2);
        let m1 = &summary.manifests[0];
        let m2 = &summary.manifests[1];

        let mut router = daqcore::router::VirtualTimelineRouter::live(
            Time::from_unix_millis(m2.end_utc_ms),
            30.0,
        )
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

        // 1. Apply and render all 6 subteam workspace presets against real PER26 data
        let registry = WorkspacePresetRegistry::with_builtin();
        let mut ids = widget_ids::WidgetIds::new();
        let (tx, _rx) = mpsc::channel();
        let mut tree = egui_tiles::Tree::empty("per26_m7_tree");
        let ctx = eframe::egui::Context::default();
        ui::theme::store_theme(&ctx, ui::theme::ThemeSelection::default().get_colors());

        for preset in registry.presets() {
            preset
                .apply_to_workspace(&mut tree, &mut ids, &mut router, tx.clone(), false)
                .expect("apply subteam preset");
            let pane_count = tree
                .tiles
                .tiles()
                .filter(|t| matches!(t, egui_tiles::Tile::Pane(_)))
                .count();
            assert!(pane_count >= 4);
        }

        // 2. Load VD preset, add custom formula, capture .daqview.json & share string,
        //    and test both "Load Layout Only" and "Load Layout + Jump to Saved Time Range"
        let vd_preset = registry
            .get("vd_handling_suspension")
            .expect("vd preset")
            .clone();
        vd_preset
            .apply_to_workspace(&mut tree, &mut ids, &mut router, tx.clone(), false)
            .expect("apply vd preset");
        router
            .formula_engine_mut()
            .merge_defs(&[DerivedSignalDef {
                id: "vd.custom_shock_avg_mm".to_owned(),
                label: "Custom Shock Avg".to_owned(),
                expression: "0.25 * (front_shockpots.left + front_shockpots.right + rear_shockpots.left + rear_shockpots.right)".to_owned(),
                unit: "mm".to_owned(),
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

        let captured = WorkspaceView::capture_from_workspace(
            &tree,
            &router,
            "PER26 Stint 1 VD Analysis",
            "Saved during M7 E2E verification",
            true,
        )
        .expect("capture workspace");
        let share_str = captured.to_share_string().expect("encode share string");

        // Move timeline elsewhere and clear tree
        router
            .timeline_mut()
            .snap_to_range_ms(m2.start_utc_ms, m2.start_utc_ms + 20_000);
        tree = egui_tiles::Tree::empty("cleared");

        // Re-import with apply_time_context = false ("Load Layout Only")
        let decoded = WorkspaceView::from_str_flexible(&share_str).expect("decode share string");
        decoded
            .apply_to_workspace(&mut tree, &mut ids, &mut router, tx.clone(), false)
            .expect("apply layout only");
        assert_eq!(router.timeline().start_utc_ms(), m2.start_utc_ms);
        assert!(
            tree.tiles
                .tiles()
                .filter(|t| matches!(t, egui_tiles::Tile::Pane(_)))
                .count()
                >= 5
        );

        // Re-import with apply_time_context = true ("Load Layout + Jump to Saved Time Range")
        decoded
            .apply_to_workspace(&mut tree, &mut ids, &mut router, tx.clone(), true)
            .expect("apply layout and time context");
        assert_eq!(router.timeline().start_utc_ms(), saved_start);
        assert_eq!(router.timeline().end_utc_ms(), saved_end);
        assert_eq!(router.timeline().setpoint_utc_ms(), saved_sp);

        // 3. Verify 3-click ExportModalState UI & export across Parquet, CSV, and MATLAB .mat
        let mut export_modal = ui::export_modal::ExportModalState::default();
        export_modal.open(&router, Some(&db_sel));
        export_modal.apply_preset("vd", &router, Some(&db_sel));
        export_modal.range_mode = ui::export_modal::UiExportRangeMode::CurrentViewport;

        let mut view_modal = WorkspaceViewModalState::default();
        view_modal.open();

        // Render one frame with both modals open to verify zero UI panics
        let screen_rect = eframe::egui::Rect::from_min_size(
            eframe::egui::Pos2::ZERO,
            eframe::egui::vec2(1400.0, 900.0),
        );
        let raw_input = eframe::egui::RawInput {
            screen_rect: Some(screen_rect),
            ..Default::default()
        };
        let _ = ctx.run(raw_input, |ctx| {
            export_modal.show(ctx, &mut router, Some(&db_sel));
            view_modal.show(ctx, &mut tree, &mut ids, &mut router, tx.clone());
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
                .execute_export(&mut router, Some(&db_sel))
                .expect("execute modal export");
            assert!(summary.row_count > 500);
            assert!(out_path.is_file());
        }
    }
}
