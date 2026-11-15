use daqcloud::{AppState, build_router, consts, db};
use std::{env, net::SocketAddr, path::PathBuf};

#[tokio::main]
async fn main() -> Result<(), db::Error> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let db_path = env::var("DAQCLOUD_DB_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(consts::DATABASE_URL));
    let store_dir = env::var("DAQCLOUD_STORE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(consts::STORE_DIR));
    let bind_addr: SocketAddr = env::var("DAQCLOUD_BIND_ADDR")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(consts::BIND_ADDR);

    let state = AppState::open(&db_path, &store_dir)?;
    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    log::info!(
        "daqcloud listening on http://{} (db: {}, store: {})",
        listener.local_addr()?,
        db_path.display(),
        store_dir.display()
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            if let Err(error) = tokio::signal::ctrl_c().await {
                log::error!("Could not listen for shutdown: {error}");
            }
        })
        .await?;
    Ok(())
}
