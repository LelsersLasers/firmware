use crate::{action, app, settings};
use std::{path, sync::mpsc, thread};

pub struct LogParser {
    pub title: String,
    pub logs_dir: Option<path::PathBuf>,
    pub output_dir: Option<path::PathBuf>,
    database_override: Option<path::PathBuf>,
    use_override: bool,
    parse_to_ui_rx: Option<mpsc::Receiver<Result<(path::PathBuf, String), String>>>,
    parse_text: String,
}

impl LogParser {
    pub fn new(instance_num: usize) -> Self {
        Self {
            title: format!("Log Parser #{instance_num}"),
            logs_dir: None,
            output_dir: None,
            database_override: None,
            use_override: false,
            parse_to_ui_rx: None,
            parse_text: String::new(),
        }
    }

    fn parse_logs(&mut self, sidebar_database: Option<&app::DatabaseSelection>) {
        let Some(logs_dir) = self.logs_dir.clone() else {
            self.parse_text = "Error: Logs directory not selected".to_owned();
            return;
        };
        let Some(output_dir) = self.output_dir.clone() else {
            self.parse_text = "Error: Store directory not selected".to_owned();
            return;
        };
        let path = if self.use_override {
            self.database_override.clone()
        } else {
            sidebar_database
                .and_then(|selection| selection.source())
                .map(|source| source.path)
        };
        let Some(path) = path else {
            self.parse_text = "Error: No SuperDBC file selected".to_owned();
            return;
        };
        let (tx, rx) = mpsc::channel();
        self.parse_to_ui_rx = Some(rx);
        self.parse_text = "Importing logs into Parquet partitions…".to_owned();
        thread::spawn(move || {
            let result = daqcore::superdbc::database::Database::load(&path)
                .map_err(|error| error.to_string())
                .and_then(|database| {
                    daqcore::log_parse::import_logs_to_store(&logs_dir, &output_dir, &database)
                        .map_err(|error| error.to_string())
                })
                .map(|summary| {
                    let annotations: usize =
                        summary.manifests.iter().map(|m| m.annotations.len()).sum();
                    let msg = format!(
                        "Import complete: {} files, {} epochs, {} partitions written ({} skipped), {} annotations at {}",
                        summary.log_files_parsed,
                        summary.boot_epochs_detected,
                        summary.partitions_written,
                        summary.partitions_skipped_existing,
                        annotations,
                        output_dir.display()
                    );
                    (output_dir, msg)
                });
            let _ = tx.send(result);
        });
    }

    pub fn show(
        &mut self,
        ui: &mut eframe::egui::Ui,
        sidebar_database: Option<&app::DatabaseSelection>,
        action_queue: &mut Vec<action::AppAction>,
    ) -> egui_tiles::UiResponse {
        eframe::egui::ScrollArea::vertical()
            .id_salt((&self.title, "log_parser_scroll"))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.heading(format!("🔧 {}", self.title));
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    if ui.button("📁 Select Logs Dir").clicked()
                        && let Some(path) = rfd::FileDialog::new().pick_folder()
                    {
                        self.logs_dir = Some(path);
                    }
                    ui.add(
                        eframe::egui::Label::new(self.logs_dir.as_ref().map_or_else(
                            || "Logs: None selected".to_owned(),
                            |path| format!("Logs: {}", path.display()),
                        ))
                        .wrap(),
                    );
                });
                ui.horizontal_wrapped(|ui| {
                    if ui.button("📁 Select Store Dir").clicked()
                        && let Some(path) = rfd::FileDialog::new().pick_folder()
                    {
                        self.output_dir = Some(path);
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
                ui.checkbox(&mut self.use_override, "Override sidebar SuperDBC file");
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(
                            self.use_override,
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
                            self.database_override = Some(path);
                        }
                    }
                    let path = if self.use_override {
                        self.database_override.clone()
                    } else {
                        sidebar_database
                            .and_then(|selection| selection.source())
                            .map(|source| source.path)
                    };
                    ui.add(
                        eframe::egui::Label::new(path.as_ref().map_or_else(
                            || "None selected".to_owned(),
                            |path| path.display().to_string(),
                        ))
                        .wrap(),
                    );
                });
                if ui
                    .add_enabled(
                        self.parse_to_ui_rx.is_none(),
                        eframe::egui::Button::new("▶ Import Logs"),
                    )
                    .clicked()
                {
                    self.parse_logs(sidebar_database);
                }
                if let Some(rx) = &self.parse_to_ui_rx {
                    match rx.try_recv() {
                        Ok(Ok((store_dir, msg))) => {
                            self.parse_text = msg;
                            self.parse_to_ui_rx = None;
                            action_queue.push(action::AppAction::AttachStore(store_dir));
                        }
                        Ok(Err(error)) => {
                            self.parse_text = format!("Error: {error}");
                            self.parse_to_ui_rx = None;
                        }
                        Err(mpsc::TryRecvError::Disconnected) => {
                            self.parse_text =
                                "Error: Log importer stopped before reporting a result".to_owned();
                            self.parse_to_ui_rx = None;
                        }
                        Err(mpsc::TryRecvError::Empty) => {}
                    }
                }
                if !self.parse_text.is_empty() {
                    ui.add(eframe::egui::Label::new(&self.parse_text).wrap());
                }
            });
        egui_tiles::UiResponse::None
    }
}
