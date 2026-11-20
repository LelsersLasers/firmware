pub mod cloud_backend;
pub mod platform;

use cloud_backend::web_cloud_router;
use daqcore::time::Time;
use daqviewer::{
    app::{ViewerApp, ViewerState},
    load_embedded_formatter,
    workspace_io::WorkspaceView,
};
use platform::{BrowserEnv, WebPlatform};
use std::sync::Arc;

/// Construct a fully initialized [`ViewerApp<WebPlatform<B>>`] connected to `daqcloud` and
/// hydrated from either the browser's `/daqview/<payload>` URL, `localStorage`, or the default preset.
pub fn build_web_viewer_app<B: BrowserEnv>(
    browser: B,
    egui_ctx: Option<&eframe::egui::Context>,
) -> ViewerApp<WebPlatform<B>> {
    let mut platform = WebPlatform::new(browser);
    let settings = platform.load_settings();

    let origin = platform.browser().origin_url();
    let cloud_url = if !origin.trim().is_empty() {
        origin.trim().trim_end_matches('/').to_owned()
    } else {
        settings.cloud_url.trim().trim_end_matches('/').to_owned()
    };

    let mut router = web_cloud_router(Some(cloud_url.clone()), Time::now(), 30.0)
        .expect("Failed to initialize WebCloudRouterBackend");
    router
        .timeline_mut()
        .set_collapse_idle_gaps(settings.collapse_idle_gaps);

    if let Some(ctx) = egui_ctx {
        egui_extras::install_image_loaders(ctx);
        daqviewer::ui::theme::store_theme(ctx, settings.theme.get_colors());
        let ctx_clone = ctx.clone();
        router.set_repaint_callback(Some(Arc::new(move || {
            ctx_clone.request_repaint();
        })));
    }

    let mut state = ViewerState::new(
        router,
        settings.theme,
        settings.pixels_per_point,
        cloud_url,
        true,
        None,
        load_embedded_formatter(),
    );
    state.workspace_view_modal.save_file_path = settings.daqview_file_path;

    let initial_path = platform.browser().pathname();
    let mut applied_layout = false;

    if initial_path.contains("/daqview/") || initial_path.trim().starts_with("daqview1:") {
        match WorkspaceView::from_str_flexible(&initial_path) {
            Ok(view) => {
                if let Err(err) = view.apply_to_state(&mut state, &mut platform, true) {
                    state.diagnostic =
                        Some(format!("Failed to apply /daqview/ workspace URL: {err}"));
                } else {
                    state.active_workspace_tab = view.id.clone();
                    platform.set_pending_initial_time_context(view.context);
                    applied_layout = true;
                }
            }
            Err(err) => {
                state.diagnostic = Some(format!("Invalid /daqview/ URL payload: {err}"));
            }
        }
    }

    if !applied_layout
        && let Some(saved_share) = settings.last_workspace_share.as_deref()
        && let Ok(view) = WorkspaceView::from_str_flexible(saved_share)
        && view
            .apply_to_state(&mut state, &mut platform, false)
            .is_ok()
    {
        state.active_workspace_tab = settings.active_workspace_tab.clone();
        applied_layout = true;
    }

    if !applied_layout {
        let default_id = settings
            .active_workspace_tab
            .as_deref()
            .unwrap_or("vd_handling_suspension");
        if let Some(preset) = state
            .workspace_presets
            .get(default_id)
            .or_else(|| state.workspace_presets.presets().first())
            .cloned()
        {
            let _ = preset.apply_to_state(&mut state, &mut platform, false);
            state.active_workspace_tab = preset.id;
        }
    }

    state.reset_undo_history();
    let _ = platform.sync_browser_url(&state);
    ViewerApp::new(state, platform)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{MemoryBrowserEnv, WEB_SETTINGS_STORAGE_KEY};
    use daqcore::analysis::DerivedSignalDef;
    use daqviewer::{
        action::AppAction,
        app::ViewerPlatform,
        ui::scope::ScopeXAxisMode,
        workspace_io::{
            FlexibleLayout, LinearSplitDir, PaneSpec, ScopeChannelSpec, WorkspaceLayoutNode,
            WorkspaceTimeContext,
        },
    };

    #[test]
    fn daqweb_restores_workspace_and_timeline_from_daqview_url_and_syncs_url_bar() {
        let base_ms = 1_794_800_000_000i64;
        let custom_view = WorkspaceView {
            id: Some("custom_shared_view".to_owned()),
            schema_version: 1,
            name: "Custom Shared Web View".to_owned(),
            description: "Shared via /daqview/ URL".to_owned(),
            context: Some(WorkspaceTimeContext {
                track_day_id: Some("2026-11-16".to_owned()),
                viewport_start_utc_ms: base_ms + 500,
                viewport_end_utc_ms: base_ms + 3_500,
                setpoint_utc_ms: Some(base_ms + 2_000),
                compare_annotation_ids: vec!["ann-web-stint-1".to_owned()],
                gap_folding_enabled: true,
            }),
            custom_formulas: vec![DerivedSignalDef {
                id: "custom.double_fl_speed".to_owned(),
                label: "Double FL Speed".to_owned(),
                expression: "wheel_speeds.front_left * 2.0".to_owned(),
                unit: "m/s".to_owned(),
                ..Default::default()
            }],
            layout: Some(FlexibleLayout::Tree(WorkspaceLayoutNode::Linear {
                dir: LinearSplitDir::Horizontal,
                shares: vec![0.6, 0.4],
                children: vec![
                    WorkspaceLayoutNode::Pane {
                        spec: PaneSpec::Scope {
                            title: "Restored Web YT Plot".to_owned(),
                            custom_title: true,
                            x_mode: ScopeXAxisMode::default(),
                            speed_signal_id: "wheel_speeds.front_left".to_owned(),
                            show_legend: true,
                            plot: Default::default(),
                            y_bounds: None,
                            channels: vec![ScopeChannelSpec {
                                signal_id: "custom.double_fl_speed".to_owned(),
                                label: "2x FL Speed".to_owned(),
                                unit: "m/s".to_owned(),
                                color_rgb: None,
                            }],
                            axes: Vec::new(),
                        },
                    },
                    WorkspaceLayoutNode::Pane {
                        spec: PaneSpec::XYScatter {
                            title: "Restored XY Scatter".to_owned(),
                            custom_title: true,
                            x_signal: "wheel_speeds.front_left".to_owned(),
                            y_signal: "IMU_acceleration.X_axis".to_owned(),
                            z_color_signal: None,
                            equal_aspect: false,
                            show_g_rings: false,
                        },
                    },
                ],
            })),
            panes: Default::default(),
        };

        let share_str = custom_view.to_share_string().expect("encode share string");
        let browser = MemoryBrowserEnv::new("", format!("/daqview/{share_str}"));
        let egui_ctx = eframe::egui::Context::default();
        let mut app = build_web_viewer_app(browser.clone(), Some(&egui_ctx));

        assert!(!app.platform.supports_hardware_widgets());
        assert!(!app.platform.supports_native_file_dialogs());
        assert!(
            app.state
                .router
                .formula_engine()
                .get("custom.double_fl_speed")
                .is_some()
        );
        assert_eq!(app.state.router.timeline().start_utc_ms(), base_ms + 500);
        assert_eq!(app.state.router.timeline().end_utc_ms(), base_ms + 3_500);
        assert_eq!(
            app.state.router.timeline().setpoint_utc_ms(),
            base_ms + 2_000
        );

        // Switching to a built-in preset updates the browser address bar `/daqview/daqview1:...`
        // and persists settings to `localStorage`.
        let prev_path = browser.pathname();
        app.state.handle_action(
            AppAction::LoadWorkspacePreset {
                preset_id: "powertrain_accumulator".to_owned(),
                apply_time_context: false,
            },
            &mut app.platform,
            &egui_ctx,
        );
        let updated_path = browser.pathname();
        assert!(updated_path.starts_with("/daqview/daqview1:"));
        assert_ne!(updated_path, prev_path);
        assert!(browser.load_storage(WEB_SETTINGS_STORAGE_KEY).is_some());

        // Saving `.daqview.json` triggers a browser download.
        app.platform
            .save_daqview_content("/tmp/my_layout.daqview.json", "{\"schema_version\":1}\n")
            .expect("save daqview content");
        let downloads = browser.downloads();
        assert_eq!(downloads.len(), 1);
        assert_eq!(downloads[0].filename, "my_layout.daqview.json");
        assert_eq!(downloads[0].mime_type, "application/json");
    }
}
