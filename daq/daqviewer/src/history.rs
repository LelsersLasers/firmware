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
        workspace_io::WorkspaceLayoutNode::Pane { spec } => {
            if let workspace_io::PaneSpec::Scope { plot, .. } = spec {
                plot.plot_names.retain(|_, name| !name.is_empty());
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        action::AppAction,
        app::{NoopViewerPlatform, PlatformWidget, ViewerPlatform, ViewerState},
        shortcuts::ShortcutHandler,
        ui::{
            scope::Scope,
            scope_settings::{AxisConfig, AxisSide, Destination, SignalStyle, TraceMode},
            theme::ThemeSelection,
        },
        widget_constructor::{WidgetConstructor, WidgetKind},
    };
    use eframe::egui::{self, Event, Key, Modifiers, PointerButton, Pos2};
    use std::any::Any;

    fn test_state() -> (ViewerState, NoopViewerPlatform, egui::Context) {
        let t0 = daqcore::time::Time::from_unix_millis(1_788_719_789_000);
        let router = daqcore::router::VirtualTimelineRouter::live(t0, 30.0).expect("live router");
        let state = ViewerState::new(
            router,
            ThemeSelection::default(),
            None,
            String::new(),
            false,
            None,
            None,
        );
        let ctx = egui::Context::default();
        crate::ui::theme::store_theme(&ctx, ThemeSelection::default().get_colors());
        (state, NoopViewerPlatform, ctx)
    }

    fn run_frame(
        state: &mut ViewerState,
        platform: &mut dyn ViewerPlatform,
        ctx: &egui::Context,
        time: f64,
        events: Vec<Event>,
    ) {
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    Pos2::ZERO,
                    egui::vec2(1200.0, 720.0),
                )),
                time: Some(time),
                events,
                ..Default::default()
            },
            |ctx| {
                state.update_frame(platform, ctx);
            },
        );
    }

    fn pane_count(state: &ViewerState) -> usize {
        state
            .tile_tree
            .tiles
            .tiles()
            .filter(|t| matches!(t, egui_tiles::Tile::Pane(_)))
            .count()
    }

    fn first_scope_mut(state: &mut ViewerState) -> &mut Scope {
        state
            .tile_tree
            .tiles
            .tiles_mut()
            .find_map(|tile| match tile {
                egui_tiles::Tile::Pane(widgets::Widget::Scope(scope)) => Some(scope.as_mut()),
                _ => None,
            })
            .expect("expected a Scope pane")
    }

    fn first_scope(state: &ViewerState) -> &Scope {
        state
            .tile_tree
            .tiles
            .tiles()
            .find_map(|tile| match tile {
                egui_tiles::Tile::Pane(widgets::Widget::Scope(scope)) => Some(scope.as_ref()),
                _ => None,
            })
            .expect("expected a Scope pane")
    }

    #[test]
    fn undo_and_redo_restore_pane_spawn_close_clear_and_preset_switch() {
        let (mut state, mut platform, ctx) = test_state();
        state.reset_undo_history();
        assert!(!state.history.can_undo());
        assert!(!state.history.can_redo());

        // 1. Spawn a Scope pane
        state
            .action_queue
            .push(AppAction::SpawnWidget(WidgetConstructor::ScopeEmpty));
        run_frame(&mut state, &mut platform, &ctx, 1.0, vec![]);
        assert_eq!(pane_count(&state), 1);
        assert!(state.history.can_undo());

        // 2. Load a workspace preset tab
        state.action_queue.push(AppAction::LoadWorkspacePreset {
            preset_id: "vd_handling_suspension".to_owned(),
            apply_time_context: false,
        });
        run_frame(&mut state, &mut platform, &ctx, 2.0, vec![]);
        let vd_panes = pane_count(&state);
        assert!(vd_panes > 1);
        assert_eq!(
            state.active_workspace_tab.as_deref(),
            Some("vd_handling_suspension")
        );

        // 3. Clear workspace (`× Clear`)
        state.action_queue.push(AppAction::ClearWorkspace);
        run_frame(&mut state, &mut platform, &ctx, 3.0, vec![]);
        assert_eq!(pane_count(&state), 0);
        assert_eq!(state.active_workspace_tab, None);

        // Undo Clear -> restores VD preset and active tab
        state.action_queue.push(AppAction::Undo);
        run_frame(&mut state, &mut platform, &ctx, 4.0, vec![]);
        assert_eq!(pane_count(&state), vd_panes);
        assert_eq!(
            state.active_workspace_tab.as_deref(),
            Some("vd_handling_suspension")
        );
        assert!(state.history.can_redo());

        // Undo Preset Switch -> restores single Scope pane
        state.action_queue.push(AppAction::Undo);
        run_frame(&mut state, &mut platform, &ctx, 5.0, vec![]);
        assert_eq!(pane_count(&state), 1);
        assert_eq!(state.active_workspace_tab, None);

        // Undo Spawn -> restores empty baseline
        state.action_queue.push(AppAction::Undo);
        run_frame(&mut state, &mut platform, &ctx, 6.0, vec![]);
        assert_eq!(pane_count(&state), 0);
        assert!(!state.history.can_undo());

        // Redo all the way back to Clear
        for step in 0..3 {
            state.action_queue.push(AppAction::Redo);
            run_frame(&mut state, &mut platform, &ctx, 7.0 + step as f64, vec![]);
        }
        assert_eq!(pane_count(&state), 0);
        assert!(!state.history.can_redo());
    }

    #[test]
    fn undo_and_redo_restore_scope_channels_styles_axes_lanes_and_plot_names() {
        let (mut state, mut platform, ctx) = test_state();
        state.add_widget_to_tree(widgets::Widget::Scope(Box::new(Scope::new_multi(
            1,
            "Drive",
            vec![
                (
                    "gps_speed_heading.gps_speed".to_owned(),
                    "gps_speed_heading.gps_speed".to_owned(),
                    "m/s".to_owned(),
                ),
                (
                    "motor.rpm".to_owned(),
                    "motor.rpm".to_owned(),
                    "rpm".to_owned(),
                ),
            ],
        ))));
        state.reset_undo_history();

        // Mutate Scope styles, lanes, plot names, discrete flag, hidden channels, and lane Y override
        {
            let scope = first_scope_mut(&mut state);
            scope.config.stacked = true;
            scope.config.lanes = 2;
            scope.config.plot_names.insert(0, "Velocity".to_owned());
            scope.config.plot_names.insert(1, "Motor".to_owned());
            scope.config.channels.insert(
                "gps_speed_heading.gps_speed".to_owned(),
                SignalStyle {
                    width: 3.5,
                    opacity: 0.6,
                    mode: TraceMode::Steps,
                    points: true,
                    lane: 0,
                    ..Default::default()
                },
            );
            let ch_rpm = scope.channels()[1].clone();
            scope.config.assign(&ch_rpm, 1, Destination::Discrete);
            scope.config.axes.insert(
                "0|m/s".to_owned(),
                AxisConfig {
                    range: Some([0.0, 40.0]),
                    side: AxisSide::Right,
                    color: Some([220, 40, 40]),
                    label: "Speed".to_owned(),
                },
            );
            scope.set_hidden_channels(["gps_speed_heading.gps_speed".to_owned()].into());
            scope.set_lane_y_overrides([(0, (-5.0, 50.0))].into());
            scope.show_inline_stats = true;
        }
        run_frame(&mut state, &mut platform, &ctx, 1.0, vec![]);
        assert!(state.history.can_undo());

        // Undo restores initial unstacked Scope without custom styles/overrides
        state.action_queue.push(AppAction::Undo);
        run_frame(&mut state, &mut platform, &ctx, 2.0, vec![]);
        {
            let scope = first_scope(&state);
            assert!(!scope.config.stacked);
            assert!(
                scope.config.plot_names.values().all(String::is_empty),
                "Custom plot names should be cleared after Undo"
            );
            assert!(scope.hidden_channels().is_empty());
            assert!(scope.lane_y_overrides().is_empty());
            assert!(!scope.show_inline_stats);
        }

        // Redo brings all Scope configuration & session extras back
        state.action_queue.push(AppAction::Redo);
        run_frame(&mut state, &mut platform, &ctx, 3.0, vec![]);
        {
            let scope = first_scope(&state);
            assert!(scope.config.stacked);
            assert_eq!(scope.config.lanes, 2);
            assert_eq!(
                scope.config.plot_names.get(&0).map(String::as_str),
                Some("Velocity")
            );
            assert_eq!(
                scope.config.style("gps_speed_heading.gps_speed").mode,
                TraceMode::Steps
            );
            assert!(scope.config.style("motor.rpm").discrete);
            assert_eq!(scope.config.axes["0|m/s"].range, Some([0.0, 40.0]));
            assert!(
                scope
                    .hidden_channels()
                    .contains("gps_speed_heading.gps_speed")
            );
            assert_eq!(scope.lane_y_overrides().get(&0), Some(&(-5.0, 50.0)));
            assert!(scope.show_inline_stats);
        }
    }

    #[test]
    fn continuous_pointer_drag_coalesces_into_single_undo_step() {
        let (mut state, mut platform, ctx) = test_state();
        state.add_widget_to_tree(widgets::Widget::Scope(Box::new(Scope::new_channel(
            1,
            "motor.rpm".to_owned(),
            "motor.rpm".to_owned(),
            "rpm".to_owned(),
        ))));
        state.reset_undo_history();
        assert_eq!(state.history.len(), 1);

        // Press pointer down and mutate width across 10 consecutive frames
        let press_pos = Pos2::new(10.0, 10.0);
        for step in 0..10 {
            first_scope_mut(&mut state)
                .config
                .channels
                .entry("motor.rpm".to_owned())
                .or_default()
                .width = 1.5 + step as f32 * 0.25;
            let events = if step == 0 {
                vec![Event::PointerButton {
                    pos: press_pos,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                }]
            } else {
                vec![Event::PointerMoved(
                    press_pos + egui::vec2(step as f32, 0.0),
                )]
            };
            run_frame(
                &mut state,
                &mut platform,
                &ctx,
                1.0 + step as f64 * 0.016,
                events,
            );
            // Still deferred while pointer is held down
            assert_eq!(state.history.len(), 1);
        }

        // Release pointer -> commits exactly 1 undo step for the whole drag
        run_frame(
            &mut state,
            &mut platform,
            &ctx,
            2.0,
            vec![Event::PointerButton {
                pos: press_pos,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            }],
        );
        assert_eq!(state.history.len(), 2);

        // Single undo returns to default width 1.5
        state.action_queue.push(AppAction::Undo);
        run_frame(&mut state, &mut platform, &ctx, 3.0, vec![]);
        assert_eq!(first_scope(&state).config.style("motor.rpm").width, 1.5);
    }

    #[test]
    fn cross_scope_signal_move_commits_atomically_as_one_undo_step() {
        let (mut state, mut platform, ctx) = test_state();
        let scope_a = Scope::new_channel(
            1,
            "motor.rpm".to_owned(),
            "motor.rpm".to_owned(),
            "rpm".to_owned(),
        );
        let scope_b = Scope::new_empty(2);
        let mut tiles = egui_tiles::Tiles::default();
        let a_id = tiles.insert_pane(widgets::Widget::Scope(Box::new(scope_a)));
        let b_id = tiles.insert_pane(widgets::Widget::Scope(Box::new(scope_b)));
        let root_id = tiles.insert_horizontal_tile(vec![a_id, b_id]);
        state.tile_tree = egui_tiles::Tree::new("workspace_tree", root_id, tiles);
        state.reset_undo_history();

        // Simulate Frame N: Scope 2 accepts the signal from Scope 1 and queues removal on Scope 1
        let drag_payload = {
            let scope1 = state
                .tile_tree
                .tiles
                .tiles()
                .find_map(|t| match t {
                    egui_tiles::Tile::Pane(widgets::Widget::Scope(s))
                        if !s.channels().is_empty() =>
                    {
                        Some(s.as_ref())
                    }
                    _ => None,
                })
                .expect("scope 1 with channel");
            crate::ui::scope_legend::SignalDrag::new(1, &scope1.channels()[0], &scope1.config)
        };
        {
            let target = state
                .tile_tree
                .tiles
                .tiles_mut()
                .find_map(|t| match t {
                    egui_tiles::Tile::Pane(widgets::Widget::Scope(s))
                        if s.channels().is_empty() =>
                    {
                        Some(s.as_mut())
                    }
                    _ => None,
                })
                .expect("empty scope 2");
            target.accept_scope_signal(&ctx, drag_payload, 0, Destination::Right);
        }
        assert!(Scope::has_pending_signal_moves(&ctx));

        // Frame N end: sync_after_frame MUST defer because Scope 1 hasn't drained removal yet
        state.history.sync_after_frame(
            &state.tile_tree,
            &state.router,
            state.active_workspace_tab.clone(),
            &ctx,
        );
        assert_eq!(
            state.history.len(),
            1,
            "Half-moved intermediate frame must not be recorded"
        );

        // Frame N+1: Scope 1 processes pending removal; now history commits ONE atomic entry
        run_frame(&mut state, &mut platform, &ctx, 1.0, vec![]);
        assert!(!Scope::has_pending_signal_moves(&ctx));
        assert_eq!(state.history.len(), 2);

        // Undo returns signal to Scope 1 and empties Scope 2 in a single step
        state.action_queue.push(AppAction::Undo);
        run_frame(&mut state, &mut platform, &ctx, 2.0, vec![]);
        let counts: Vec<usize> = state
            .tile_tree
            .tiles
            .tiles()
            .filter_map(|t| match t {
                egui_tiles::Tile::Pane(widgets::Widget::Scope(s)) => Some(s.channels().len()),
                _ => None,
            })
            .collect();
        assert!(counts.contains(&1) && counts.contains(&0));
    }

    #[test]
    fn redo_stack_clears_on_new_edit_and_respects_max_history_cap() {
        let (mut state, mut platform, ctx) = test_state();
        state.add_widget_to_tree(widgets::Widget::Scope(Box::new(Scope::new_empty(1))));
        state.reset_undo_history();

        // Push MAX_HISTORY_LEN + 15 distinct edits
        for i in 0..(MAX_HISTORY_LEN + 15) {
            first_scope_mut(&mut state)
                .config
                .plot_names
                .insert(0, format!("Plot Name {i}"));
            run_frame(&mut state, &mut platform, &ctx, 1.0 + i as f64, vec![]);
        }
        assert_eq!(state.history.len(), MAX_HISTORY_LEN);
        assert_eq!(state.history.cursor(), MAX_HISTORY_LEN - 1);

        // Undo twice, then make a new edit -> redo tail must be truncated
        state.action_queue.push(AppAction::Undo);
        run_frame(&mut state, &mut platform, &ctx, 500.0, vec![]);
        state.action_queue.push(AppAction::Undo);
        run_frame(&mut state, &mut platform, &ctx, 501.0, vec![]);
        assert!(state.history.can_redo());

        first_scope_mut(&mut state)
            .config
            .plot_names
            .insert(0, "Branched Edit".to_owned());
        run_frame(&mut state, &mut platform, &ctx, 502.0, vec![]);
        assert!(!state.history.can_redo());
    }

    #[test]
    fn idle_frames_and_timeline_scrubbing_do_not_create_phantom_undo_entries() {
        let (mut state, mut platform, ctx) = test_state();
        state.action_queue.push(AppAction::LoadWorkspacePreset {
            preset_id: "powertrain_accumulator".to_owned(),
            apply_time_context: false,
        });
        run_frame(&mut state, &mut platform, &ctx, 0.5, vec![]);
        state.reset_undo_history();

        for i in 0..20 {
            state.action_queue.push(AppAction::StepPlayhead {
                direction: 1,
                fine: false,
            });
            state
                .action_queue
                .push(AppAction::PanWindow { direction: 1 });
            state
                .action_queue
                .push(AppAction::ZoomWindow { factor: 0.95 });
            run_frame(
                &mut state,
                &mut platform,
                &ctx,
                1.0 + i as f64 * 0.05,
                vec![],
            );
        }
        assert_eq!(state.history.len(), 1);
        assert!(!state.history.can_undo());
    }

    #[test]
    fn custom_formula_create_edit_delete_roundtrips_through_undo_redo() {
        let (mut state, mut platform, ctx) = test_state();
        state.reset_undo_history();

        let custom = DerivedSignalDef {
            id: "custom.test_power".to_owned(),
            label: "Test Power".to_owned(),
            unit: "W".to_owned(),
            expression: "pack_stats.pack_voltage * pack_stats.pack_current".to_owned(),
            updated_at_ms: 0,
            deleted: false,
        };
        state
            .router
            .save_derived_signal(custom.clone())
            .expect("save formula");
        run_frame(&mut state, &mut platform, &ctx, 1.0, vec![]);
        assert!(
            state
                .router
                .formula_engine()
                .get("custom.test_power")
                .is_some()
        );
        assert!(state.history.can_undo());

        // Undo deletes the custom formula
        state.action_queue.push(AppAction::Undo);
        run_frame(&mut state, &mut platform, &ctx, 2.0, vec![]);
        assert!(
            state
                .router
                .formula_engine()
                .get("custom.test_power")
                .is_none()
        );

        // Redo restores the custom formula
        state.action_queue.push(AppAction::Redo);
        run_frame(&mut state, &mut platform, &ctx, 3.0, vec![]);
        let restored = state
            .router
            .formula_engine()
            .get("custom.test_power")
            .expect("restored formula");
        assert_eq!(restored.def.expression, custom.expression);
        assert_eq!(restored.def.unit, "W");
    }

    struct StatefulMockPlatformWidget {
        title: String,
        instance_token: usize,
    }

    impl PlatformWidget for StatefulMockPlatformWidget {
        fn kind(&self) -> WidgetKind {
            WidgetKind::LogParser
        }
        fn title(&self) -> &str {
            &self.title
        }
        fn set_title(&mut self, new_title: String) {
            self.title = new_title;
        }
        fn show(
            &mut self,
            ui: &mut egui::Ui,
            _context: widgets::WidgetContext<'_>,
        ) -> egui_tiles::UiResponse {
            ui.label(&self.title);
            egui_tiles::UiResponse::None
        }
        fn capture_spec(&self) -> workspace_io::PaneSpec {
            workspace_io::PaneSpec::LogParser {
                title: self.title.clone(),
            }
        }
        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }
    }

    struct MockHardwarePlatform {
        next_token: usize,
    }

    impl ViewerPlatform for MockHardwarePlatform {
        fn supports_hardware_widgets(&self) -> bool {
            true
        }
        fn instantiate_platform_pane(
            &mut self,
            spec: &workspace_io::PaneSpec,
            _widget_ids: &mut widget_ids::WidgetIds,
        ) -> Option<Box<dyn PlatformWidget>> {
            match spec {
                workspace_io::PaneSpec::LogParser { title } => {
                    self.next_token += 1;
                    Some(Box::new(StatefulMockPlatformWidget {
                        title: title.clone(),
                        instance_token: self.next_token,
                    }))
                }
                _ => None,
            }
        }
    }

    #[test]
    fn platform_widget_instances_are_preserved_across_undo_redo() {
        let (mut state, _, ctx) = test_state();
        let mut platform = MockHardwarePlatform { next_token: 10 };
        state.add_widget_to_tree(widgets::Widget::Platform(Box::new(
            StatefulMockPlatformWidget {
                title: "SD Importer".to_owned(),
                instance_token: 42,
            },
        )));
        state.add_widget_to_tree(widgets::Widget::Scope(Box::new(Scope::new_empty(1))));
        state.reset_undo_history();

        // Make an unrelated edit on the Scope
        first_scope_mut(&mut state)
            .config
            .plot_names
            .insert(0, "Edited".to_owned());
        run_frame(&mut state, &mut platform, &ctx, 1.0, vec![]);

        // Undo should rebuild the tile tree while moving the SAME PlatformWidget (token 42) intact
        state.action_queue.push(AppAction::Undo);
        run_frame(&mut state, &mut platform, &ctx, 2.0, vec![]);

        let token = state
            .tile_tree
            .tiles
            .tiles_mut()
            .find_map(|tile| match tile {
                egui_tiles::Tile::Pane(widgets::Widget::Platform(w)) => w
                    .as_any_mut()
                    .downcast_mut::<StatefulMockPlatformWidget>()
                    .map(|m| m.instance_token),
                _ => None,
            })
            .expect("platform widget preserved");
        assert_eq!(token, 42, "Live PlatformWidget must not be recreated");
    }

    #[test]
    fn shortcuts_trigger_undo_redo_only_when_text_input_not_focused() {
        let ctx = egui::Context::default();

        let key_input = |key: Key, shift: bool| {
            let modifiers = Modifiers {
                command: true,
                ctrl: true,
                shift,
                ..Default::default()
            };
            egui::RawInput {
                modifiers,
                events: vec![Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers,
                }],
                ..Default::default()
            }
        };

        // 1. No text focus: Ctrl+Z -> Undo, Ctrl+Shift+Z -> Redo, Ctrl+Y -> Redo
        let mut captured = Vec::new();
        let _ = ctx.run(key_input(Key::Z, false), |ctx| {
            captured = ShortcutHandler::check_shortcuts(ctx);
        });
        assert!(captured.iter().any(|a| matches!(a, AppAction::Undo)));

        let _ = ctx.run(key_input(Key::Z, true), |ctx| {
            captured = ShortcutHandler::check_shortcuts(ctx);
        });
        assert!(captured.iter().any(|a| matches!(a, AppAction::Redo)));

        let _ = ctx.run(key_input(Key::Y, false), |ctx| {
            captured = ShortcutHandler::check_shortcuts(ctx);
        });
        assert!(captured.iter().any(|a| matches!(a, AppAction::Redo)));

        // 2. Focused TextEdit: Ctrl+Z must NOT emit global AppAction::Undo
        let mut text = "hello".to_owned();
        let edit_id = egui::Id::new("focused_text_edit");
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let resp = ui.add(egui::TextEdit::singleline(&mut text).id(edit_id));
                resp.request_focus();
            });
        });
        let _ = ctx.run(key_input(Key::Z, false), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.add(egui::TextEdit::singleline(&mut text).id(edit_id));
            });
            captured = ShortcutHandler::check_shortcuts(ctx);
        });
        assert!(
            !captured
                .iter()
                .any(|a| matches!(a, AppAction::Undo | AppAction::Redo)),
            "Focused TextEdit must retain native Ctrl+Z"
        );
    }

    #[test]
    fn closing_tab_in_powertrain_preset_undoes_and_redoes_cleanly() {
        let (mut state, mut platform, ctx) = test_state();
        run_frame(&mut state, &mut platform, &ctx, 1.0, Vec::new());

        // 1. Load Powertrain / Accumulator preset
        state.action_queue.push(AppAction::LoadWorkspacePreset {
            preset_id: "powertrain_accumulator".to_owned(),
            apply_time_context: false,
        });
        run_frame(&mut state, &mut platform, &ctx, 1.1, Vec::new());
        // Run an additional frame so `tile_tree.ui()` renders the loaded preset
        run_frame(&mut state, &mut platform, &ctx, 1.2, Vec::new());

        let has_pane_titled = |state: &ViewerState, title: &str| {
            state
                .tile_tree
                .tiles
                .tiles()
                .any(|tile| matches!(tile, egui_tiles::Tile::Pane(w) if w.title() == title))
        };

        assert!(
            has_pane_titled(&state, "Battery Power and Current"),
            "Powertrain preset must contain 'Battery Power and Current'"
        );
        let initial_cursor = state.history.cursor;

        // 2. Simulate closing 'Battery Power and Current' via its tab × button
        let battery_tile_id = state
            .tile_tree
            .tiles
            .iter()
            .find_map(|(id, tile)| match tile {
                egui_tiles::Tile::Pane(w) if w.title() == "Battery Power and Current" => Some(*id),
                _ => None,
            })
            .expect("find 'Battery Power and Current' tile id");
        state.tile_tree.tiles.remove(battery_tile_id);
        run_frame(&mut state, &mut platform, &ctx, 1.3, Vec::new());

        assert!(
            !has_pane_titled(&state, "Battery Power and Current"),
            "Tab must be removed after closing"
        );
        assert_eq!(
            state.history.cursor,
            initial_cursor + 1,
            "Closing the tab must record exactly one undo step"
        );
        assert!(state.history.can_undo());

        // 3. Undo once — 'Battery Power and Current' must immediately reappear
        //    and Redo must remain available across subsequent frames.
        state.action_queue.push(AppAction::Undo);
        run_frame(&mut state, &mut platform, &ctx, 1.4, Vec::new());
        run_frame(&mut state, &mut platform, &ctx, 1.5, Vec::new());

        assert!(
            has_pane_titled(&state, "Battery Power and Current"),
            "Undo must restore 'Battery Power and Current'"
        );
        assert!(
            state.history.can_redo(),
            "Redo must remain available after Undo renders on subsequent frames"
        );

        // 4. Redo — 'Battery Power and Current' closes again
        state.action_queue.push(AppAction::Redo);
        run_frame(&mut state, &mut platform, &ctx, 1.6, Vec::new());
        run_frame(&mut state, &mut platform, &ctx, 1.7, Vec::new());

        assert!(
            !has_pane_titled(&state, "Battery Power and Current"),
            "Redo must re-close 'Battery Power and Current'"
        );
        assert!(state.history.can_undo());
    }
}
