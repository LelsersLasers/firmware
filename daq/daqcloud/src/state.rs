use crate::db;
use bytes::Bytes;
use daqcore::{
    router::{DEFAULT_TILE_CACHE_BYTES, LruTileMap, SignalSeries, TileKey},
    storage::CloudWsMessage,
};
use daqnative::storage::{Registry, compute_file_sha256_hex};
use std::{
    collections::HashMap,
    env, fs, io, path,
    sync::{Arc, Mutex},
};
use tokio::sync::broadcast;

const WS_BROADCAST_CAPACITY: usize = 512;

fn resolve_default_web_dist_dir() -> path::PathBuf {
    if let Ok(custom) = env::var("DAQCLOUD_WEB_DIST_DIR") {
        return path::PathBuf::from(custom);
    }
    let candidates = [
        path::PathBuf::from("dist"),
        path::PathBuf::from("daqweb/dist"),
        path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../daqweb/dist"),
    ];
    for c in &candidates {
        if c.is_dir() {
            return c.clone();
        }
    }
    candidates[2].clone()
}

#[derive(Clone)]
pub struct AppState {
    pub registry: Registry,
    pub store_dir: path::PathBuf,
    pub web_dist_dir: path::PathBuf,
    pub ws_tx: broadcast::Sender<CloudWsMessage>,
    sha256_cache: Arc<Mutex<HashMap<(path::PathBuf, u64), String>>>,
    tile_cache: Arc<Mutex<LruTileMap>>,
    preload_cache: Arc<Mutex<HashMap<String, Bytes>>>,
}

impl AppState {
    pub fn new(registry: Registry, store_dir: impl Into<path::PathBuf>) -> Result<Self, db::Error> {
        let store_dir = store_dir.into();
        fs::create_dir_all(store_dir.join("partitions"))?;
        fs::create_dir_all(store_dir.join("raw"))?;
        fs::create_dir_all(store_dir.join("superdbcs"))?;
        let _ = registry.sync_from_store_dir(&store_dir)?;
        let (ws_tx, _) = broadcast::channel(WS_BROADCAST_CAPACITY);
        Ok(Self {
            registry,
            store_dir,
            web_dist_dir: resolve_default_web_dist_dir(),
            ws_tx,
            sha256_cache: Arc::new(Mutex::new(HashMap::new())),
            tile_cache: Arc::new(Mutex::new(LruTileMap::new(DEFAULT_TILE_CACHE_BYTES))),
            preload_cache: Arc::new(Mutex::new(HashMap::new())),
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

    pub fn get_cached_preload(&self, partition_id: &str) -> Option<Bytes> {
        self.preload_cache
            .lock()
            .ok()
            .and_then(|g| g.get(partition_id).cloned())
    }

    pub fn insert_cached_preload(&self, partition_id: String, payload: Bytes) {
        if let Ok(mut g) = self.preload_cache.lock() {
            g.insert(partition_id, payload);
        }
    }

    pub fn get_cached_tile(&self, key: &TileKey) -> Option<Arc<SignalSeries>> {
        self.tile_cache.lock().ok().and_then(|mut g| g.get(key))
    }

    pub fn insert_cached_tile(&self, key: TileKey, series: Arc<SignalSeries>) {
        if let Ok(mut g) = self.tile_cache.lock() {
            g.insert(key, series);
        }
    }

    pub fn invalidate_partition_cache(&self, partition_id: &str) {
        if let Ok(mut g) = self.preload_cache.lock() {
            g.remove(partition_id);
        }
    }
}
