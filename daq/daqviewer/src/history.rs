//! Coalesced structural snapshot Undo/Redo history for `daqviewer` (`daqapp` and `daqweb`).

use crate::{app, ui, widget_ids, widgets, workspace_io};
use daqcore::analysis::{DerivedSignalDef, FormulaEngine};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    sync::OnceLock,
};

/// Maximum number of snapshots retained in the workspace Undo/Redo ring.
pub const MAX_HISTORY_LEN: usize = 100;

/// Debounce window (seconds) before committing workspace changes while a `TextEdit` holds focus.
const TEXT_EDIT_DEBOUNCE_SECS: f64 = 0.6;

fn builtin_formula_engine() -> &'static FormulaEngine {
    static ENGINE: OnceLock<FormulaEngine> = OnceLock::new();
    ENGINE.get_or_init(FormulaEngine::with_builtin_config)
}

/// Extra per-`Scope` session state preserved across Undo/Redo without altering `.daqview.json`.
#[derive(Debug, Clone, PartialEq)]
pub struct ScopeUndoExtras {
    pub hidden_channels: BTreeSet<String>,
    pub lane_y_overrides: BTreeMap<usize, [f64; 2]>,
    pub show_inline_stats: bool,
}

/// Complete, comparable snapshot of undoable workspace state.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceSnapshot {
    pub layout: Option<workspace_io::WorkspaceLayoutNode>,
    /// Pre-order traversal of `Scope` panes matching `layout`.
    pub scope_extras: Vec<ScopeUndoExtras>,
    pub active_workspace_tab: Option<String>,
    /// Non-builtin (or modified builtin) derived signal definitions, sorted by `id` with
    /// timestamps normalized for stable equality checks.
    pub custom_formulas: Vec<DerivedSignalDef>,
}

impl WorkspaceSnapshot {
    pub fn capture(
        tile_tree: &egui_tiles::Tree<widgets::Widget>,
        router: &daqcore::router::VirtualTimelineRouter,
        active_workspace_tab: Option<String>,
    ) -> Self {
        let mut layout = tile_tree
            .root
            .and_then(|root_id| workspace_io::capture_tile_node(&tile_tree.tiles, root_id));
        if let Some(node) = &mut layout {
            normalize_layout_floats(node);
        }

        let mut scope_extras = Vec::new();
        if let Some(root_id) = tile_tree.root {
            collect_scope_extras(&tile_tree.tiles, root_id, &mut scope_extras);
        }

        let custom_formulas = capture_custom_formulas(router.formula_engine());

        Self {
            layout,
            scope_extras,
            active_workspace_tab,
            custom_formulas,
        }
    }
}

fn quantize_share(share: f32) -> f32 {
    if share.is_finite() {
        (share * 1000.0).round() / 1000.0
    } else {
        1.0
    }
}

fn normalize_layout_floats(node: &mut workspace_io::WorkspaceLayoutNode) {
    match node {
        workspace_io::WorkspaceLayoutNode::Pane { .. } => {}
        workspace_io::WorkspaceLayoutNode::Linear {
            children, shares, ..
        } => {
            for share in shares.iter_mut() {
                *share = quantize_share(*share);
            }
            for child in children.iter_mut() {
                normalize_layout_floats(child);
            }
        }
        workspace_io::WorkspaceLayoutNode::Tabs { children, .. }
        | workspace_io::WorkspaceLayoutNode::Grid { children } => {
            for child in children.iter_mut() {
                normalize_layout_floats(child);
            }
        }
    }
}

fn collect_scope_extras(
    tiles: &egui_tiles::Tiles<widgets::Widget>,
    tile_id: egui_tiles::TileId,
    out: &mut Vec<ScopeUndoExtras>,
) {
    let Some(tile) = tiles.get(tile_id) else {
        return;
    };
    match tile {
        egui_tiles::Tile::Pane(widgets::Widget::Scope(scope)) => {
            let hidden_channels = scope.hidden_channels().iter().cloned().collect();
            let lane_y_overrides = scope
                .lane_y_overrides()
                .iter()
                .map(|(&lane, &(lo, hi))| (lane, [lo, hi]))
                .collect();
            out.push(ScopeUndoExtras {
                hidden_channels,
                lane_y_overrides,
                show_inline_stats: scope.show_inline_stats,
            });
        }
        egui_tiles::Tile::Pane(_) => {}
        egui_tiles::Tile::Container(container) => match container {
            egui_tiles::Container::Linear(linear) => {
                for &child_id in &linear.children {
                    collect_scope_extras(tiles, child_id, out);
                }
            }
            egui_tiles::Container::Tabs(tabs) => {
                for &child_id in &tabs.children {
                    collect_scope_extras(tiles, child_id, out);
                }
            }
            egui_tiles::Container::Grid(grid) => {
                for &child_id in grid.children() {
                    collect_scope_extras(tiles, child_id, out);
                }
            }
        },
    }
}

fn apply_scope_extras(
    tiles: &mut egui_tiles::Tiles<widgets::Widget>,
    tile_id: egui_tiles::TileId,
    extras: &mut std::slice::Iter<'_, ScopeUndoExtras>,
) {
    let children: Option<Vec<egui_tiles::TileId>> = match tiles.get_mut(tile_id) {
        Some(egui_tiles::Tile::Pane(widgets::Widget::Scope(scope))) => {
            if let Some(extra) = extras.next() {
                let hidden: HashSet<String> = extra.hidden_channels.iter().cloned().collect();
                scope.set_hidden_channels(hidden);
                let overrides: BTreeMap<usize, (f64, f64)> = extra
                    .lane_y_overrides
                    .iter()
                    .map(|(&lane, &[lo, hi])| (lane, (lo, hi)))
                    .collect();
                scope.set_lane_y_overrides(overrides);
                scope.show_inline_stats = extra.show_inline_stats;
            }
            None
        }
        Some(egui_tiles::Tile::Pane(_)) | None => None,
        Some(egui_tiles::Tile::Container(container)) => Some(match container {
            egui_tiles::Container::Linear(linear) => linear.children.clone(),
            egui_tiles::Container::Tabs(tabs) => tabs.children.clone(),
            egui_tiles::Container::Grid(grid) => grid.children().copied().collect(),
        }),
    };
    if let Some(children) = children {
        for child_id in children {
            apply_scope_extras(tiles, child_id, extras);
        }
    }
}

fn normalize_formula_def(def: &DerivedSignalDef) -> DerivedSignalDef {
    let mut norm = def.clone();
    norm.updated_at_ms = 0;
    norm.deleted = false;
    norm
}

fn capture_custom_formulas(engine: &FormulaEngine) -> Vec<DerivedSignalDef> {
    let builtin = builtin_formula_engine();
    let mut custom: Vec<DerivedSignalDef> = engine
        .definitions()
        .iter()
        .filter(|compiled| {
            let norm = normalize_formula_def(&compiled.def);
            builtin
                .get(&compiled.def.id)
                .is_none_or(|b| normalize_formula_def(&b.def) != norm)
        })
        .map(|compiled| normalize_formula_def(&compiled.def))
        .collect();
    custom.sort_by(|a, b| a.id.cmp(&b.id));
    custom
}

fn sync_custom_formulas(
    router: &mut daqcore::router::VirtualTimelineRouter,
    target_custom: &[DerivedSignalDef],
) {
    let builtin = builtin_formula_engine();
    let current_custom = capture_custom_formulas(router.formula_engine());
    if current_custom == target_custom {
        return;
    }

    let target_by_id: BTreeMap<&str, &DerivedSignalDef> = target_custom
        .iter()
        .map(|def| (def.id.as_str(), def))
        .collect();

    // Remove or revert formulas that are no longer customized in `target_custom`.
    for existing in &current_custom {
        if !target_by_id.contains_key(existing.id.as_str()) {
            if let Some(orig_builtin) = builtin.get(&existing.id) {
                let _ = router.save_derived_signal(orig_builtin.def.clone());
            } else {
                let _ = router.delete_derived_signal(&existing.id);
            }
        }
    }

    // Upsert formulas that are added or changed in `target_custom`.
    for target in target_custom {
        let already_matches = router
            .formula_engine()
            .get(&target.id)
            .is_some_and(|c| normalize_formula_def(&c.def) == *target);
        if !already_matches {
            let _ = router.save_derived_signal(target.clone());
        }
    }
}

fn extract_platform_widgets(
    tile_tree: &mut egui_tiles::Tree<widgets::Widget>,
) -> Vec<Box<dyn app::PlatformWidget>> {
    let mut old_tree = std::mem::replace(tile_tree, egui_tiles::Tree::empty("workspace_tree"));
    let ids: Vec<egui_tiles::TileId> = old_tree.tiles.tile_ids().collect();
    let mut extracted = Vec::new();
    for id in ids {
        if let Some(egui_tiles::Tile::Pane(widgets::Widget::Platform(widget))) =
            old_tree.tiles.remove(id)
        {
            extracted.push(widget);
        }
    }
    extracted
}

/// Bounded workspace Undo/Redo stack with gesture, text-edit, and cross-scope move coalescing.
#[derive(Debug, Default)]
pub struct WorkspaceHistory {
    entries: Vec<WorkspaceSnapshot>,
    cursor: usize,
    initialized: bool,
    pending_text_commit_since: Option<f64>,
}

impl WorkspaceHistory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    pub fn can_undo(&self) -> bool {
        self.initialized && self.cursor > 0
    }

    pub fn can_redo(&self) -> bool {
        self.initialized && self.cursor + 1 < self.entries.len()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Seed or reset the history stack to a single baseline snapshot.
    pub fn reset(&mut self, baseline: WorkspaceSnapshot) {
        self.entries.clear();
        self.entries.push(baseline);
        self.cursor = 0;
        self.initialized = true;
        self.pending_text_commit_since = None;
    }

    /// Inspect current workspace state at the end of a frame and commit a new snapshot if changed
    /// and no continuous gesture, drag-and-drop, cross-scope handoff, or active text edit is in flight.
    pub fn sync_after_frame(
        &mut self,
        tile_tree: &egui_tiles::Tree<widgets::Widget>,
        router: &daqcore::router::VirtualTimelineRouter,
        active_workspace_tab: Option<String>,
        ctx: &eframe::egui::Context,
    ) {
        if !self.initialized {
            self.reset(WorkspaceSnapshot::capture(
                tile_tree,
                router,
                active_workspace_tab,
            ));
            return;
        }

        let pointer_down = ctx.input(|i| i.pointer.any_down());
        let dnd_active = eframe::egui::DragAndDrop::has_any_payload(ctx);
        let pending_scope_moves = ui::scope::Scope::has_pending_signal_moves(ctx);
        if pointer_down || dnd_active || pending_scope_moves {
            return;
        }

        let current = WorkspaceSnapshot::capture(tile_tree, router, active_workspace_tab);
        if self.entries.get(self.cursor) == Some(&current) {
            self.pending_text_commit_since = None;
            return;
        }

        let text_focused = ctx.wants_keyboard_input();
        if text_focused {
            let now = ctx.input(|i| i.time);
            let since = *self.pending_text_commit_since.get_or_insert(now);
            if now - since < TEXT_EDIT_DEBOUNCE_SECS {
                return;
            }
        }

        self.push_snapshot(current);
    }

    fn push_snapshot(&mut self, snapshot: WorkspaceSnapshot) {
        if !self.initialized {
            self.reset(snapshot);
            return;
        }
        if self.entries.get(self.cursor) == Some(&snapshot) {
            self.pending_text_commit_since = None;
            return;
        }
        self.entries.truncate(self.cursor + 1);
        self.entries.push(snapshot);
        if self.entries.len() > MAX_HISTORY_LEN {
            let overflow = self.entries.len() - MAX_HISTORY_LEN;
            self.entries.drain(..overflow);
        }
        self.cursor = self.entries.len().saturating_sub(1);
        self.pending_text_commit_since = None;
    }

    /// Step backward one snapshot if available, returning the snapshot to apply.
    pub fn step_undo(&mut self) -> Option<WorkspaceSnapshot> {
        if !self.can_undo() {
            return None;
        }
        self.cursor -= 1;
        self.pending_text_commit_since = None;
        self.entries.get(self.cursor).cloned()
    }

    /// Step forward one snapshot if available, returning the snapshot to apply.
    pub fn step_redo(&mut self) -> Option<WorkspaceSnapshot> {
        if !self.can_redo() {
            return None;
        }
        self.cursor += 1;
        self.pending_text_commit_since = None;
        self.entries.get(self.cursor).cloned()
    }
}

/// Apply a captured [`WorkspaceSnapshot`] onto `state`, preserving live [`app::PlatformWidget`]
/// instances across the tile tree rebuild and syncing any custom formula differences.
pub fn apply_snapshot(
    snapshot: &WorkspaceSnapshot,
    tile_tree: &mut egui_tiles::Tree<widgets::Widget>,
    widget_ids: &mut widget_ids::WidgetIds,
    router: &mut daqcore::router::VirtualTimelineRouter,
    active_workspace_tab: &mut Option<String>,
    platform: &mut dyn app::ViewerPlatform,
    ctx: &eframe::egui::Context,
) {
    eframe::egui::Popup::close_all(ctx);

    sync_custom_formulas(router, &snapshot.custom_formulas);

    let mut reusable_platform = extract_platform_widgets(tile_tree);
    let mut new_widget_ids = widget_ids::WidgetIds::new();

    if let Some(root_node) = &snapshot.layout {
        let mut tiles = egui_tiles::Tiles::default();
        if let Some(root_id) = workspace_io::build_tile_node_with_reuse(
            root_node,
            &mut tiles,
            &mut new_widget_ids,
            platform,
            Some(router.formula_engine()),
            &mut reusable_platform,
        ) {
            let mut extras_iter = snapshot.scope_extras.iter();
            apply_scope_extras(&mut tiles, root_id, &mut extras_iter);
            *tile_tree = egui_tiles::Tree::new("workspace_tree", root_id, tiles);
        } else {
            *tile_tree = egui_tiles::Tree::empty("workspace_tree");
        }
    } else {
        *tile_tree = egui_tiles::Tree::empty("workspace_tree");
    }

    *widget_ids = new_widget_ids;
    *active_workspace_tab = snapshot.active_workspace_tab.clone();
}
