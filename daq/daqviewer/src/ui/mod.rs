pub mod bus_load;
pub mod cloud_stats;
pub mod command_palette;
pub mod export_modal;
pub mod gps_plot;
pub mod jitter;
pub mod message_picker;
pub mod scope;
mod scope_icons;
mod scope_interaction;
mod scope_legend;
pub mod scope_settings;
pub mod sidebar;
pub mod signal_browser;
pub mod state_lane;
pub mod theme;
pub mod timeline;
pub mod viewer_list;
pub mod viewer_table;
pub mod xy_scatter;

/// Smooth wheel/trackpad zoom factor (`< 1.0` zooms in, `> 1.0` zooms out).
pub(crate) fn scroll_zoom_factor(delta: f32) -> f64 {
    2.0_f64.powf((-f64::from(delta) / 240.0).clamp(-1.0, 1.0))
}
