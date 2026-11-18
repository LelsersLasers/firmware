use clap::Parser as _;
use daqnative::{
    export::{
        ExportError, ExportFormat, ExportPresetRegistry, ExportSummary, SamplingMode,
        export_dataset, resolve_annotation_time_range,
    },
    router::VirtualTimelineRouter,
    storage::NativeRouterExt as _,
    superdbc::database::Database,
    time::Time,
};
use std::{ops::RangeInclusive, path, process};

#[derive(clap::Parser)]
#[command(
    name = "daqcli",
    about = "PER DAQ command-line ingestion and export tool"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Import raw SD card .log files into time-correlated Parquet partition pyramids
    Import {
        #[arg(long)]
        logs_dir: path::PathBuf,
        #[arg(long)]
        store_dir: path::PathBuf,
        #[arg(long)]
        superdbc: path::PathBuf,
    },
    /// Export a synchronized time-range or event dataset using a subteam preset or signal globs
    Export {
        /// Path to the local Parquet partition store directory
        #[arg(long)]
        store_dir: path::PathBuf,
        /// Optional event/stint annotation name or ID (e.g. "Driving Stint 1")
        #[arg(long)]
        event: Option<String>,
        /// Optional explicit start timestamp in UTC milliseconds
        #[arg(long)]
        start_ms: Option<i64>,
        /// Optional explicit end timestamp in UTC milliseconds
        #[arg(long)]
        end_ms: Option<i64>,
        /// Subteam export preset ID (vd, aero, powertrain_thermals, controls, raw_all)
        #[arg(long)]
        preset: Option<String>,
        /// Optional comma-separated signal IDs or glob patterns (e.g. "IMU_acceleration.*,battery.power_kw")
        #[arg(long)]
        signals: Option<String>,
        /// Output format: parquet, csv, or mat (inferred from -o extension or defaults to parquet)
        #[arg(long)]
        format: Option<String>,
        /// Optional uniform grid sampling rate override in Hz (e.g. 100.0)
        #[arg(long)]
        rate_hz: Option<f64>,
        /// Export at sparse raw union timestamps without uniform grid resampling
        #[arg(long, default_value_t = false)]
        sparse: bool,
        /// Output file path (.parquet, .csv, or .mat)
        #[arg(short = 'o', long = "output")]
        output: path::PathBuf,
    },
    /// Synchronize a local Parquet partition store directory with a remote daqcloud server
    Sync {
        /// Path to the local Parquet partition store directory
        #[arg(long)]
        store_dir: path::PathBuf,
        /// Base URL of the daqcloud server (e.g. "http://127.0.0.1:3000")
        #[arg(long, default_value = "http://127.0.0.1:3000")]
        cloud_url: String,
        /// Also download all remote full_res.parquet and overview_10hz.parquet files upfront
        #[arg(long, default_value_t = false)]
        download_full: bool,
    },
    /// Process and import all PER26 / PER25 datasets (raw logs and parsed CSVs) into a store directory
    ImportPer26 {
        /// Path to the PER26 dataset root directory
        #[arg(long, default_value = "PER26")]
        per26_dir: path::PathBuf,
        /// Path to the output Parquet partition store directory
        #[arg(long)]
        store_dir: path::PathBuf,
        /// Path to the directory containing era-specific superdbc_*.json files
        #[arg(long, default_value = "superdbc")]
        superdbc_dir: path::PathBuf,
    },
    /// Normalize all existing PER26 partitions in a store directory to the canonical superdbc_per26.json schema
    NormalizePer26 {
        /// Path to the Parquet partition store directory (e.g. ./cloud_store)
        #[arg(long)]
        store_dir: path::PathBuf,
        /// Path to the canonical superdbc_per26.json file
        #[arg(long, default_value = "../superdbc/superdbc_per26.json")]
        superdbc: path::PathBuf,
    },
}

/// Configuration for executing a headless CLI dataset export.
#[derive(Debug, Clone)]
pub struct ExportCommandArgs {
    pub store_dir: path::PathBuf,
    pub event: Option<String>,
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
    pub preset: Option<String>,
    pub signals: Option<String>,
    pub format: Option<String>,
    pub rate_hz: Option<f64>,
    pub sparse: bool,
    pub output: path::PathBuf,
}

fn main() -> process::ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let cli = Cli::parse();

    match cli.command {
        Command::Import {
            logs_dir,
            store_dir,
            superdbc,
        } => run_import(&logs_dir, &store_dir, &superdbc),
        Command::Export {
            store_dir,
            event,
            start_ms,
            end_ms,
            preset,
            signals,
            format,
            rate_hz,
            sparse,
            output,
        } => {
            let args = ExportCommandArgs {
                store_dir,
                event,
                start_ms,
                end_ms,
                preset,
                signals,
                format,
                rate_hz,
                sparse,
                output,
            };
            match execute_export(&args) {
                Ok(summary) => {
                    log::info!(
                        "Export complete ({}): {} rows x {} signals over [{} .. {}] ms -> {}",
                        summary.format.label(),
                        summary.row_count,
                        summary.signal_count,
                        summary.start_utc_ms,
                        summary.end_utc_ms,
                        summary.output_path.display()
                    );
                    process::ExitCode::SUCCESS
                }
                Err(err) => {
                    log::error!("Dataset export failed: {err}");
                    process::ExitCode::FAILURE
                }
            }
        }
        Command::Sync {
            store_dir,
            cloud_url,
            download_full,
        } => match daqnative::storage::sync_store_with_cloud_blocking(
            &store_dir,
            &cloud_url,
            download_full,
        ) {
            Ok(summary) => {
                log::info!(
                    "Cloud sync complete ({}): {} remote track days, {} remote partitions ({} full-res downloaded), {} local partitions uploaded, {} annotations pulled",
                    cloud_url,
                    summary.remote_track_days,
                    summary.remote_partitions,
                    summary.full_partitions_downloaded,
                    summary.partitions_uploaded,
                    summary.annotations_pulled
                );
                process::ExitCode::SUCCESS
            }
            Err(err) => {
                log::error!("Cloud sync failed: {err}");
                process::ExitCode::FAILURE
            }
        },
        Command::ImportPer26 {
            per26_dir,
            store_dir,
            superdbc_dir,
        } => {
            match daqnative::log_parse::per26::import_all_per26(
                &per26_dir,
                &store_dir,
                &superdbc_dir,
            ) {
                Ok(report) => {
                    log::info!(
                        "PER26 import complete: {} track days, {} partitions written ({} skipped existing), {} annotations -> {}",
                        report.track_days_imported,
                        report.partitions_written,
                        report.partitions_skipped_existing,
                        report.annotations_created,
                        store_dir.display()
                    );
                    process::ExitCode::SUCCESS
                }
                Err(err) => {
                    log::error!("PER26 import failed: {err}");
                    process::ExitCode::FAILURE
                }
            }
        }
        Command::NormalizePer26 {
            store_dir,
            superdbc,
        } => match daqnative::log_parse::per26::normalize_per26_store(&store_dir, &superdbc) {
            Ok(count) => {
                log::info!(
                    "Normalized {count} partitions in {} to canonical schema {}",
                    store_dir.display(),
                    superdbc.display()
                );
                process::ExitCode::SUCCESS
            }
            Err(err) => {
                log::error!("PER26 store normalization failed: {err}");
                process::ExitCode::FAILURE
            }
        },
    }
}

fn run_import(
    logs_dir: &path::Path,
    store_dir: &path::Path,
    superdbc: &path::Path,
) -> process::ExitCode {
    let database = match Database::load(superdbc) {
        Ok(database) => database,
        Err(error) => {
            log::error!("SuperDBC load failed ({}): {error}", superdbc.display());
            return process::ExitCode::FAILURE;
        }
    };

    match daqnative::log_parse::import_logs_to_store(logs_dir, store_dir, &database) {
        Ok(summary) => {
            let annotation_count: usize =
                summary.manifests.iter().map(|m| m.annotations.len()).sum();
            log::info!(
                "Import complete: {} .log files -> {} boot epochs -> {} partitions written ({} skipped existing), {} auto-annotations",
                summary.log_files_parsed,
                summary.boot_epochs_detected,
                summary.partitions_written,
                summary.partitions_skipped_existing,
                annotation_count
            );
            process::ExitCode::SUCCESS
        }
        Err(e) => {
            log::error!("Log import failed: {e}");
            process::ExitCode::FAILURE
        }
    }
}

/// Execute a headless dataset export from `args`.
pub fn execute_export(args: &ExportCommandArgs) -> Result<ExportSummary, ExportError> {
    let router = VirtualTimelineRouter::open_store(&args.store_dir, Time::now(), 30.0)
        .map_err(ExportError::from)?;

    let range = resolve_requested_range(&router, args)?;
    let (signal_patterns, sampling_mode) = resolve_signals_and_sampling(args)?;
    let export_format = resolve_export_format(args)?;

    export_dataset(
        &router,
        range,
        &signal_patterns,
        sampling_mode,
        export_format,
        &args.output,
    )
}

fn resolve_requested_range(
    router: &VirtualTimelineRouter,
    args: &ExportCommandArgs,
) -> Result<RangeInclusive<i64>, ExportError> {
    if let Some(event_query) = &args.event {
        let (_, range) = resolve_annotation_time_range(router, event_query)?;
        let start_ms = args.start_ms.unwrap_or(*range.start());
        let end_ms = args.end_ms.unwrap_or(*range.end());
        return Ok(start_ms..=end_ms);
    }

    let day_bounds = router.timeline().day_bounds_ms().or_else(|| {
        let min_ms = router.partitions().iter().map(|p| p.start_utc_ms).min()?;
        let max_ms = router.partitions().iter().map(|p| p.end_utc_ms).max()?;
        Some((min_ms, max_ms))
    });

    match (args.start_ms, args.end_ms, day_bounds) {
        (Some(s), Some(e), _) => Ok(s..=e),
        (Some(s), None, Some((_, day_end))) => Ok(s..=day_end),
        (None, Some(e), Some((day_start, _))) => Ok(day_start..=e),
        (None, None, Some((day_start, day_end))) => Ok(day_start..=day_end),
        _ => Err(ExportError::EmptyDataset),
    }
}

fn resolve_signals_and_sampling(
    args: &ExportCommandArgs,
) -> Result<(Vec<String>, SamplingMode), ExportError> {
    let presets = ExportPresetRegistry::with_builtin();
    let mut patterns = Vec::new();
    let mut default_sampling = SamplingMode::UniformGridHz(100.0);

    let preset_id = match (&args.preset, &args.signals) {
        (Some(id), _) => Some(id.as_str()),
        (None, None) => Some("vd"),
        (None, Some(_)) => None,
    };

    if let Some(id) = preset_id {
        let preset = presets
            .get(id)
            .ok_or_else(|| ExportError::UnknownPreset(id.to_owned()))?;
        patterns.extend(preset.signals.iter().cloned());
        default_sampling = preset.default_sampling;
    }

    if let Some(extra_csv) = &args.signals {
        for token in extra_csv.split(',') {
            let trimmed = token.trim();
            if !trimmed.is_empty() {
                patterns.push(trimmed.to_owned());
            }
        }
    }

    let sampling = if args.sparse {
        SamplingMode::SparseRaw
    } else if let Some(hz) = args.rate_hz {
        SamplingMode::UniformGridHz(hz)
    } else {
        default_sampling
    };

    Ok((patterns, sampling))
}

fn resolve_export_format(args: &ExportCommandArgs) -> Result<ExportFormat, ExportError> {
    if let Some(fmt_str) = &args.format {
        return ExportFormat::parse_format(fmt_str).ok_or_else(|| {
            ExportError::ConfigParse(format!(
                "unsupported export format '{fmt_str}' (expected parquet, csv, or mat)"
            ))
        });
    }
    if let Some(ext) = args.output.extension().and_then(|e| e.to_str())
        && let Some(fmt) = ExportFormat::parse_format(ext)
    {
        return Ok(fmt);
    }
    Ok(ExportFormat::Parquet)
}

#[cfg(test)]
mod tests {
    use super::*;
    use daqnative::export::{read_exported_parquet_info, read_matlab_mat_vars};
    use std::fs;

    #[test]
    fn cli_export_preset_and_event_across_parquet_csv_and_mat_on_per26() {
        let manifest_dir = path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let dbc_path = manifest_dir.join("../../superdbc/superdbc_per26.json");
        let log_path =
            manifest_dir.join("../../PER26/2026-09-06/2_regen/raw/log-2026-09-06--20-21-44.log");
        if !dbc_path.is_file() || !log_path.is_file() {
            return;
        }

        let db = Database::load(&dbc_path).expect("load PER26 database");
        let tmp_raw = tempfile::tempdir().expect("temp raw dir");
        fs::copy(
            &log_path,
            tmp_raw.path().join("log-2026-09-06--20-21-44.log"),
        )
        .expect("copy PER26 log");

        let tmp_store = tempfile::tempdir().expect("temp store dir");
        let summary =
            daqnative::log_parse::import_logs_to_store(tmp_raw.path(), tmp_store.path(), &db)
                .expect("import PER26 log");
        let m0 = &summary.manifests[0];
        let win_start = m0.start_utc_ms;
        let win_end = (m0.start_utc_ms + 5_000).min(m0.end_utc_ms);

        // Register a named "Driving Stint 1" annotation so `--event "Driving Stint 1"` resolves it
        {
            let mut router = VirtualTimelineRouter::open_store(
                tmp_store.path(),
                Time::from_unix_millis(m0.end_utc_ms),
                30.0,
            )
            .expect("open store");
            let track_day_id = router
                .timeline()
                .active_track_day_id()
                .unwrap_or("2026-09-06")
                .to_owned();
            router
                .upsert_annotation(&daqcore::storage::Annotation {
                    id: "stint_1".to_owned(),
                    track_day_id,
                    name: "Driving Stint 1".to_owned(),
                    category: "driving_stint".to_owned(),
                    color: "#2ECC71".to_owned(),
                    start_utc_ms: win_start,
                    end_utc_ms: Some(win_end),
                    author: "daqcli_test".to_owned(),
                    source: "manual".to_owned(),
                    notes: String::new(),
                    updated_at_ms: win_end,
                    deleted: false,
                })
                .expect("upsert Driving Stint 1");
        }

        let tmp_out = tempfile::tempdir().expect("temp out dir");
        let pq_out = tmp_out.path().join("stint1_vd.parquet");
        let csv_out = tmp_out.path().join("stint1_pt.csv");
        let mat_out = tmp_out.path().join("stint1_aero.mat");

        // 1. Export "Driving Stint 1" with preset "vd" to Parquet (100 Hz -> 501 rows for 5.0 s)
        let pq_summary = execute_export(&ExportCommandArgs {
            store_dir: tmp_store.path().to_path_buf(),
            event: Some("Driving Stint 1".to_owned()),
            start_ms: None,
            end_ms: None,
            preset: Some("vd".to_owned()),
            signals: None,
            format: Some("parquet".to_owned()),
            rate_hz: None,
            sparse: false,
            output: pq_out.clone(),
        })
        .expect("execute_export parquet");
        assert_eq!(pq_summary.row_count, 501);
        let (rows, cols) = read_exported_parquet_info(&pq_out).expect("read parquet info");
        assert_eq!(rows, 501);
        assert!(cols.iter().any(|(n, _)| n == "IMU_acceleration.X_axis"));
        assert!(cols.iter().any(|(n, _)| n == "battery.power_kw"));

        // 2. Export "Driving Stint 1" with preset "powertrain_thermals" to CSV (50 Hz -> 251 rows)
        let csv_summary = execute_export(&ExportCommandArgs {
            store_dir: tmp_store.path().to_path_buf(),
            event: Some("Driving Stint 1".to_owned()),
            start_ms: None,
            end_ms: None,
            preset: Some("powertrain_thermals".to_owned()),
            signals: None,
            format: Some("csv".to_owned()),
            rate_hz: None,
            sparse: false,
            output: csv_out.clone(),
        })
        .expect("execute_export csv");
        assert_eq!(csv_summary.row_count, 251);
        let csv_content = fs::read_to_string(&csv_out).expect("read csv");
        assert_eq!(csv_content.lines().count(), 252); // header + 251 rows

        // 3. Export explicit window with preset "aero" to MATLAB .mat (50 Hz -> 251 rows)
        let mat_summary = execute_export(&ExportCommandArgs {
            store_dir: tmp_store.path().to_path_buf(),
            event: None,
            start_ms: Some(win_start),
            end_ms: Some(win_end),
            preset: Some("aero".to_owned()),
            signals: Some("battery.power_kw".to_owned()),
            format: None, // inferred from .mat extension
            rate_hz: None,
            sparse: false,
            output: mat_out.clone(),
        })
        .expect("execute_export mat");
        assert_eq!(mat_summary.row_count, 251);
        let mat_vars = read_matlab_mat_vars(&mat_out).expect("read mat vars");
        assert_eq!(mat_vars.get("utc_ms").map(Vec::len), Some(251));
        assert_eq!(mat_vars.get("battery_power_kw").map(Vec::len), Some(251));
    }
}
