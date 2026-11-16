use daqcore::{
    export::{
        ExportError, ExportFormat, ExportPresetRegistry, ExportSummary, SamplingMode,
        discover_available_signals, expand_signal_patterns, export_dataset,
        resolve_annotation_time_range,
    },
    router::VirtualTimelineRouter,
};
use std::{collections::BTreeSet, ops::RangeInclusive, path::PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiExportRangeMode {
    CurrentViewport,
    Annotation(String),
}

pub struct ExportModalState {
    pub is_open: bool,
    pub range_mode: UiExportRangeMode,
    pub preset_registry: ExportPresetRegistry,
    pub selected_preset_id: String,
    pub selected_signals: BTreeSet<String>,
    pub signal_filter: String,
    pub show_signal_checklist: bool,
    pub format: ExportFormat,
    pub sparse_mode: bool,
    pub rate_hz: f64,
    pub output_path: String,
    pub status_banner: Option<Result<String, String>>,
}

impl Default for ExportModalState {
    fn default() -> Self {
        let preset_registry = ExportPresetRegistry::with_builtin();
        let default_signals: BTreeSet<String> = preset_registry
            .get("vd")
            .map(|p| p.signals.iter().cloned().collect())
            .unwrap_or_default();
        Self {
            is_open: false,
            range_mode: UiExportRangeMode::CurrentViewport,
            preset_registry,
            selected_preset_id: "vd".to_owned(),
            selected_signals: default_signals,
            signal_filter: String::new(),
            show_signal_checklist: false,
            format: ExportFormat::Parquet,
            sparse_mode: false,
            rate_hz: 100.0,
            output_path: "/tmp/per_daq_export.parquet".to_owned(),
            status_banner: None,
        }
    }
}

impl ExportModalState {
    pub fn open(&mut self, router: &VirtualTimelineRouter) {
        self.is_open = true;
        self.status_banner = None;
        if let Some(first_selected) = router.timeline().selected_annotation_ids().first() {
            self.range_mode = UiExportRangeMode::Annotation(first_selected.clone());
        } else {
            self.range_mode = UiExportRangeMode::CurrentViewport;
        }
        let preset_id = self.selected_preset_id.clone();
        self.apply_preset(&preset_id, router);
        self.sync_output_extension();
    }

    pub fn active_range(
        &self,
        router: &VirtualTimelineRouter,
    ) -> Result<RangeInclusive<i64>, ExportError> {
        match &self.range_mode {
            UiExportRangeMode::CurrentViewport => {
                let s = router.timeline().start_utc_ms();
                let e = router.timeline().end_utc_ms();
                Ok(s..=e.max(s))
            }
            UiExportRangeMode::Annotation(query) => {
                let (_name, range) = resolve_annotation_time_range(router, query)?;
                Ok(range)
            }
        }
    }

    pub fn apply_preset(&mut self, preset_id: &str, router: &VirtualTimelineRouter) {
        self.selected_preset_id = preset_id.to_owned();
        let Some(preset) = self.preset_registry.get(preset_id).cloned() else {
            return;
        };
        match preset.default_sampling {
            SamplingMode::UniformGridHz(hz) => {
                self.sparse_mode = false;
                self.rate_hz = hz;
            }
            SamplingMode::SparseRaw => {
                self.sparse_mode = true;
            }
        }
        let range = self.active_range(router).unwrap_or_else(|_| {
            let s = router.timeline().start_utc_ms();
            let e = router.timeline().end_utc_ms();
            s..=e.max(s)
        });
        if let Ok(expanded) = expand_signal_patterns(&preset.signals, router, range) {
            self.selected_signals = expanded.into_iter().map(|m| m.signal_id).collect();
        } else {
            self.selected_signals.clear();
        }
    }

    pub fn set_format(&mut self, format: ExportFormat) {
        self.format = format;
        self.sync_output_extension();
    }

    fn sync_output_extension(&mut self) {
        let trimmed = self.output_path.trim();
        if trimmed.is_empty() {
            self.output_path = format!("/tmp/per_daq_export.{}", self.format.extension());
            return;
        }
        let mut path = PathBuf::from(trimmed);
        path.set_extension(self.format.extension());
        self.output_path = path.to_string_lossy().into_owned();
    }

    pub fn execute_export(
        &mut self,
        router: &mut VirtualTimelineRouter,
    ) -> Result<ExportSummary, ExportError> {
        let range = self.active_range(router)?;
        let sampling_mode = if self.sparse_mode {
            SamplingMode::SparseRaw
        } else {
            SamplingMode::UniformGridHz(self.rate_hz)
        };
        let signal_patterns: Vec<String> = self.selected_signals.iter().cloned().collect();

        match export_dataset(
            router,
            range,
            &signal_patterns,
            sampling_mode,
            self.format,
            self.output_path.trim(),
        ) {
            Ok(summary) => {
                self.status_banner = Some(Ok(format!(
                    "Exported {} rows × {} signals ({:.2} s) to {}",
                    summary.row_count,
                    summary.signal_count,
                    (summary.end_utc_ms - summary.start_utc_ms) as f64 / 1000.0,
                    summary.output_path.display()
                )));
                Ok(summary)
            }
            Err(err) => {
                self.status_banner = Some(Err(err.to_string()));
                Err(err)
            }
        }
    }

    pub fn show(&mut self, ctx: &eframe::egui::Context, router: &mut VirtualTimelineRouter) {
        if !self.is_open {
            return;
        }

        let mut keep_open = self.is_open;
        let mut trigger_export = false;
        let mut preset_to_apply: Option<String> = None;

        let (day_min, day_max) = router
            .timeline()
            .day_bounds_ms()
            .or_else(|| {
                router
                    .captured_time_span()
                    .map(|(s, e)| (s.unix_millis(), e.unix_millis()))
            })
            .unwrap_or_else(|| {
                (
                    router.timeline().start_utc_ms(),
                    router.timeline().end_utc_ms(),
                )
            });
        let annotations = router
            .query_annotations_in_range(
                day_min.min(router.timeline().start_utc_ms())
                    ..=day_max.max(router.timeline().end_utc_ms()),
            )
            .unwrap_or_default();

        eframe::egui::Window::new("📤 Export Dataset (Parquet / CSV / MATLAB .mat)")
            .open(&mut keep_open)
            .collapsible(false)
            .resizable(true)
            .default_width(540.0)
            .show(ctx, |ui| {
                ui.label(
                    eframe::egui::RichText::new(
                        "Export time-aligned physical signals and derived formulas in 3 clicks.",
                    )
                    .weak(),
                );
                ui.separator();

                // ---------------------------------------------------------
                // Step 1: Time Range
                // ---------------------------------------------------------
                ui.heading("1. Time Range");
                let vp_start = router.timeline().start_utc_ms();
                let vp_end = router.timeline().end_utc_ms();
                let vp_dur_s = ((vp_end - vp_start).max(0)) as f64 / 1000.0;
                if ui
                    .radio(
                        matches!(self.range_mode, UiExportRangeMode::CurrentViewport),
                        format!(
                            "Current Viewport ({vp_dur_s:.2} s window: [{vp_start} .. {vp_end}] ms)"
                        ),
                    )
                    .clicked()
                {
                    self.range_mode = UiExportRangeMode::CurrentViewport;
                    preset_to_apply = Some(self.selected_preset_id.clone());
                }

                ui.horizontal(|ui| {
                    let is_ann = matches!(self.range_mode, UiExportRangeMode::Annotation(_));
                    if ui.radio(is_ann, "Event / Stint Annotation:").clicked() && !is_ann {
                        let default_ann = annotations
                            .first()
                            .map(|a| a.name.clone())
                            .unwrap_or_else(|| "Stint 1".to_owned());
                        self.range_mode = UiExportRangeMode::Annotation(default_ann);
                        preset_to_apply = Some(self.selected_preset_id.clone());
                    }

                    let current_ann_label = match &self.range_mode {
                        UiExportRangeMode::Annotation(q) => q.clone(),
                        UiExportRangeMode::CurrentViewport => annotations
                            .first()
                            .map(|a| a.name.clone())
                            .unwrap_or_else(|| "Select Stint / Event...".to_owned()),
                    };

                    eframe::egui::ComboBox::from_id_salt("export_modal_annotation_combo")
                        .selected_text(current_ann_label)
                        .width(280.0)
                        .show_ui(ui, |ui| {
                            if annotations.is_empty() {
                                ui.weak("No annotations in active day");
                            }
                            for ann in &annotations {
                                let dur_s = ((ann.end_utc_ms.unwrap_or(ann.start_utc_ms)
                                    - ann.start_utc_ms)
                                    .max(0)) as f64
                                    / 1000.0;
                                let display = format!("{} ({dur_s:.1} s)", ann.name);
                                if ui
                                    .selectable_label(
                                        matches!(
                                            &self.range_mode,
                                            UiExportRangeMode::Annotation(q)
                                                if q == &ann.id || q == &ann.name
                                        ),
                                        display,
                                    )
                                    .clicked()
                                {
                                    self.range_mode =
                                        UiExportRangeMode::Annotation(ann.name.clone());
                                    preset_to_apply = Some(self.selected_preset_id.clone());
                                }
                            }
                        });
                });

                ui.separator();

                // ---------------------------------------------------------
                // Step 2: Subteam Preset & Signals
                // ---------------------------------------------------------
                ui.heading("2. Subteam Signal Bundle");
                ui.horizontal_wrapped(|ui| {
                    for preset in self.preset_registry.presets() {
                        let selected = self.selected_preset_id == preset.id;
                        if ui
                            .selectable_label(selected, &preset.name)
                            .on_hover_text(&preset.description)
                            .clicked()
                        {
                            preset_to_apply = Some(preset.id.clone());
                        }
                    }
                });

                ui.horizontal(|ui| {
                    ui.label(
                        eframe::egui::RichText::new(format!(
                            "{} signals selected",
                            self.selected_signals.len()
                        ))
                        .strong(),
                    );
                    let toggle_label = if self.show_signal_checklist {
                        "Hide Signal Checklist"
                    } else {
                        "Customize Signal Checklist..."
                    };
                    if ui.small_button(toggle_label).clicked() {
                        self.show_signal_checklist = !self.show_signal_checklist;
                    }
                });

                if self.show_signal_checklist {
                    let active_range = self.active_range(router).unwrap_or_else(|_| {
                        let s = router.timeline().start_utc_ms();
                        let e = router.timeline().end_utc_ms();
                        s..=e.max(s)
                    });
                    let all_signals = collect_available_signals(router, active_range);
                    ui.horizontal(|ui| {
                        ui.label("🔍");
                        ui.add(
                            eframe::egui::TextEdit::singleline(&mut self.signal_filter)
                                .hint_text("Filter signals (e.g. shockpots, vd., pack_stats)...")
                                .desired_width(240.0),
                        );
                        if ui.small_button("Select Filtered").clicked() {
                            let q = self.signal_filter.trim().to_ascii_lowercase();
                            for sig in &all_signals {
                                if q.is_empty() || sig.to_ascii_lowercase().contains(&q) {
                                    self.selected_signals.insert(sig.clone());
                                }
                            }
                        }
                        if ui.small_button("Clear All").clicked() {
                            self.selected_signals.clear();
                        }
                    });

                    let q = self.signal_filter.trim().to_ascii_lowercase();
                    eframe::egui::ScrollArea::vertical()
                        .max_height(150.0)
                        .id_salt("export_modal_signal_scroll")
                        .show(ui, |ui| {
                            for sig in all_signals {
                                if !q.is_empty() && !sig.to_ascii_lowercase().contains(&q) {
                                    continue;
                                }
                                let mut checked = self.selected_signals.contains(&sig);
                                if ui.checkbox(&mut checked, &sig).changed() {
                                    if checked {
                                        self.selected_signals.insert(sig);
                                    } else {
                                        self.selected_signals.remove(&sig);
                                    }
                                }
                            }
                        });
                }

                ui.separator();

                // ---------------------------------------------------------
                // Step 3: Format, Sampling Rate & Output Path
                // ---------------------------------------------------------
                ui.heading("3. Format & Sampling Rate");
                ui.horizontal(|ui| {
                    ui.label("Format:");
                    for fmt in [
                        ExportFormat::Parquet,
                        ExportFormat::Csv,
                        ExportFormat::MatlabMat,
                    ] {
                        if ui
                            .selectable_label(self.format == fmt, fmt.label())
                            .clicked()
                        {
                            self.set_format(fmt);
                        }
                    }
                });

                ui.horizontal_wrapped(|ui| {
                    ui.label("Sampling:");
                    if ui
                        .radio(!self.sparse_mode, "Uniform Time Grid (Hz):")
                        .clicked()
                    {
                        self.sparse_mode = false;
                    }
                    for preset_hz in [10.0, 50.0, 100.0, 200.0, 500.0] {
                        let active = !self.sparse_mode && (self.rate_hz - preset_hz).abs() < 1e-3;
                        if ui
                            .selectable_label(active, format!("{preset_hz:.0} Hz"))
                            .clicked()
                        {
                            self.sparse_mode = false;
                            self.rate_hz = preset_hz;
                        }
                    }
                    ui.add_enabled(
                        !self.sparse_mode,
                        eframe::egui::DragValue::new(&mut self.rate_hz)
                            .range(1.0..=2000.0)
                            .speed(1.0)
                            .suffix(" Hz"),
                    );
                    if ui
                        .radio(self.sparse_mode, "Sparse Raw Timestamps")
                        .on_hover_text(
                            "Union of exact raw sample timestamps without uniform resampling",
                        )
                        .clicked()
                    {
                        self.sparse_mode = true;
                    }
                });

                ui.horizontal(|ui| {
                    ui.label("Output File:");
                    ui.add(
                        eframe::egui::TextEdit::singleline(&mut self.output_path)
                            .desired_width(310.0),
                    );
                    if ui.button("Browse...").clicked() {
                        let ext = self.format.extension();
                        if let Some(picked) = rfd::FileDialog::new()
                            .add_filter(self.format.label(), &[ext])
                            .save_file()
                        {
                            self.output_path = picked.to_string_lossy().into_owned();
                        }
                    }
                });

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let can_export =
                        !self.selected_signals.is_empty() && !self.output_path.trim().is_empty();
                    if ui
                        .add_enabled(
                            can_export,
                            eframe::egui::Button::new(
                                eframe::egui::RichText::new("📤 Export Dataset").strong(),
                            ),
                        )
                        .clicked()
                    {
                        trigger_export = true;
                    }
                    if ui.button("Close").clicked() {
                        self.is_open = false;
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
        if let Some(preset_id) = preset_to_apply {
            self.apply_preset(&preset_id, router);
        }
        if trigger_export {
            let _ = self.execute_export(router);
        }
    }
}

fn collect_available_signals(
    router: &VirtualTimelineRouter,
    range: RangeInclusive<i64>,
) -> Vec<String> {
    let catalog = discover_available_signals(router, range);
    let set: BTreeSet<String> = catalog
        .into_iter()
        .filter(|(_, meta)| meta.has_samples_in_range)
        .map(|(id, _)| id)
        .collect();
    set.into_iter().collect()
}
