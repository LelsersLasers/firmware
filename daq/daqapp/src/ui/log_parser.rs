use crate::{action, app, settings, workspace_view};
use std::{path, sync::mpsc, thread};

type ImportChannelMsg = Result<(path::PathBuf, daqnative::log_parse::ImportProgress), String>;

pub struct LogParser {
    pub title: String,
    pub logs_dir: Option<path::PathBuf>,
    pub output_dir: Option<path::PathBuf>,
    pub database_path: Option<path::PathBuf>,
    parse_to_ui_rx: Option<mpsc::Receiver<ImportChannelMsg>>,
    progress_fraction: f32,
    parse_text: String,
    last_summary: Option<daqnative::storage::ImportSummary>,
}

impl LogParser {
    pub fn new(instance_num: usize) -> Self {
        let default_dbc = settings::database_dir().and_then(|db_dir| {
            for candidate in ["superdbc_per26.json", "superdbc_1e6fa38.json"] {
                let path = db_dir.join(candidate);
                if path.is_file() {
                    return Some(path);
                }
            }
            None
        });
        Self {
            title: format!("SD Card Import #{instance_num}"),
            logs_dir: None,
            output_dir: Some(path::PathBuf::from(settings::DEFAULT_LOG_FOLDER)),
            database_path: default_dbc,
            parse_to_ui_rx: None,
            progress_fraction: 0.0,
            parse_text: String::new(),
            last_summary: None,
        }
    }

    /// Start a background import of `self.logs_dir` into `self.output_dir`.
    pub fn start_import(&mut self, sidebar_database: Option<&app::DatabaseSelection>) {
        let Some(logs_dir) = self.logs_dir.clone() else {
            self.parse_text = "Error: SD card / raw logs directory not selected".to_owned();
            return;
        };
        let Some(output_dir) = self.output_dir.clone() else {
            self.parse_text = "Error: Store directory not selected".to_owned();
            return;
        };
        let path = self.database_path.clone().or_else(|| {
            sidebar_database
                .and_then(|selection| selection.source())
                .map(|source| source.path)
        });
        let Some(path) = path else {
            self.parse_text = "Error: No SuperDBC file selected".to_owned();
            return;
        };

        let (tx, rx) = mpsc::channel();
        self.parse_to_ui_rx = Some(rx);
        self.progress_fraction = 0.01;
        self.parse_text = "Starting SD card import pipeline…".to_owned();
        self.last_summary = None;

        thread::spawn(move || {
            let db = match daqnative::superdbc::database::Database::load(&path) {
                Ok(db) => db,
                Err(error) => {
                    let _ = tx.send(Err(error.to_string()));
                    return;
                }
            };

            let store_clone = output_dir.clone();
            let tx_progress = tx.clone();
            let result = daqnative::log_parse::import_logs_to_store_with_progress(
                &logs_dir,
                &output_dir,
                &db,
                move |progress| {
                    let _ = tx_progress.send(Ok((store_clone.clone(), progress)));
                },
            );

            if let Err(error) = result {
                let _ = tx.send(Err(error.to_string()));
            }
        });
    }

    pub fn poll_import_messages(&mut self, action_queue: &mut Vec<action::AppAction>) -> bool {
        let mut updated = false;
        let mut finished = false;

        if let Some(rx) = &self.parse_to_ui_rx {
            loop {
                match rx.try_recv() {
                    Ok(Ok((store_dir, progress))) => {
                        updated = true;
                        self.progress_fraction = progress.fraction();
                        self.parse_text = progress.status_text();
                        if let daqnative::log_parse::ImportProgress::Complete(summary) = progress {
                            let annotations: usize =
                                summary.manifests.iter().map(|m| m.annotations.len()).sum();
                            self.parse_text = format!(
                                "Import complete: {} files, {} epochs, {} partitions written ({} skipped), {} annotations at {}",
                                summary.log_files_parsed,
                                summary.boot_epochs_detected,
                                summary.partitions_written,
                                summary.partitions_skipped_existing,
                                annotations,
                                store_dir.display()
                            );
                            self.last_summary = Some(summary);
                            action_queue.push(action::AppAction::AttachStore(store_dir));
                            finished = true;
                            break;
                        }
                    }
                    Ok(Err(error)) => {
                        updated = true;
                        self.progress_fraction = 0.0;
                        self.parse_text = format!("Error: {error}");
                        finished = true;
                        break;
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        updated = true;
                        self.progress_fraction = 0.0;
                        self.parse_text =
                            "Error: Log importer stopped before reporting a result".to_owned();
                        finished = true;
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                }
            }
        }

        if finished {
            self.parse_to_ui_rx = None;
        }
        updated
    }

    pub fn is_importing(&self) -> bool {
        self.parse_to_ui_rx.is_some()
    }

    #[cfg(test)]
    pub fn progress_fraction(&self) -> f32 {
        self.progress_fraction
    }

    #[cfg(test)]
    pub fn status_text(&self) -> &str {
        &self.parse_text
    }

    #[cfg(test)]
    pub fn last_summary(&self) -> Option<&daqnative::storage::ImportSummary> {
        self.last_summary.as_ref()
    }

    pub fn show(
        &mut self,
        ui: &mut eframe::egui::Ui,
        sidebar_database: Option<&app::DatabaseSelection>,
        view: &workspace_view::WorkspaceDataView<'_>,
        action_queue: &mut Vec<action::AppAction>,
    ) -> egui_tiles::UiResponse {
        if self.output_dir.is_none()
            && let Some(active_store) = view.router.store_dir()
        {
            self.output_dir = Some(active_store.to_path_buf());
        }
        if self.database_path.is_none()
            && let Some(source) = sidebar_database.and_then(|s| s.source())
        {
            self.database_path = Some(source.path);
        }

        self.poll_import_messages(action_queue);
        if self.is_importing() {
            ui.ctx().request_repaint();
        }

        eframe::egui::ScrollArea::vertical()
            .id_salt((&self.title, "log_parser_scroll"))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.heading(format!("💾 {}", self.title));
                ui.separator();

                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(
                            !self.is_importing(),
                            eframe::egui::Button::new("📁 Select SD / Raw Logs Folder"),
                        )
                        .clicked()
                        && let Some(path) = rfd::FileDialog::new().pick_folder()
                    {
                        self.logs_dir = Some(path);
                    }
                    ui.add(
                        eframe::egui::Label::new(self.logs_dir.as_ref().map_or_else(
                            || "Raw Logs: None selected".to_owned(),
                            |path| format!("Raw Logs: {}", path.display()),
                        ))
                        .wrap(),
                    );
                });

                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(
                            !self.is_importing(),
                            eframe::egui::Button::new("📁 Select Parquet Store Folder"),
                        )
                        .clicked()
                        && let Some(path) = rfd::FileDialog::new().pick_folder()
                    {
                        self.output_dir = Some(path);
                    }
                    if let Some(store_dir) = &self.output_dir
                        && ui
                            .add_enabled(
                                !self.is_importing(),
                                eframe::egui::Button::new("🔗 Attach Store to Timeline"),
                            )
                            .on_hover_text(
                                "Load existing Parquet partitions & annotations from this store into the Master Timeline",
                            )
                            .clicked()
                    {
                        action_queue.push(action::AppAction::AttachStore(store_dir.clone()));
                    }
                    ui.add(
                        eframe::egui::Label::new(self.output_dir.as_ref().map_or_else(
                            || "Store: None selected".to_owned(),
                            |path| format!("Store: {}", path.display()),
                        ))
                        .wrap(),
                    );
                });

                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(
                            !self.is_importing(),
                            eframe::egui::Button::new("📁 Select SuperDBC"),
                        )
                        .clicked()
                    {
                        let mut dialog =
                            rfd::FileDialog::new().add_filter("SuperDBC JSON", &["json"]);
                        if let Some(dir) = settings::database_dir() {
                            dialog = dialog.set_directory(dir);
                        }
                        if let Some(path) = dialog.pick_file() {
                            self.database_path = Some(path);
                        }
                    }
                    ui.add(
                        eframe::egui::Label::new(self.database_path.as_ref().map_or_else(
                            || "SuperDBC: None selected".to_owned(),
                            |path| format!("SuperDBC: {}", path.display()),
                        ))
                        .wrap(),
                    );
                });

                ui.add_space(4.0);
                if ui
                    .add_enabled(
                        !self.is_importing(),
                        eframe::egui::Button::new(
                            "▶ Import SD Logs & Auto-Annotate into Timeline",
                        ),
                    )
                    .clicked()
                {
                    self.start_import(sidebar_database);
                }

                if self.is_importing() || self.progress_fraction > 0.0 {
                    ui.add_space(6.0);
                    ui.add(
                        eframe::egui::ProgressBar::new(self.progress_fraction.clamp(0.0, 1.0))
                            .show_percentage()
                            .text(&self.parse_text)
                            .animate(self.is_importing()),
                    );
                }

                if !self.parse_text.is_empty() {
                    ui.add_space(4.0);
                    ui.add(eframe::egui::Label::new(&self.parse_text).wrap());
                }

                if let Some(summary) = &self.last_summary
                    && !summary.manifests.is_empty()
                {
                    ui.separator();
                    ui.label(
                        eframe::egui::RichText::new(format!(
                            "Imported Partitions ({})",
                            summary.manifests.len()
                        ))
                        .strong(),
                    );
                    for manifest in &summary.manifests {
                        let provenance = daqnative::router::ClockProvenance::CanonicalPartition(
                            manifest.clock_mapping.clone(),
                        );
                        let duration_s =
                            ((manifest.end_utc_ms - manifest.start_utc_ms).max(0) as f64) / 1000.0;
                        ui.label(
                            eframe::egui::RichText::new(format!(
                                "• [{}] {} ({:.1}s, {}, {} signals, {} annotations)",
                                manifest.track_day_id,
                                manifest.partition_id,
                                duration_s,
                                provenance.badge_label(),
                                manifest.signals.len(),
                                manifest.annotations.len()
                            ))
                            .small(),
                        );
                    }
                }
            });
        egui_tiles::UiResponse::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn sd_card_import_manager_streams_progress_and_attaches_per26_store() {
        let manifest_dir = path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let dbc_path = manifest_dir.join("../../superdbc/superdbc_per26.json");
        let raw_logs_dir = manifest_dir.join("../../PER26/2026-09-06/2_regen/raw");
        if !dbc_path.is_file() || !raw_logs_dir.is_dir() {
            return;
        }

        // Copy two real PER26 RTC log files into a temporary SD folder so the test runs fast in debug mode
        let tmp_raw = tempfile::tempdir().expect("temp raw dir");
        for name in [
            "log-2026-09-06--20-16-37.log",
            "log-2026-09-06--20-30-45.log",
        ] {
            let src = raw_logs_dir.join(name);
            if src.is_file() {
                std::fs::copy(&src, tmp_raw.path().join(name)).expect("copy PER26 log file");
            }
        }

        let tmp_store = tempfile::tempdir().expect("temp store dir");
        let mut parser = LogParser::new(1);
        parser.logs_dir = Some(tmp_raw.path().to_path_buf());
        parser.output_dir = Some(tmp_store.path().to_path_buf());
        parser.database_path = Some(dbc_path);

        parser.start_import(None);
        assert!(parser.is_importing());

        let mut actions = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut saw_intermediate_progress = false;

        while parser.is_importing() && Instant::now() < deadline {
            if parser.poll_import_messages(&mut actions)
                && parser.progress_fraction() > 0.01
                && parser.progress_fraction() < 1.0
            {
                saw_intermediate_progress = true;
            }
            thread::sleep(Duration::from_millis(5));
        }

        assert!(
            !parser.is_importing(),
            "import should finish before timeout: {}",
            parser.status_text()
        );
        assert!(
            saw_intermediate_progress || (parser.progress_fraction() - 1.0).abs() < 1e-5,
            "progress should advance to completion"
        );
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, action::AppAction::AttachStore(p) if p == tmp_store.path())),
            "AttachStore action must be queued on completion"
        );

        let summary = parser.last_summary().expect("completed summary");
        assert!(summary.log_files_parsed >= 1);
        assert!(summary.partitions_written >= 1);

        let mut router = daqnative::storage::live_router(
            daqnative::time::Time::from_unix_millis(1_794_500_000_000),
            30.0,
        )
        .expect("live router");
        router
            .attach_store(tmp_store.path())
            .expect("attach imported store");
        assert!(!router.partitions().is_empty());
        assert!(!router.activity_envelope_1hz(i64::MIN..=i64::MAX).is_empty());
    }
}
