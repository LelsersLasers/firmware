use crate::db;
use daqcore::storage::{CloudWsMessage, Registry, compute_file_sha256_hex};
use std::{
    collections::HashMap,
    fs, io, path,
    sync::{Arc, Mutex},
};
use tokio::sync::broadcast;

const WS_BROADCAST_CAPACITY: usize = 512;

#[derive(Clone)]
pub struct AppState {
    pub registry: Registry,
    pub store_dir: path::PathBuf,
    pub ws_tx: broadcast::Sender<CloudWsMessage>,
    sha256_cache: Arc<Mutex<HashMap<(path::PathBuf, u64), String>>>,
}

impl AppState {
    pub fn new(registry: Registry, store_dir: impl Into<path::PathBuf>) -> Result<Self, db::Error> {
        let store_dir = store_dir.into();
        fs::create_dir_all(store_dir.join("partitions"))?;
        fs::create_dir_all(store_dir.join("raw"))?;
        let _ = registry.sync_from_store_dir(&store_dir)?;
        let (ws_tx, _) = broadcast::channel(WS_BROADCAST_CAPACITY);
        Ok(Self {
            registry,
            store_dir,
            ws_tx,
            sha256_cache: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub fn open(
        database_path: impl AsRef<path::Path>,
        store_dir: impl AsRef<path::Path>,
    ) -> Result<Self, db::Error> {
        let registry = db::init_registry(database_path)?;
        Self::new(registry, store_dir.as_ref().to_path_buf())
    }

    pub fn cached_file_sha256(&self, path: &path::Path) -> io::Result<(u64, String)> {
        let meta = fs::metadata(path)?;
        let size = meta.len();
        let key = (path.to_path_buf(), size);
        if let Ok(guard) = self.sha256_cache.lock()
            && let Some(cached) = guard.get(&key)
        {
            return Ok((size, cached.clone()));
        }
        let (computed_size, digest) = compute_file_sha256_hex(path)?;
        if let Ok(mut guard) = self.sha256_cache.lock() {
            guard.insert((path.to_path_buf(), computed_size), digest.clone());
        }
        Ok((computed_size, digest))
    }

    pub fn insert_cached_sha256(&self, path: path::PathBuf, size: u64, sha256_hex: String) {
        if let Ok(mut guard) = self.sha256_cache.lock() {
            guard.retain(|(p, _), _| p != &path);
            guard.insert((path, size), sha256_hex);
        }
    }
}
