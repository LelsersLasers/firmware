use clap::Parser as _;
use std::{path, process};

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
}

fn main() -> process::ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let cli = Cli::parse();

    match cli.command {
        Command::Import {
            logs_dir,
            store_dir,
            superdbc,
        } => {
            let database = match daqcore::superdbc::database::Database::load(&superdbc) {
                Ok(database) => database,
                Err(error) => {
                    log::error!("SuperDBC load failed ({}): {error}", superdbc.display());
                    return process::ExitCode::FAILURE;
                }
            };

            match daqcore::log_parse::import_logs_to_store(&logs_dir, &store_dir, &database) {
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
    }
}
