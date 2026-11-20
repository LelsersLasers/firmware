use clap::Parser as _;
use daqcloud::{AppState, build_router, consts, db};
use std::{env, net::SocketAddr, path::PathBuf};

/// PER DAQ schema-agnostic central partition, raw log, SuperDBC, and annotation archive server.
#[derive(clap::Parser)]
#[command(name = "daqcloud")]
struct Cli {
    /// Store directory containing `partitions/`, `raw/`, and `superdbcs/`
    #[arg(long)]
    store_dir: Option<PathBuf>,
    /// Path to the SQLite metadata registry (`cloud_registry.sqlite`)
    #[arg(long)]
    db_path: Option<PathBuf>,
    /// Address and port to bind the HTTP/WebSocket server on
    #[arg(long)]
    bind: Option<SocketAddr>,
    /// Maximum combined in-memory cache size in MiB for tiles and partition preloads (0 to disable)
    #[arg(long)]
    max_cache_mb: Option<usize>,
    /// Shared password/token required for write endpoints (PUT/POST/PATCH/DELETE). Can also be set via DAQCLOUD_WRITE_TOKEN.
    #[arg(long)]
    write_token: Option<String>,
    /// Automatically allow write requests originating from Tailscale IPs (100.64.0.0/10, fd7a:115c:a1e0::/48)
    #[arg(long)]
    trust_tailscale: Option<bool>,
}

#[tokio::main(worker_threads = 2)]
async fn main() -> Result<(), db::Error> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let cli = Cli::parse();

    let store_dir = cli
        .store_dir
        .or_else(|| env::var("DAQCLOUD_STORE_DIR").ok().map(PathBuf::from))
        .unwrap_or_else(|| {
            let local = PathBuf::from(consts::STORE_DIR);
            if local.exists() {
                return local;
            }
            let manifest_store = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join(consts::STORE_DIR);
            if manifest_store.exists() {
                return manifest_store;
            }
            local
        });
    let db_path = cli
        .db_path
        .or_else(|| env::var("DAQCLOUD_DB_PATH").ok().map(PathBuf::from))
        .unwrap_or_else(|| {
            let local = PathBuf::from(consts::DATABASE_URL);
            if local.exists() {
                return local;
            }
            let in_store = store_dir.join(consts::DATABASE_URL);
            if in_store.exists() {
                return in_store;
            }
            let manifest_db = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join(consts::DATABASE_URL);
            if manifest_db.exists() {
                return manifest_db;
            }
            store_dir.join(consts::DATABASE_URL)
        });
    let bind_addr: SocketAddr = cli
        .bind
        .or_else(|| {
            env::var("DAQCLOUD_BIND_ADDR")
                .ok()
                .and_then(|s| s.parse().ok())
        })
        .unwrap_or(consts::BIND_ADDR);

    let max_cache_mb: usize = cli
        .max_cache_mb
        .or_else(|| {
            env::var("DAQCLOUD_MAX_CACHE_MB")
                .ok()
                .and_then(|s| s.parse().ok())
        })
        .unwrap_or(consts::DEFAULT_MAX_CACHE_MB);

    let write_token: Option<String> = cli
        .write_token
        .or_else(|| env::var("DAQCLOUD_WRITE_TOKEN").ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());

    let trust_tailscale: bool = cli
        .trust_tailscale
        .or_else(|| {
            env::var("DAQCLOUD_TRUST_TAILSCALE")
                .ok()
                .and_then(|s| match s.trim().to_ascii_lowercase().as_str() {
                    "1" | "true" | "yes" => Some(true),
                    "0" | "false" | "no" => Some(false),
                    _ => None,
                })
        })
        .unwrap_or(true);

    let state = AppState::open_with_cache_budget(
        &db_path,
        &store_dir,
        max_cache_mb.saturating_mul(1024 * 1024),
    )?
    .with_write_auth(write_token, trust_tailscale);
    let write_protected = state.write_token().is_some();
    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    log::info!(
        "daqcloud listening on http://{} (db: {}, store: {}, max_cache: {} MiB, write_auth: {}, trust_tailscale: {})",
        listener.local_addr()?,
        db_path.display(),
        store_dir.display(),
        max_cache_mb,
        if write_protected { "enabled" } else { "open" },
        trust_tailscale
    );

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            log::error!("Could not listen for shutdown: {error}");
        }
    })
    .await?;
    Ok(())
}
