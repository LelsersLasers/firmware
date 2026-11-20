use crate::{action, app, ui, workspace_view};
use daqcore::{
    router::{TileInFlightSummary, VirtualTimelineRouter},
    storage::{
        BackendLoadingActivity, CloudCacheStats, CloudMemoryStats, CloudStatsResponse,
        CloudStatsSnapshot, CloudSyncStatus, EndpointLatencyStats,
    },
};
use eframe::egui::{self, Color32, RichText, Stroke};

/// Aggregated summary of all in-flight data requests and background loading tasks in the viewer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ViewerLoadingSummary {
    pub tiles: TileInFlightSummary,
    pub backend: BackendLoadingActivity,
    pub cloud_connecting: bool,
    pub database_loading: bool,
    pub platform_requests: usize,
}

impl ViewerLoadingSummary {
    pub fn from_state(
        router: &VirtualTimelineRouter,
        selection_pending: bool,
        platform_requests: usize,
    ) -> Self {
        let tiles = router.tile_cache().in_flight_summary();
        let backend = router.backend_loading_activity();
        let cloud_connecting = matches!(router.cloud_status(), CloudSyncStatus::Connecting { .. });
        Self {
            tiles,
            backend,
            cloud_connecting,
            database_loading: selection_pending,
            platform_requests,
        }
    }

    /// Number of user-facing data/sync requests currently waiting (excluding background stats polling).
    pub fn active_data_requests(&self) -> usize {
        self.tiles.total()
            + self.backend.in_flight_preloads
            + self.backend.pending_downloads
            + self.backend.pending_uploads
            + usize::from(self.backend.catalog_syncing)
            + usize::from(self.backend.metadata_syncing)
            + usize::from(self.cloud_connecting)
            + usize::from(self.database_loading)
            + self.platform_requests
    }

    pub fn is_active(&self) -> bool {
        self.active_data_requests() > 0
    }

    pub fn badge_label(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        let tile_total = self.tiles.total();
        if tile_total > 0 {
            if self.tiles.full_res_count == tile_total {
                parts.push(format!(
                    "{} full-res {}",
                    tile_total,
                    if tile_total == 1 { "tile" } else { "tiles" }
                ));
            } else {
                parts.push(format!(
                    "{} {}",
                    tile_total,
                    if tile_total == 1 { "tile" } else { "tiles" }
                ));
            }
        }
        if self.backend.in_flight_preloads > 0 {
            let n = self.backend.in_flight_preloads;
            parts.push(format!(
                "{n} {}",
                if n == 1 { "preload" } else { "preloads" }
            ));
        }
        if self.backend.pending_downloads > 0 {
            let n = self.backend.pending_downloads;
            parts.push(format!("dl:{n}"));
        }
        if self.backend.pending_uploads > 0 {
            let n = self.backend.pending_uploads;
            parts.push(format!("ul:{n}"));
        }
        if self.backend.catalog_syncing || self.cloud_connecting {
            parts.push("catalog".to_owned());
        } else if self.backend.metadata_syncing {
            parts.push("sync".to_owned());
        }
        if self.platform_requests > 0 {
            parts.push("export".to_owned());
        }
        if self.database_loading {
            parts.push("SuperDBC".to_owned());
        }
        if parts.is_empty() {
            "Ready".to_owned()
        } else {
            format!("Loading {}", parts.join(" · "))
        }
    }

    pub fn tooltip_breakdown(&self, router: &VirtualTimelineRouter) -> String {
        let mut lines = Vec::new();
        let active = self.active_data_requests();
        if active == 0 {
            lines.push("All viewer data requests & background queues are idle.".to_owned());
        } else {
            lines.push(format!("Active waiting requests / tasks: {active}"));
        }

        if self.tiles.total() > 0 {
            lines.push(format!(
                "• Signal Tiles In-Flight: {} (Full-Res: {}, 10Hz: {}, 1Hz: {})",
                self.tiles.total(),
                self.tiles.full_res_count,
                self.tiles.overview_10hz_count,
                self.tiles.overview_1hz_count
            ));
            if !self.tiles.sample_signals.is_empty() {
                lines.push(format!(
                    "  Signals: {}",
                    self.tiles.sample_signals.join(", ")
                ));
            }
        }
        if self.backend.in_flight_preloads > 0 {
            lines.push(format!(
                "• Partition 1Hz Preloads Waiting: {}",
                self.backend.in_flight_preloads
            ));
        }
        if self.backend.pending_downloads > 0 {
            lines.push(format!(
                "• Full Partition Downloads Waiting: {}",
                self.backend.pending_downloads
            ));
        }
        if self.backend.pending_uploads > 0 {
            lines.push(format!(
                "• Local Partition Uploads Waiting: {}",
                self.backend.pending_uploads
            ));
        }
        if self.backend.catalog_syncing || self.cloud_connecting {
            lines.push("• daqcloud Catalog Sync: In Flight".to_owned());
        }
        if self.backend.metadata_syncing {
            lines.push("• Annotations / Derived Formulas Poll: In Flight".to_owned());
        }
        if self.platform_requests > 0 {
            lines.push(format!(
                "• Dataset Exports Waiting: {}",
                self.platform_requests
            ));
        }
        if self.database_loading {
            lines.push("• SuperDBC Database Selection: Loading".to_owned());
        }

        let tc = router.tile_cache();
        let lru_mib = tc.lru_bytes() as f64 / (1024.0 * 1024.0);
        let max_mib = tc.max_lru_bytes() as f64 / (1024.0 * 1024.0);
        lines.push(format!(
            "\nLocal TileCache: {:.1} / {:.0} MiB ({} tiles, {} preloaded partitions)",
            lru_mib,
            max_mib,
            tc.lru_len(),
            tc.preloaded_partition_count()
        ));
        lines.push("Click to open the daqcloud Stats Dashboard.".to_owned());
        lines.join("\n")
    }
}

/// Draw an animated spinner icon inside `rect`.
pub fn paint_animated_spinner(ui: &egui::Ui, rect: egui::Rect, color: Color32) {
    let time = ui.ctx().input(|i| i.time);
    let center = rect.center();
    let radius = (rect.width().min(rect.height()) * 0.38).clamp(3.5, 7.0);
    let base_angle = (time * std::f64::consts::TAU * 1.35) as f32;

    ui.painter().circle_stroke(
        center,
        radius,
        Stroke::new(1.3_f32, color.gamma_multiply(0.25)),
    );

    let segments = 10usize;
    let arc_span = std::f32::consts::PI * 1.15;
    let mut pts = Vec::with_capacity(segments + 1);
    for i in 0..=segments {
        let t = i as f32 / segments as f32;
        let a = base_angle + t * arc_span;
        pts.push(center + egui::vec2(a.cos() * radius, a.sin() * radius));
    }
    ui.painter()
        .add(egui::Shape::line(pts, Stroke::new(1.8_f32, color)));
    ui.ctx().request_repaint();
}

/// Render an interactive animated loading status pill when requests are waiting or data is loading.
/// Returns `true` if clicked (e.g. to open the `daqcloud` Stats Dashboard).
pub fn show_loading_activity_pill(
    ui: &mut egui::Ui,
    router: &VirtualTimelineRouter,
    selection_pending: bool,
    platform_requests: usize,
    show_when_idle: bool,
) -> bool {
    let summary = ViewerLoadingSummary::from_state(router, selection_pending, platform_requests);
    let is_active = summary.is_active();
    if !is_active && !show_when_idle {
        return false;
    }

    let theme = ui::theme::get_theme(ui.ctx());
    let (pill_color, fill_alpha, label) = if is_active {
        (
            Color32::from_rgb(56, 189, 248),
            0.18_f32,
            format!("    {}", summary.badge_label()),
        )
    } else {
        (
            theme.muted_text_color(),
            0.08_f32,
            "☁ Cloud Stats".to_owned(),
        )
    };

    let btn = egui::Button::new(
        RichText::new(label)
            .size(10.0)
            .strong()
            .color(if is_active {
                ui.visuals().strong_text_color()
            } else {
                pill_color
            }),
    )
    .fill(pill_color.gamma_multiply(fill_alpha))
    .stroke(Stroke::new(1.0_f32, pill_color.gamma_multiply(0.65)))
    .corner_radius(3.0);

    let resp = ui
        .add(btn)
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(summary.tooltip_breakdown(router));

    if is_active {
        let spinner_rect = egui::Rect::from_center_size(
            egui::pos2(resp.rect.left() + 10.0, resp.rect.center().y),
            egui::vec2(12.0, 12.0),
        );
        paint_animated_spinner(ui, spinner_rect, pill_color);
    }

    resp.clicked()
}

pub fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= GIB {
        format!("{:.2} GiB", b / GIB)
    } else if b >= MIB {
        format!("{:.1} MiB", b / MIB)
    } else if b >= KIB {
        format!("{:.1} KiB", b / KIB)
    } else {
        format!("{bytes} B")
    }
}

pub fn format_uptime(secs: f64) -> String {
    let total = secs.max(0.0).round() as u64;
    let days = total / 86_400;
    let hours = (total % 86_400) / 3_600;
    let mins = (total % 3_600) / 60;
    let s = total % 60;
    if days > 0 {
        format!("{days}d {hours}h {mins}m")
    } else if hours > 0 {
        format!("{hours}h {mins}m {s}s")
    } else if mins > 0 {
        format!("{mins}m {s}s")
    } else {
        format!("{s}s")
    }
}

/// Dockable workspace widget pane for monitoring `daqcloud` response times, cache usage,
/// process RSS, VPS host memory, and store inventory.
pub struct CloudStats {
    pub title: String,
    pub poll_interval_secs: f32,
    last_poll_instant: Option<web_time::Instant>,
}

impl CloudStats {
    pub fn new(instance: usize) -> Self {
        let title = if instance <= 1 {
            "daqcloud Stats".to_owned()
        } else {
            format!("daqcloud Stats #{instance}")
        };
        Self {
            title,
            poll_interval_secs: 2.0,
            last_poll_instant: None,
        }
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        view: &workspace_view::WorkspaceDataView<'_>,
        action_queue: &mut Vec<action::AppAction>,
    ) -> egui_tiles::UiResponse {
        let cloud_status = view.router.cloud_status();
        let snapshot = view.router.cloud_stats_snapshot();
        let is_connected = matches!(
            cloud_status,
            CloudSyncStatus::Connected { .. } | CloudSyncStatus::Connecting { .. }
        );

        if is_connected && self.poll_interval_secs > 0.0 && !snapshot.is_loading {
            let should_poll = self
                .last_poll_instant
                .is_none_or(|t| t.elapsed().as_secs_f32() >= self.poll_interval_secs);
            if should_poll {
                self.last_poll_instant = Some(web_time::Instant::now());
                view.router.request_cloud_stats();
            }
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs_f32(
                    self.poll_interval_secs.max(0.5),
                ));
        }

        let theme = ui::theme::get_theme(ui.ctx());
        let muted_color = theme.muted_text_color();
        let border_color = theme.border_color();
        let accent_color = theme.accent_color();

        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);

        // 1. Top Control & Status Bar
        self.show_header_bar(
            ui,
            view.router,
            &cloud_status,
            &snapshot,
            action_queue,
            accent_color,
            muted_color,
            border_color,
        );
        ui.separator();

        // 2. Main Scrollable Dashboard Area
        egui::ScrollArea::vertical()
            .id_salt((&self.title, "cloud_stats_scroll"))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if let Some(err) = &snapshot.last_error {
                    egui::Frame::NONE
                        .fill(Color32::from_rgba_unmultiplied(239, 68, 68, 28))
                        .stroke(Stroke::new(1.0_f32, Color32::from_rgb(239, 68, 68)))
                        .corner_radius(4.0)
                        .inner_margin(egui::Margin::symmetric(8, 6))
                        .show(ui, |ui| {
                            ui.colored_label(
                                Color32::from_rgb(248, 113, 113),
                                format!("Failed to fetch /api/v1/stats: {err}"),
                            );
                        });
                }

                let Some(stats) = &snapshot.latest else {
                    self.show_empty_or_connecting_state(
                        ui,
                        view.router,
                        &cloud_status,
                        &snapshot,
                        action_queue,
                        muted_color,
                    );
                    return;
                };

                // Row A: 4 KPI Summary Cards
                self.show_kpi_cards_row(
                    ui,
                    stats,
                    view.router,
                    accent_color,
                    muted_color,
                    border_color,
                );

                ui.add_space(2.0);

                // Row B: Interactive Charts (Latency Distribution/Timeline + Memory & Cache History)
                self.show_charts_row(
                    ui,
                    stats,
                    &snapshot,
                    accent_color,
                    muted_color,
                    border_color,
                );

                ui.add_space(2.0);

                // Row C: Detailed Per-Endpoint & Cache/Store Tables
                self.show_tables_row(
                    ui,
                    stats,
                    view.router,
                    accent_color,
                    muted_color,
                    border_color,
                );
            });

        egui_tiles::UiResponse::None
    }

    #[allow(clippy::too_many_arguments)]
    fn show_header_bar(
        &mut self,
        ui: &mut egui::Ui,
        router: &VirtualTimelineRouter,
        cloud_status: &CloudSyncStatus,
        snapshot: &CloudStatsSnapshot,
        action_queue: &mut Vec<action::AppAction>,
        accent_color: Color32,
        muted_color: Color32,
        border_color: Color32,
    ) {
        ui.horizontal_wrapped(|ui| {
            let (status_text, status_color) = match cloud_status {
                CloudSyncStatus::Connected {
                    url, ws_connected, ..
                } => (
                    format!(
                        "✔ {} ({})",
                        url,
                        if *ws_connected { "WS Live" } else { "HTTP" }
                    ),
                    Color32::from_rgb(34, 197, 94),
                ),
                CloudSyncStatus::Connecting { url } => {
                    (format!("• Connecting to {url}…"), accent_color)
                }
                CloudSyncStatus::Error { url, .. } => {
                    (format!("🔴 Error ({url})"), Color32::from_rgb(239, 68, 68))
                }
                CloudSyncStatus::Disconnected => ("■ Disconnected".to_owned(), muted_color),
            };

            egui::Frame::NONE
                .fill(status_color.gamma_multiply(0.12))
                .stroke(Stroke::new(1.0_f32, status_color.gamma_multiply(0.5)))
                .corner_radius(3.0)
                .inner_margin(egui::Margin::symmetric(6, 2))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(status_text)
                            .size(11.0)
                            .strong()
                            .color(status_color),
                    );
                });

            if let Some(stats) = &snapshot.latest {
                ui.label(
                    RichText::new(format!(
                        "Uptime: {}  ·  WS Clients: {}  ·  Server In-Flight: {}",
                        format_uptime(stats.uptime_secs),
                        stats.active_ws_clients,
                        stats.in_flight_http_requests
                    ))
                    .size(11.0)
                    .color(muted_color),
                );
            }

            let loading = ViewerLoadingSummary::from_state(router, false, 0);
            if loading.is_active() {
                let _ = show_loading_activity_pill(ui, router, false, 0, false);
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        !snapshot.is_loading,
                        egui::Button::new(
                            RichText::new(if snapshot.is_loading {
                                "🔄 Fetching…"
                            } else {
                                "🔄 Refresh"
                            })
                            .size(11.0),
                        ),
                    )
                    .on_hover_text("Fetch latest /api/v1/stats snapshot from daqcloud now")
                    .clicked()
                {
                    self.last_poll_instant = Some(web_time::Instant::now());
                    router.request_cloud_stats();
                }

                let poll_label = if self.poll_interval_secs <= 0.0 {
                    "Paused".to_owned()
                } else {
                    format!("Every {:.0}s", self.poll_interval_secs)
                };
                egui::ComboBox::from_id_salt((&self.title, "poll_interval_combo"))
                    .width(84.0)
                    .selected_text(RichText::new(poll_label).size(11.0))
                    .show_ui(ui, |ui| {
                        for (secs, label) in [
                            (1.0_f32, "Every 1s"),
                            (2.0_f32, "Every 2s"),
                            (5.0_f32, "Every 5s"),
                            (10.0_f32, "Every 10s"),
                            (0.0_f32, "Paused"),
                        ] {
                            ui.selectable_value(&mut self.poll_interval_secs, secs, label);
                        }
                    });

                if matches!(cloud_status, CloudSyncStatus::Disconnected)
                    && ui
                        .small_button("☁ Connect Default")
                        .on_hover_text("Connect to default daqcloud server")
                        .clicked()
                {
                    action_queue.push(action::AppAction::ConnectCloud(
                        app::DEFAULT_CLOUD_URL.to_owned(),
                    ));
                }
            });
        });
        let _ = border_color;
    }

    fn show_empty_or_connecting_state(
        &mut self,
        ui: &mut egui::Ui,
        router: &VirtualTimelineRouter,
        cloud_status: &CloudSyncStatus,
        snapshot: &CloudStatsSnapshot,
        action_queue: &mut Vec<action::AppAction>,
        muted_color: Color32,
    ) {
        ui.vertical_centered(|ui| {
            ui.add_space(36.0);
            if snapshot.is_loading || matches!(cloud_status, CloudSyncStatus::Connecting { .. }) {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(24.0, 24.0), egui::Sense::hover());
                paint_animated_spinner(ui, rect, Color32::from_rgb(56, 189, 248));
                ui.add_space(6.0);
                ui.label(RichText::new("Fetching daqcloud server telemetry…").strong());
            } else if matches!(cloud_status, CloudSyncStatus::Connected { .. }) {
                ui.label(RichText::new("Connected to daqcloud — no stats snapshot yet.").strong());
                ui.add_space(4.0);
                if ui.button("🔄 Fetch daqcloud Stats Now").clicked() {
                    self.last_poll_instant = Some(web_time::Instant::now());
                    router.request_cloud_stats();
                }
            } else {
                ui.label(RichText::new("Not connected to a daqcloud server").strong());
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "Connect to a daqcloud instance to inspect live avg/p90 response times, Tile & Preload LRU cache usage, process RSS, and VPS memory.",
                    )
                    .small()
                    .color(muted_color),
                );
                ui.add_space(8.0);
                if ui
                    .button(format!("☁ Connect to {}", app::DEFAULT_CLOUD_URL))
                    .clicked()
                {
                    action_queue.push(action::AppAction::ConnectCloud(
                        app::DEFAULT_CLOUD_URL.to_owned(),
                    ));
                }
            }
        });
    }

    fn show_kpi_cards_row(
        &self,
        ui: &mut egui::Ui,
        stats: &CloudStatsResponse,
        router: &VirtualTimelineRouter,
        accent_color: Color32,
        muted_color: Color32,
        border_color: Color32,
    ) {
        let avail_w = (ui.available_width() - 4.0).max(220.0);
        let spacing = 6.0_f32;

        if avail_w >= 1120.0 {
            let card_w = ((avail_w - spacing * 3.0) / 4.0).floor().max(180.0);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = spacing;
                self.render_latency_card(
                    ui,
                    card_w,
                    &stats.overall_latency,
                    accent_color,
                    muted_color,
                    border_color,
                );
                self.render_cache_card(
                    ui,
                    card_w,
                    &stats.cache,
                    accent_color,
                    muted_color,
                    border_color,
                );
                self.render_process_memory_card(
                    ui,
                    card_w,
                    &stats.memory,
                    &stats.cache,
                    router,
                    accent_color,
                    muted_color,
                    border_color,
                );
                self.render_vps_memory_card(
                    ui,
                    card_w,
                    &stats.memory,
                    &stats.store,
                    muted_color,
                    border_color,
                );
            });
        } else if avail_w >= 480.0 {
            let card_w = ((avail_w - spacing) * 0.5).floor().max(180.0);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = spacing;
                self.render_latency_card(
                    ui,
                    card_w,
                    &stats.overall_latency,
                    accent_color,
                    muted_color,
                    border_color,
                );
                self.render_cache_card(
                    ui,
                    card_w,
                    &stats.cache,
                    accent_color,
                    muted_color,
                    border_color,
                );
            });
            ui.add_space(spacing);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = spacing;
                self.render_process_memory_card(
                    ui,
                    card_w,
                    &stats.memory,
                    &stats.cache,
                    router,
                    accent_color,
                    muted_color,
                    border_color,
                );
                self.render_vps_memory_card(
                    ui,
                    card_w,
                    &stats.memory,
                    &stats.store,
                    muted_color,
                    border_color,
                );
            });
        } else {
            self.render_latency_card(
                ui,
                avail_w,
                &stats.overall_latency,
                accent_color,
                muted_color,
                border_color,
            );
            ui.add_space(spacing);
            self.render_cache_card(
                ui,
                avail_w,
                &stats.cache,
                accent_color,
                muted_color,
                border_color,
            );
            ui.add_space(spacing);
            self.render_process_memory_card(
                ui,
                avail_w,
                &stats.memory,
                &stats.cache,
                router,
                accent_color,
                muted_color,
                border_color,
            );
            ui.add_space(spacing);
            self.render_vps_memory_card(
                ui,
                avail_w,
                &stats.memory,
                &stats.store,
                muted_color,
                border_color,
            );
        }
    }

    fn render_latency_card(
        &self,
        ui: &mut egui::Ui,
        width: f32,
        lat: &EndpointLatencyStats,
        accent_color: Color32,
        muted_color: Color32,
        border_color: Color32,
    ) {
        card_frame(ui, width, border_color, |ui| {
            ui.set_min_height(68.0);
            ui.spacing_mut().item_spacing.y = 3.0;
            ui.label(
                RichText::new("RESPONSE TIMES (DATA PLANE)")
                    .size(10.0)
                    .strong()
                    .color(muted_color),
            );
            let p90_color = if lat.p90_ms < 25.0 {
                Color32::from_rgb(34, 197, 94)
            } else if lat.p90_ms < 100.0 {
                Color32::from_rgb(245, 158, 11)
            } else {
                Color32::from_rgb(239, 68, 68)
            };
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!("Avg {:.2} ms", lat.avg_ms))
                        .monospace()
                        .size(14.0)
                        .strong()
                        .color(accent_color),
                );
                ui.label(RichText::new("·").color(muted_color));
                ui.label(
                    RichText::new(format!("P90 {:.2} ms", lat.p90_ms))
                        .monospace()
                        .size(14.0)
                        .strong()
                        .color(p90_color),
                );
            });
            ui.label(
                RichText::new(format!(
                    "P50: {:.2} ms  ·  P99: {:.2} ms  ·  Max: {:.2} ms",
                    lat.p50_ms, lat.p99_ms, lat.max_ms
                ))
                .monospace()
                .size(10.5)
                .color(ui.visuals().text_color()),
            );
            ui.label(
                RichText::new(format!(
                    "{} requests ({} errors)  ·  Last: {:.2} ms",
                    lat.request_count, lat.error_count, lat.last_ms
                ))
                .size(10.0)
                .color(muted_color),
            );
        });
    }

    fn render_cache_card(
        &self,
        ui: &mut egui::Ui,
        width: f32,
        cache: &CloudCacheStats,
        accent_color: Color32,
        muted_color: Color32,
        border_color: Color32,
    ) {
        card_frame(ui, width, border_color, |ui| {
            ui.set_min_height(68.0);
            ui.spacing_mut().item_spacing.y = 3.0;
            ui.label(
                RichText::new("DAQCLOUD CACHE USAGE")
                    .size(10.0)
                    .strong()
                    .color(muted_color),
            );
            let used_pct = if cache.total_max_bytes > 0 {
                (cache.total_used_bytes as f64 / cache.total_max_bytes as f64 * 100.0)
                    .clamp(0.0, 100.0)
            } else {
                0.0
            };
            let hit_str = cache
                .hit_rate_pct()
                .map_or_else(|| "N/A".to_owned(), |r| format!("{r:.1}%"));

            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!(
                        "{} / {}",
                        format_bytes(cache.total_used_bytes),
                        format_bytes(cache.total_max_bytes)
                    ))
                    .monospace()
                    .size(14.0)
                    .strong(),
                );
                ui.label(
                    RichText::new(format!("({used_pct:.0}% · Hit {hit_str})"))
                        .monospace()
                        .size(11.0)
                        .color(accent_color),
                );
            });

            draw_split_usage_bar(
                ui,
                cache.tile_used_bytes,
                cache.preload_used_bytes,
                cache.total_max_bytes.max(1),
                Color32::from_rgb(56, 189, 248),
                Color32::from_rgb(245, 158, 11),
            );

            ui.label(
                RichText::new(format!(
                    "Tiles: {} ({})  ·  Preloads: {} ({})",
                    format_bytes(cache.tile_used_bytes),
                    cache.tile_entries,
                    format_bytes(cache.preload_used_bytes),
                    cache.preload_entries
                ))
                .size(10.0)
                .color(muted_color),
            );
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn render_process_memory_card(
        &self,
        ui: &mut egui::Ui,
        width: f32,
        mem: &CloudMemoryStats,
        cache: &CloudCacheStats,
        router: &VirtualTimelineRouter,
        accent_color: Color32,
        muted_color: Color32,
        border_color: Color32,
    ) {
        card_frame(ui, width, border_color, |ui| {
            ui.set_min_height(68.0);
            ui.spacing_mut().item_spacing.y = 3.0;
            ui.label(
                RichText::new("TOTAL MEMORY USAGE (PROCESS & CACHE)")
                    .size(10.0)
                    .strong()
                    .color(muted_color),
            );
            let rss_str = mem
                .process_rss_bytes
                .map_or_else(|| "N/A".to_owned(), format_bytes);
            let vms_str = mem
                .process_virtual_bytes
                .map_or_else(|| "N/A".to_owned(), format_bytes);
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!("RSS {rss_str}"))
                        .monospace()
                        .size(14.0)
                        .strong()
                        .color(Color32::from_rgb(168, 85, 247)),
                );
                ui.label(
                    RichText::new(format!("(Virtual {vms_str})"))
                        .monospace()
                        .size(11.0)
                        .color(muted_color),
                );
            });

            if let (Some(rss), Some(vps_total)) = (mem.process_rss_bytes, mem.vps_total_bytes) {
                draw_split_usage_bar(
                    ui,
                    cache.total_used_bytes.min(rss),
                    rss.saturating_sub(cache.total_used_bytes),
                    vps_total.max(1),
                    accent_color,
                    Color32::from_rgb(168, 85, 247),
                );
            }

            let viewer_tc = router.tile_cache();
            ui.label(
                RichText::new(format!(
                    "Viewer Local TileCache: {} / {} ({} tiles)",
                    format_bytes(viewer_tc.lru_bytes() as u64),
                    format_bytes(viewer_tc.max_lru_bytes() as u64),
                    viewer_tc.lru_len()
                ))
                .size(10.0)
                .color(muted_color),
            );
        });
    }

    fn render_vps_memory_card(
        &self,
        ui: &mut egui::Ui,
        width: f32,
        mem: &CloudMemoryStats,
        store: &daqcore::storage::CloudStoreStats,
        muted_color: Color32,
        border_color: Color32,
    ) {
        card_frame(ui, width, border_color, |ui| {
            ui.set_min_height(68.0);
            ui.spacing_mut().item_spacing.y = 3.0;
            ui.label(
                RichText::new("VPS HOST MEMORY FREE & AVAILABLE")
                    .size(10.0)
                    .strong()
                    .color(muted_color),
            );
            let avail_bytes = mem.vps_available_bytes.or(mem.vps_free_bytes);
            let avail_str = avail_bytes.map_or_else(|| "N/A".to_owned(), format_bytes);
            let free_str = mem
                .vps_free_bytes
                .map_or_else(|| "N/A".to_owned(), format_bytes);
            let total_str = mem
                .vps_total_bytes
                .map_or_else(|| "N/A".to_owned(), format_bytes);

            let free_color = match (avail_bytes, mem.vps_total_bytes) {
                (Some(avail), Some(tot)) if tot > 0 => {
                    let ratio = avail as f64 / tot as f64;
                    if ratio >= 0.25 {
                        Color32::from_rgb(34, 197, 94)
                    } else if ratio >= 0.10 {
                        Color32::from_rgb(245, 158, 11)
                    } else {
                        Color32::from_rgb(239, 68, 68)
                    }
                }
                _ => Color32::from_rgb(34, 197, 94),
            };

            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!("{avail_str} Avail"))
                        .monospace()
                        .size(14.0)
                        .strong()
                        .color(free_color),
                );
                ui.label(
                    RichText::new(format!("({free_str} Free / {total_str} Total)"))
                        .monospace()
                        .size(10.5)
                        .color(muted_color),
                );
            });

            if let (Some(tot), Some(avail)) = (mem.vps_total_bytes, avail_bytes) {
                let used = tot.saturating_sub(avail);
                let buf_cached = mem.vps_buffers_cached_bytes.unwrap_or(0).min(avail);
                draw_split_usage_bar(
                    ui,
                    used,
                    buf_cached,
                    tot.max(1),
                    Color32::from_rgb(245, 158, 11),
                    Color32::from_rgb(56, 189, 248).gamma_multiply(0.55),
                );
            }

            ui.label(
                RichText::new(format!(
                    "Store Disk: {}  ·  {} partitions ({} days)",
                    format_bytes(store.store_disk_bytes),
                    store.partitions,
                    store.track_days
                ))
                .size(10.0)
                .color(muted_color),
            );
        });
    }

    fn show_charts_row(
        &self,
        ui: &mut egui::Ui,
        stats: &CloudStatsResponse,
        snapshot: &CloudStatsSnapshot,
        accent_color: Color32,
        muted_color: Color32,
        border_color: Color32,
    ) {
        let avail_w = (ui.available_width() - 4.0).max(220.0);
        let spacing = 6.0_f32;
        let side_by_side = avail_w >= 640.0;
        let chart_h = 175.0_f32;

        if side_by_side {
            let col_w = ((avail_w - spacing) * 0.5).floor().max(200.0);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = spacing;
                self.render_latency_chart(
                    ui,
                    col_w,
                    chart_h,
                    stats,
                    accent_color,
                    muted_color,
                    border_color,
                );
                self.render_memory_history_chart(
                    ui,
                    col_w,
                    chart_h,
                    snapshot,
                    accent_color,
                    muted_color,
                    border_color,
                );
            });
        } else {
            self.render_latency_chart(
                ui,
                avail_w,
                chart_h,
                stats,
                accent_color,
                muted_color,
                border_color,
            );
            ui.add_space(spacing);
            self.render_memory_history_chart(
                ui,
                avail_w,
                chart_h,
                snapshot,
                accent_color,
                muted_color,
                border_color,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_latency_chart(
        &self,
        ui: &mut egui::Ui,
        width: f32,
        height: f32,
        stats: &CloudStatsResponse,
        accent_color: Color32,
        muted_color: Color32,
        border_color: Color32,
    ) {
        card_frame(ui, width, border_color, |ui| {
            let inner_w = ui.available_width().max(80.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new("RECENT REQUEST LATENCY (MS)")
                        .size(10.0)
                        .strong()
                        .color(muted_color),
                );
                ui.add_space(6.0);
                ui.colored_label(
                    Color32::from_rgb(34, 197, 94),
                    RichText::new("• Hit").size(10.0),
                );
                ui.colored_label(
                    Color32::from_rgb(245, 158, 11),
                    RichText::new("• Miss/Decode").size(10.0),
                );
                ui.colored_label(
                    accent_color,
                    RichText::new(format!("— Avg ({:.1}ms)", stats.overall_latency.avg_ms))
                        .size(10.0),
                );
                ui.colored_label(
                    Color32::from_rgb(245, 158, 11),
                    RichText::new(format!("-- P90 ({:.1}ms)", stats.overall_latency.p90_ms))
                        .size(10.0),
                );
            });

            let samples = &stats.recent_latency_samples;
            let all_pts: Vec<[f64; 2]> = samples
                .iter()
                .enumerate()
                .map(|(idx, s)| [idx as f64 + 1.0, s.elapsed_ms])
                .collect();
            let hit_pts: Vec<[f64; 2]> = samples
                .iter()
                .enumerate()
                .filter(|(_, s)| s.cache_hit)
                .map(|(idx, s)| [idx as f64 + 1.0, s.elapsed_ms])
                .collect();
            let miss_pts: Vec<[f64; 2]> = samples
                .iter()
                .enumerate()
                .filter(|(_, s)| !s.cache_hit)
                .map(|(idx, s)| [idx as f64 + 1.0, s.elapsed_ms])
                .collect();

            let avg_ms = stats.overall_latency.avg_ms;
            let p90_ms = stats.overall_latency.p90_ms;
            let max_sample_ms = samples
                .iter()
                .map(|s| s.elapsed_ms)
                .fold(0.0_f64, f64::max)
                .max(p90_ms)
                .max(avg_ms)
                .max(1.0);

            egui_plot::Plot::new((&self.title, "recent_latency_plot"))
                .width(inner_w)
                .height((height - 28.0).max(80.0))
                .allow_drag(false)
                .allow_zoom(false)
                .allow_scroll(false)
                .allow_boxed_zoom(false)
                .include_x(0.0)
                .include_x((samples.len().max(5)) as f64)
                .include_y(0.0)
                .include_y(max_sample_ms * 1.1)
                .x_axis_label("Recent Requests")
                .y_axis_label("ms")
                .show(ui, |plot| {
                    if !all_pts.is_empty() {
                        plot.line(
                            egui_plot::Line::new("Latency (ms)", all_pts)
                                .color(accent_color.gamma_multiply(0.65))
                                .width(1.5_f32),
                        );
                    }
                    if !hit_pts.is_empty() {
                        plot.points(
                            egui_plot::Points::new("Cache Hit", hit_pts)
                                .color(Color32::from_rgb(34, 197, 94))
                                .radius(3.0_f32),
                        );
                    }
                    if !miss_pts.is_empty() {
                        plot.points(
                            egui_plot::Points::new("Parquet Decode / Disk", miss_pts)
                                .color(Color32::from_rgb(245, 158, 11))
                                .radius(3.0_f32),
                        );
                    }
                    if avg_ms > 0.0 {
                        plot.hline(
                            egui_plot::HLine::new("Avg", avg_ms)
                                .color(accent_color)
                                .style(egui_plot::LineStyle::dashed_dense()),
                        );
                    }
                    if p90_ms > 0.0 {
                        plot.hline(
                            egui_plot::HLine::new("P90", p90_ms)
                                .color(Color32::from_rgb(245, 158, 11))
                                .style(egui_plot::LineStyle::dotted_dense()),
                        );
                    }
                });
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn render_memory_history_chart(
        &self,
        ui: &mut egui::Ui,
        width: f32,
        height: f32,
        snapshot: &CloudStatsSnapshot,
        accent_color: Color32,
        muted_color: Color32,
        border_color: Color32,
    ) {
        card_frame(ui, width, border_color, |ui| {
            let inner_w = ui.available_width().max(80.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new("MEMORY & CACHE HISTORY (MIB)")
                        .size(10.0)
                        .strong()
                        .color(muted_color),
                );
                ui.add_space(6.0);
                ui.colored_label(
                    Color32::from_rgb(168, 85, 247),
                    RichText::new("■ Process RSS").size(10.0),
                );
                ui.colored_label(accent_color, RichText::new("■ Cache Used").size(10.0));
                ui.colored_label(
                    Color32::from_rgb(245, 158, 11),
                    RichText::new("-- Cache Budget").size(10.0),
                );
            });

            let mib = 1024.0 * 1024.0;
            let rss_pts: Vec<[f64; 2]> = snapshot
                .history
                .iter()
                .enumerate()
                .filter_map(|(i, s)| {
                    s.memory
                        .process_rss_bytes
                        .map(|b| [i as f64 + 1.0, b as f64 / mib])
                })
                .collect();
            let cache_pts: Vec<[f64; 2]> = snapshot
                .history
                .iter()
                .enumerate()
                .map(|(i, s)| [i as f64 + 1.0, s.cache.total_used_bytes as f64 / mib])
                .collect();
            let budget_mib = snapshot
                .latest
                .as_ref()
                .map_or(0.0, |s| s.cache.total_max_bytes as f64 / mib);
            let max_y = rss_pts
                .iter()
                .chain(cache_pts.iter())
                .map(|p| p[1])
                .fold(0.0_f64, f64::max)
                .max(budget_mib)
                .max(10.0);

            egui_plot::Plot::new((&self.title, "memory_history_plot"))
                .width(inner_w)
                .height((height - 28.0).max(80.0))
                .allow_drag(false)
                .allow_zoom(false)
                .allow_scroll(false)
                .allow_boxed_zoom(false)
                .include_x(0.0)
                .include_x((snapshot.history.len().max(5)) as f64)
                .include_y(0.0)
                .include_y(max_y * 1.15)
                .x_axis_label("Poll Samples")
                .y_axis_label("MiB")
                .show(ui, |plot| {
                    if !rss_pts.is_empty() {
                        plot.line(
                            egui_plot::Line::new("Process RSS (MiB)", rss_pts.clone())
                                .color(Color32::from_rgb(168, 85, 247))
                                .width(1.8_f32),
                        );
                        if rss_pts.len() <= 20 {
                            plot.points(
                                egui_plot::Points::new("Process RSS", rss_pts)
                                    .color(Color32::from_rgb(168, 85, 247))
                                    .radius(3.0_f32),
                            );
                        }
                    }
                    if !cache_pts.is_empty() {
                        plot.line(
                            egui_plot::Line::new("Cache Used (MiB)", cache_pts.clone())
                                .color(accent_color)
                                .width(1.8_f32),
                        );
                        if cache_pts.len() <= 20 {
                            plot.points(
                                egui_plot::Points::new("Cache Used", cache_pts)
                                    .color(accent_color)
                                    .radius(3.0_f32),
                            );
                        }
                    }
                    if budget_mib > 0.0 {
                        plot.hline(
                            egui_plot::HLine::new("Cache Budget", budget_mib)
                                .color(Color32::from_rgb(245, 158, 11).gamma_multiply(0.7))
                                .style(egui_plot::LineStyle::dashed_loose()),
                        );
                    }
                });
        });
    }

    fn show_tables_row(
        &self,
        ui: &mut egui::Ui,
        stats: &CloudStatsResponse,
        router: &VirtualTimelineRouter,
        accent_color: Color32,
        muted_color: Color32,
        border_color: Color32,
    ) {
        let avail_w = (ui.available_width() - 4.0).max(220.0);
        self.render_endpoint_latency_table(
            ui,
            avail_w,
            stats,
            accent_color,
            muted_color,
            border_color,
        );
        ui.add_space(2.0);
        self.render_cache_and_vps_breakdown_table(
            ui,
            avail_w,
            stats,
            router,
            muted_color,
            border_color,
        );
    }

    fn render_endpoint_latency_table(
        &self,
        ui: &mut egui::Ui,
        width: f32,
        stats: &CloudStatsResponse,
        accent_color: Color32,
        muted_color: Color32,
        border_color: Color32,
    ) {
        card_frame(ui, width, border_color, |ui| {
            ui.label(
                RichText::new("PER-ENDPOINT RESPONSE TIME BREAKDOWN")
                    .size(10.0)
                    .strong()
                    .color(muted_color),
            );
            ui.add_space(4.0);
            egui::ScrollArea::horizontal()
                .id_salt((&self.title, "endpoint_table_hscroll"))
                .show(ui, |ui| {
                    egui::Grid::new((&self.title, "endpoint_latency_grid"))
                        .striped(true)
                        .spacing([18.0, 4.0])
                        .show(ui, |ui| {
                            for header in [
                                "Endpoint Category",
                                "Requests",
                                "Errors",
                                "Avg (ms)",
                                "P50 (ms)",
                                "P90 (ms)",
                                "P99 (ms)",
                                "Max (ms)",
                                "Last (ms)",
                            ] {
                                ui.label(
                                    RichText::new(header).size(10.5).strong().color(muted_color),
                                );
                            }
                            ui.end_row();

                            render_endpoint_row(ui, &stats.overall_latency, true, accent_color);
                            for ep in &stats.endpoints {
                                render_endpoint_row(ui, ep, false, accent_color);
                            }
                        });
                });
        });
    }

    fn render_cache_and_vps_breakdown_table(
        &self,
        ui: &mut egui::Ui,
        width: f32,
        stats: &CloudStatsResponse,
        router: &VirtualTimelineRouter,
        muted_color: Color32,
        border_color: Color32,
    ) {
        card_frame(ui, width, border_color, |ui| {
            ui.label(
                RichText::new("CACHE TIERS, VPS RESOURCES & STORE INVENTORY")
                    .size(10.0)
                    .strong()
                    .color(muted_color),
            );
            ui.add_space(4.0);
            egui::ScrollArea::horizontal()
                .id_salt((&self.title, "cache_table_hscroll"))
                .show(ui, |ui| {
                    egui::Grid::new((&self.title, "cache_tiers_grid"))
                        .striped(true)
                        .spacing([18.0, 4.0])
                        .show(ui, |ui| {
                            for h in [
                                "Cache Tier / Resource",
                                "Used",
                                "Budget / Total",
                                "Entries",
                                "Hits",
                                "Misses",
                                "Hit Rate",
                            ] {
                                ui.label(RichText::new(h).size(10.5).strong().color(muted_color));
                            }
                            ui.end_row();

                            let c = &stats.cache;
                            let tile_hr = c
                                .tile_hit_rate_pct()
                                .map_or_else(|| "—".to_owned(), |p| format!("{p:.1}%"));
                            let preload_hr = c
                                .preload_hit_rate_pct()
                                .map_or_else(|| "—".to_owned(), |p| format!("{p:.1}%"));
                            let cat_tot = c.catalog_hits + c.catalog_misses;
                            let cat_hr = if cat_tot > 0 {
                                format!("{:.1}%", c.catalog_hits as f64 / cat_tot as f64 * 100.0)
                            } else {
                                "—".to_owned()
                            };
                            let sha_tot = c.sha256_hits + c.sha256_misses;
                            let sha_hr = if sha_tot > 0 {
                                format!("{:.1}%", c.sha256_hits as f64 / sha_tot as f64 * 100.0)
                            } else {
                                "—".to_owned()
                            };

                            for (name, used, budget, entries, hits, misses, hr) in [
                                (
                                    "daqcloud Tile LRU Cache",
                                    format_bytes(c.tile_used_bytes),
                                    format_bytes(c.tile_max_bytes),
                                    c.tile_entries.to_string(),
                                    c.tile_hits.to_string(),
                                    c.tile_misses.to_string(),
                                    tile_hr,
                                ),
                                (
                                    "daqcloud Preload LRU Cache",
                                    format_bytes(c.preload_used_bytes),
                                    format_bytes(c.preload_max_bytes),
                                    c.preload_entries.to_string(),
                                    c.preload_hits.to_string(),
                                    c.preload_misses.to_string(),
                                    preload_hr,
                                ),
                                (
                                    "daqcloud Catalog JSON Cache",
                                    format_bytes(c.catalog_used_bytes),
                                    "Auto".to_owned(),
                                    usize::from(c.catalog_used_bytes > 0).to_string(),
                                    c.catalog_hits.to_string(),
                                    c.catalog_misses.to_string(),
                                    cat_hr,
                                ),
                                (
                                    "daqcloud File SHA-256 Cache",
                                    "—".to_owned(),
                                    "4096 entries".to_owned(),
                                    c.sha256_entries.to_string(),
                                    c.sha256_hits.to_string(),
                                    c.sha256_misses.to_string(),
                                    sha_hr,
                                ),
                            ] {
                                ui.label(RichText::new(name).size(11.0));
                                ui.label(RichText::new(used).monospace().size(11.0));
                                ui.label(RichText::new(budget).monospace().size(11.0));
                                ui.label(RichText::new(entries).monospace().size(11.0));
                                ui.label(RichText::new(hits).monospace().size(11.0));
                                ui.label(RichText::new(misses).monospace().size(11.0));
                                ui.label(RichText::new(hr).monospace().size(11.0).strong());
                                ui.end_row();
                            }

                            let tc = router.tile_cache();
                            ui.label(RichText::new("Viewer Local TileCache (Client)").size(11.0));
                            ui.label(
                                RichText::new(format_bytes(tc.lru_bytes() as u64))
                                    .monospace()
                                    .size(11.0),
                            );
                            ui.label(
                                RichText::new(format_bytes(tc.max_lru_bytes() as u64))
                                    .monospace()
                                    .size(11.0),
                            );
                            ui.label(
                                RichText::new(format!(
                                    "{} tiles ({} preloads)",
                                    tc.lru_len(),
                                    tc.preloaded_partition_count()
                                ))
                                .monospace()
                                .size(11.0),
                            );
                            ui.label(RichText::new("—").monospace().size(11.0));
                            ui.label(RichText::new("—").monospace().size(11.0));
                            ui.label(RichText::new("Local RAM").monospace().size(11.0));
                            ui.end_row();
                        });

                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(4.0);

                    let m = &stats.memory;
                    let st = &stats.store;
                    let rss = m
                        .process_rss_bytes
                        .map_or_else(|| "N/A".to_owned(), format_bytes);
                    let vms = m
                        .process_virtual_bytes
                        .map_or_else(|| "N/A".to_owned(), format_bytes);
                    let vps_tot = m
                        .vps_total_bytes
                        .map_or_else(|| "N/A".to_owned(), format_bytes);
                    let vps_avail = m
                        .vps_available_bytes
                        .or(m.vps_free_bytes)
                        .map_or_else(|| "N/A".to_owned(), format_bytes);
                    let vps_free = m
                        .vps_free_bytes
                        .map_or_else(|| "N/A".to_owned(), format_bytes);
                    let vps_buf = m
                        .vps_buffers_cached_bytes
                        .map_or_else(|| "N/A".to_owned(), format_bytes);
                    let swap_free = m
                        .vps_swap_free_bytes
                        .map_or_else(|| "N/A".to_owned(), format_bytes);
                    let swap_tot = m
                        .vps_swap_total_bytes
                        .map_or_else(|| "N/A".to_owned(), format_bytes);

                    egui::Grid::new((&self.title, "vps_system_store_grid"))
                        .striped(true)
                        .spacing([18.0, 4.0])
                        .show(ui, |ui| {
                            for h in [
                                "System & Store Resource",
                                "Primary Metric",
                                "Secondary / Capacity",
                                "Details",
                            ] {
                                ui.label(RichText::new(h).size(10.5).strong().color(muted_color));
                            }
                            ui.end_row();

                            for (name, primary, secondary, details) in [
                                (
                                    "daqcloud Process Memory",
                                    format!("RSS: {rss}"),
                                    format!("Virtual: {vms}"),
                                    format!(
                                        "Cache: {} / {}",
                                        format_bytes(stats.cache.total_used_bytes),
                                        format_bytes(stats.cache.total_max_bytes)
                                    ),
                                ),
                                (
                                    "VPS Host Physical Memory",
                                    format!("Available: {vps_avail}"),
                                    format!("Free: {vps_free} / Total: {vps_tot}"),
                                    format!("Buffers + Cached: {vps_buf}"),
                                ),
                                (
                                    "VPS Host Swap Memory",
                                    format!("Swap Free: {swap_free}"),
                                    format!("Swap Total: {swap_tot}"),
                                    format!("Active WS Clients: {}", stats.active_ws_clients),
                                ),
                                (
                                    "daqcloud Store Inventory",
                                    format!("Disk: {}", format_bytes(st.store_disk_bytes)),
                                    format!(
                                        "{} Partitions across {} Track Days",
                                        st.partitions, st.track_days
                                    ),
                                    format!(
                                        "{} Annotations  ·  {} Derived Signals",
                                        st.annotations, st.derived_signals
                                    ),
                                ),
                            ] {
                                ui.label(RichText::new(name).size(11.0));
                                ui.label(RichText::new(primary).monospace().size(11.0).strong());
                                ui.label(RichText::new(secondary).monospace().size(11.0));
                                ui.label(RichText::new(details).monospace().size(11.0));
                                ui.end_row();
                            }
                        });
                });
        });
    }
}

fn render_endpoint_row(
    ui: &mut egui::Ui,
    ep: &EndpointLatencyStats,
    is_summary: bool,
    accent_color: Color32,
) {
    let label_rt = if is_summary {
        RichText::new(&ep.endpoint)
            .size(11.0)
            .strong()
            .color(accent_color)
    } else {
        RichText::new(&ep.endpoint).monospace().size(11.0)
    };
    ui.label(label_rt);
    ui.label(
        RichText::new(ep.request_count.to_string())
            .monospace()
            .size(11.0),
    );
    let err_color = if ep.error_count > 0 {
        Color32::from_rgb(239, 68, 68)
    } else {
        ui.visuals().text_color()
    };
    ui.label(
        RichText::new(ep.error_count.to_string())
            .monospace()
            .size(11.0)
            .color(err_color),
    );
    for val in [
        ep.avg_ms, ep.p50_ms, ep.p90_ms, ep.p99_ms, ep.max_ms, ep.last_ms,
    ] {
        let rt = RichText::new(format!("{val:.2}")).monospace().size(11.0);
        ui.label(if is_summary { rt.strong() } else { rt });
    }
    ui.end_row();
}

fn card_frame(
    ui: &mut egui::Ui,
    width: f32,
    border_color: Color32,
    add_contents: impl FnOnce(&mut egui::Ui),
) -> egui::Rect {
    let theme = ui::theme::get_theme(ui.ctx());
    let inner_w = (width - 18.0).max(80.0);
    ui.allocate_ui_with_layout(
        egui::vec2(width, 0.0),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.set_width(width);
            ui.set_max_width(width);
            egui::Frame::NONE
                .fill(theme.panel_color())
                .stroke(Stroke::new(1.0_f32, border_color))
                .corner_radius(4.0)
                .inner_margin(egui::Margin::symmetric(8, 6))
                .show(ui, |ui| {
                    ui.set_width(inner_w);
                    ui.set_max_width(inner_w);
                    add_contents(ui);
                })
                .response
                .rect
        },
    )
    .inner
}

fn draw_split_usage_bar(
    ui: &mut egui::Ui,
    primary_bytes: u64,
    secondary_bytes: u64,
    total_bytes: u64,
    primary_color: Color32,
    secondary_color: Color32,
) {
    let desired = egui::vec2(ui.available_width().max(40.0), 6.0);
    let (rect, _) = ui.allocate_exact_size(desired, egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, 3.0, ui.visuals().extreme_bg_color);

    if total_bytes == 0 {
        return;
    }
    let p_frac = (primary_bytes as f32 / total_bytes as f32).clamp(0.0, 1.0);
    let s_frac = (secondary_bytes as f32 / total_bytes as f32).clamp(0.0, 1.0 - p_frac);

    if p_frac > 0.0 {
        let p_rect =
            egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() * p_frac, rect.height()));
        ui.painter().rect_filled(p_rect, 3.0, primary_color);
    }
    if s_frac > 0.0 {
        let s_rect = egui::Rect::from_min_size(
            egui::pos2(rect.left() + rect.width() * p_frac, rect.top()),
            egui::vec2(rect.width() * s_frac, rect.height()),
        );
        ui.painter().rect_filled(s_rect, 3.0, secondary_color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::{NoopViewerPlatform, ViewerState},
        widget_constructor,
        workspace_io::{FlexibleLayout, PaneSpec, WorkspaceLayoutNode, WorkspaceView},
    };
    use daqcore::storage::{CloudLatencySample, CloudStoreStats};

    #[test]
    fn viewer_loading_summary_formats_badges_and_tooltips() {
        let router = VirtualTimelineRouter::live(
            daqcore::time::Time::from_unix_millis(1_794_500_000_000),
            30.0,
        )
        .expect("live router");

        let idle = ViewerLoadingSummary::from_state(&router, false, 0);
        assert!(!idle.is_active());
        assert_eq!(idle.badge_label(), "Ready");

        let busy = ViewerLoadingSummary {
            tiles: TileInFlightSummary {
                full_res_count: 3,
                overview_10hz_count: 1,
                overview_1hz_count: 0,
                sample_signals: vec![
                    "IMU_acceleration.X_axis".to_owned(),
                    "pack_stats.pack_voltage".to_owned(),
                ],
            },
            backend: BackendLoadingActivity {
                catalog_syncing: false,
                metadata_syncing: false,
                stats_fetching: true,
                in_flight_preloads: 2,
                pending_downloads: 1,
                pending_uploads: 0,
            },
            cloud_connecting: false,
            database_loading: false,
            platform_requests: 1,
        };
        assert!(busy.is_active());
        assert_eq!(
            busy.badge_label(),
            "Loading 4 tiles · 2 preloads · dl:1 · export"
        );
        let tip = busy.tooltip_breakdown(&router);
        assert!(tip.contains("Signal Tiles In-Flight: 4"));
        assert!(tip.contains("IMU_acceleration.X_axis"));
        assert!(tip.contains("Partition 1Hz Preloads Waiting: 2"));
        assert!(tip.contains("Dataset Exports Waiting: 1"));
    }

    #[test]
    fn cloud_stats_pane_renders_and_round_trips_through_workspace_view() {
        let ctx = egui::Context::default();
        let router = VirtualTimelineRouter::live(
            daqcore::time::Time::from_unix_millis(1_794_500_000_000),
            30.0,
        )
        .expect("live router");
        let mut state = ViewerState::new(
            router,
            ui::theme::ThemeSelection::default(),
            None,
            app::DEFAULT_CLOUD_URL.to_owned(),
            false,
            None,
            None,
        );
        let mut platform = NoopViewerPlatform;

        state.handle_action(
            action::AppAction::SpawnWidget(widget_constructor::WidgetConstructor::CloudStats),
            &mut platform,
            &ctx,
        );
        assert!(!state.tile_tree.is_empty());

        // Verify .daqview.json and daqview1:... share string round-trip
        let captured = WorkspaceView::capture_from_state(&state, "Cloud Stats View", "Test", true)
            .expect("capture workspace");
        let share = captured.to_share_string().expect("encode share string");
        let restored = WorkspaceView::from_str_flexible(&share).expect("decode share string");
        assert_eq!(
            restored.resolved_layout().unwrap(),
            WorkspaceLayoutNode::Pane {
                spec: PaneSpec::CloudStats {
                    title: "daqcloud Stats".to_owned(),
                },
            }
        );
        let _ = FlexibleLayout::Tree(restored.resolved_layout().unwrap());

        // Render a full frame with CloudStats open and synthetic stats data
        let sample_stats = CloudStatsResponse {
            collected_at_utc_ms: 1_794_500_005_000,
            uptime_secs: 3725.0,
            active_ws_clients: 3,
            in_flight_http_requests: 1,
            overall_latency: EndpointLatencyStats {
                endpoint: "All Data Requests".to_owned(),
                request_count: 142,
                error_count: 0,
                avg_ms: 3.45,
                p50_ms: 1.80,
                p90_ms: 8.20,
                p99_ms: 19.50,
                max_ms: 24.10,
                last_ms: 1.20,
            },
            endpoints: vec![EndpointLatencyStats {
                endpoint: "GET /partitions/{id}/tile".to_owned(),
                request_count: 120,
                error_count: 0,
                avg_ms: 2.90,
                p50_ms: 1.50,
                p90_ms: 6.40,
                p99_ms: 14.00,
                max_ms: 18.20,
                last_ms: 1.10,
            }],
            recent_latency_samples: vec![
                CloudLatencySample {
                    utc_ms: 1_794_500_004_000,
                    elapsed_ms: 12.4,
                    endpoint: "GET /partitions/{id}/tile".to_owned(),
                    status: 200,
                    cache_hit: false,
                },
                CloudLatencySample {
                    utc_ms: 1_794_500_004_500,
                    elapsed_ms: 0.9,
                    endpoint: "GET /partitions/{id}/tile".to_owned(),
                    status: 200,
                    cache_hit: true,
                },
            ],
            cache: CloudCacheStats {
                total_used_bytes: 24 * 1024 * 1024,
                total_max_bytes: 50 * 1024 * 1024,
                tile_used_bytes: 18 * 1024 * 1024,
                tile_max_bytes: 38 * 1024 * 1024,
                tile_entries: 64,
                tile_hits: 96,
                tile_misses: 24,
                preload_used_bytes: 6 * 1024 * 1024,
                preload_max_bytes: 12 * 1024 * 1024,
                preload_entries: 4,
                preload_hits: 18,
                preload_misses: 4,
                catalog_used_bytes: 16 * 1024,
                catalog_hits: 15,
                catalog_misses: 1,
                sha256_entries: 20,
                sha256_hits: 50,
                sha256_misses: 20,
            },
            memory: CloudMemoryStats {
                process_rss_bytes: Some(68 * 1024 * 1024),
                process_virtual_bytes: Some(412 * 1024 * 1024),
                vps_total_bytes: Some(2 * 1024 * 1024 * 1024),
                vps_free_bytes: Some(640 * 1024 * 1024),
                vps_available_bytes: Some(1280 * 1024 * 1024),
                vps_buffers_cached_bytes: Some(512 * 1024 * 1024),
                vps_swap_total_bytes: Some(1024 * 1024 * 1024),
                vps_swap_free_bytes: Some(1024 * 1024 * 1024),
            },
            store: CloudStoreStats {
                track_days: 3,
                partitions: 12,
                annotations: 48,
                derived_signals: 25,
                store_disk_bytes: 340 * 1024 * 1024,
            },
        };
        let mut snap = CloudStatsSnapshot::default();
        snap.record_response(sample_stats.clone());

        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 800.0),
            )),
            ..Default::default()
        };
        let mut recorded_max_right = 0.0_f32;
        let mut panel_right = 0.0_f32;
        let mut kpi_height = 0.0_f32;
        let mut chart_height = 0.0_f32;
        let _ = ctx.run(raw, |ctx| {
            state.update_frame(&mut platform, ctx);
            egui::CentralPanel::default().show(ctx, |ui| {
                panel_right = ui.max_rect().right();
                let widget = CloudStats::new(1);
                let top_y = ui.cursor().top();
                widget.show_kpi_cards_row(
                    ui,
                    &sample_stats,
                    &state.router,
                    Color32::from_rgb(56, 189, 248),
                    Color32::GRAY,
                    Color32::DARK_GRAY,
                );
                kpi_height = ui.cursor().top() - top_y;
                let chart_top_y = ui.cursor().top();
                widget.show_charts_row(
                    ui,
                    &sample_stats,
                    &snap,
                    Color32::from_rgb(56, 189, 248),
                    Color32::GRAY,
                    Color32::DARK_GRAY,
                );
                chart_height = ui.cursor().top() - chart_top_y;
                widget.show_tables_row(
                    ui,
                    &sample_stats,
                    &state.router,
                    Color32::from_rgb(56, 189, 248),
                    Color32::GRAY,
                    Color32::DARK_GRAY,
                );
                recorded_max_right = ui.min_rect().right();
            });
        });
        assert!(
            recorded_max_right <= panel_right + 1.0,
            "CloudStats content overflowed right edge of panel: content_right={recorded_max_right}, panel_right={panel_right}"
        );
        assert!(
            kpi_height >= 130.0,
            "Expected 2x2 KPI cards grid height >= 130px when sidebar is open, got {kpi_height}"
        );
        assert!(
            (160.0..=230.0).contains(&chart_height),
            "Expected side-by-side chart row height in 160..=230px, got {chart_height}"
        );
    }
}
