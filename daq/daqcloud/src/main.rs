use daqcloud::{AppState, build_router, consts, db};
use std::{env, net::SocketAddr, path::PathBuf};

#[tokio::main]
async fn main() -> Result<(), db::Error> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let store_dir = env::var("DAQCLOUD_STORE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
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
    let db_path = env::var("DAQCLOUD_DB_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
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
