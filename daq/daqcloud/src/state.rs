use crate::{consts, db};
use bytes::Bytes;
use daqcore::{
    router::{LruTileMap, SignalSeries, TileKey},
    storage::CloudWsMessage,
};
use daqnative::storage::{Registry, compute_file_sha256_hex};
use std::{
    collections::{HashMap, VecDeque},
    env, fs, io, path,
    sync::{Arc, Mutex},
};
use tokio::sync::broadcast;

const WS_BROADCAST_CAPACITY: usize = 64;

struct PreloadLruCache {
    max_bytes: usize,
    used_bytes: usize,
    order: VecDeque<String>,
    entries: HashMap<String, Bytes>,
}

impl PreloadLruCache {
    fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            used_bytes: 0,
            order: VecDeque::new(),
            entries: HashMap::new(),
        }
    }

    fn get(&mut self, partition_id: &str) -> Option<Bytes> {
        let bytes = self.entries.get(partition_id)?.clone();
        if let Some(pos) = self.order.iter().position(|k| k == partition_id) {
            self.order.remove(pos);
        }
        self.order.push_back(partition_id.to_owned());
        Some(bytes)
    }

    fn insert(&mut self, partition_id: String, payload: Bytes) {
        let len = payload.len();
        if self.max_bytes == 0 || len > self.max_bytes {
            return;
        }
        self.remove(&partition_id);
        while self.used_bytes.saturating_add(len) > self.max_bytes {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(removed) = self.entries.remove(&oldest) {
                self.used_bytes = self.used_bytes.saturating_sub(removed.len());
            }
        }
        self.used_bytes = self.used_bytes.saturating_add(len);
        self.order.push_back(partition_id.clone());
        self.entries.insert(partition_id, payload);
    }

    fn remove(&mut self, partition_id: &str) {
        if let Some(removed) = self.entries.remove(partition_id) {
            self.used_bytes = self.used_bytes.saturating_sub(removed.len());
        }
        if let Some(pos) = self.order.iter().position(|k| k == partition_id) {
            self.order.remove(pos);
        }
    }
}

fn resolve_default_max_cache_bytes() -> usize {
    env::var("DAQCLOUD_MAX_CACHE_MB")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(consts::DEFAULT_MAX_CACHE_MB)
        .saturating_mul(1024 * 1024)
}

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

unsafe extern "C" {
    fn malloc_trim(pad: usize) -> i32;
}

pub fn trim_process_heap() {
    // SAFETY: `malloc_trim(0)` is a thread-safe glibc function that releases free heap pages back to the OS.
    unsafe {
        let _ = malloc_trim(0);
    }
}

#[derive(Clone)]
pub struct AppState {
    pub registry: Registry,
    pub store_dir: path::PathBuf,
    pub web_dist_dir: path::PathBuf,
    pub ws_tx: broadcast::Sender<CloudWsMessage>,
    max_cache_bytes: usize,
    sha256_cache: Arc<Mutex<HashMap<(path::PathBuf, u64), String>>>,
    tile_cache: Arc<Mutex<LruTileMap>>,
    preload_cache: Arc<Mutex<PreloadLruCache>>,
    catalog_cache: Arc<Mutex<Option<Bytes>>>,
}

impl AppState {
    pub fn new(registry: Registry, store_dir: impl Into<path::PathBuf>) -> Result<Self, db::Error> {
        Self::with_cache_budget(registry, store_dir, resolve_default_max_cache_bytes())
    }

    pub fn with_cache_budget(
        registry: Registry,
        store_dir: impl Into<path::PathBuf>,
        max_cache_bytes: usize,
    ) -> Result<Self, db::Error> {
        let store_dir = store_dir.into();
        fs::create_dir_all(store_dir.join("partitions"))?;
        fs::create_dir_all(store_dir.join("raw"))?;
        fs::create_dir_all(store_dir.join("superdbcs"))?;
        let _ = registry.sync_from_store_dir(&store_dir)?;
        let (ws_tx, _) = broadcast::channel(WS_BROADCAST_CAPACITY);
        let preload_bytes = max_cache_bytes / 4;
        let tile_bytes = max_cache_bytes.saturating_sub(preload_bytes);
        Ok(Self {
            registry,
            store_dir,
            web_dist_dir: resolve_default_web_dist_dir(),
            ws_tx,
            max_cache_bytes,
            sha256_cache: Arc::new(Mutex::new(HashMap::new())),
            tile_cache: Arc::new(Mutex::new(LruTileMap::new(tile_bytes))),
            preload_cache: Arc::new(Mutex::new(PreloadLruCache::new(preload_bytes))),
            catalog_cache: Arc::new(Mutex::new(None)),
        })
    }

    pub fn open(
        database_path: impl AsRef<path::Path>,
        store_dir: impl AsRef<path::Path>,
    ) -> Result<Self, db::Error> {
        Self::open_with_cache_budget(database_path, store_dir, resolve_default_max_cache_bytes())
    }

    pub fn open_with_cache_budget(
        database_path: impl AsRef<path::Path>,
        store_dir: impl AsRef<path::Path>,
        max_cache_bytes: usize,
    ) -> Result<Self, db::Error> {
        let registry = db::init_registry(database_path)?;
        Self::with_cache_budget(registry, store_dir.as_ref().to_path_buf(), max_cache_bytes)
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
            .and_then(|mut g| g.get(partition_id))
    }

    pub fn insert_cached_preload(&self, partition_id: String, payload: Bytes) {
        if let Ok(mut g) = self.preload_cache.lock() {
            g.insert(partition_id, payload);
        }
        self.enforce_memory_budget();
    }

    pub fn get_cached_tile(&self, key: &TileKey) -> Option<Arc<SignalSeries>> {
        self.tile_cache.lock().ok().and_then(|mut g| g.get(key))
    }

    pub fn insert_cached_tile(&self, key: TileKey, series: Arc<SignalSeries>) {
        if self.max_cache_bytes == 0 {
            return;
        }
        if let Ok(mut g) = self.tile_cache.lock() {
            g.insert(key, series);
        }
        self.enforce_memory_budget();
    }

    pub fn invalidate_partition_cache(&self, partition_id: &str) {
        if let Ok(mut g) = self.preload_cache.lock() {
            g.remove(partition_id);
        }
        self.invalidate_catalog_cache();
    }

    pub fn get_cached_catalog(&self) -> Option<Bytes> {
        self.catalog_cache.lock().ok().and_then(|g| g.clone())
    }

    pub fn insert_cached_catalog(&self, payload: Bytes) {
        if let Ok(mut g) = self.catalog_cache.lock() {
            *g = Some(payload);
        }
        trim_process_heap();
    }

    pub fn invalidate_catalog_cache(&self) {
        if let Ok(mut g) = self.catalog_cache.lock() {
            *g = None;
        }
    }

    pub fn enforce_memory_budget(&self) {
        trim_process_heap();
        if self.max_cache_bytes == 0 {
            return;
        }
        if let Some(rss) = Self::process_rss_bytes()
            && (rss as usize) > self.max_cache_bytes
        {
            let excess = (rss as usize).saturating_sub(self.max_cache_bytes);
            if let Ok(mut g) = self.tile_cache.lock() {
                let target = g
                    .current_bytes()
                    .saturating_sub(excess.max(16 * 1024 * 1024));
                g.evict_to_target_bytes(target);
            }
            trim_process_heap();
        }
    }

    pub fn cache_usage_bytes(&self) -> (usize, usize) {
        let tile_used = self
            .tile_cache
            .lock()
            .map(|g| g.current_bytes())
            .unwrap_or(0);
        let preload_used = self.preload_cache.lock().map(|g| g.used_bytes).unwrap_or(0);
        (tile_used.saturating_add(preload_used), self.max_cache_bytes)
    }

    pub fn process_rss_bytes() -> Option<u64> {
        let statm = fs::read_to_string("/proc/self/statm").ok()?;
        let resident_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        Some(resident_pages.saturating_mul(4096))
    }
}
