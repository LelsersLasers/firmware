use crate::{consts, db};
use bytes::Bytes;
use daqcore::{
    router::TileKey,
    storage::{
        CloudCacheStats, CloudLatencySample, CloudMemoryStats, CloudStatsResponse, CloudStoreStats,
        CloudWsMessage, EndpointLatencyStats,
    },
    time::Time,
};
use daqnative::storage::{Registry, compute_file_sha256_hex};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    env, fs, io, path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{Semaphore, broadcast};

const WS_BROADCAST_CAPACITY: usize = 64;
const DEFAULT_MAX_CONCURRENT_DECODES: usize = 2;
const OVERALL_LATENCY_WINDOW: usize = 1024;
const ENDPOINT_LATENCY_WINDOW: usize = 256;
const RECENT_LATENCY_SAMPLES_CAP: usize = 120;
const STORE_DISK_CACHE_TTL: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug)]
pub struct CacheHitMarker(pub bool);

#[derive(Clone, Debug)]
struct RollingLatencyBucket {
    request_count: u64,
    error_count: u64,
    sum_ms: f64,
    max_ms: f64,
    last_ms: f64,
    window_cap: usize,
    samples_ms: VecDeque<f64>,
}

impl RollingLatencyBucket {
    fn new(window_cap: usize) -> Self {
        Self {
            request_count: 0,
            error_count: 0,
            sum_ms: 0.0,
            max_ms: 0.0,
            last_ms: 0.0,
            window_cap: window_cap.max(1),
            samples_ms: VecDeque::with_capacity(window_cap.max(1)),
        }
    }

    fn record(&mut self, elapsed_ms: f64, is_error: bool) {
        let clean_ms = if elapsed_ms.is_finite() && elapsed_ms >= 0.0 {
            elapsed_ms
        } else {
            0.0
        };
        self.request_count = self.request_count.saturating_add(1);
        if is_error {
            self.error_count = self.error_count.saturating_add(1);
        }
        self.sum_ms += clean_ms;
        self.max_ms = self.max_ms.max(clean_ms);
        self.last_ms = clean_ms;
        if self.samples_ms.len() >= self.window_cap {
            self.samples_ms.pop_front();
        }
        self.samples_ms.push_back(clean_ms);
    }

    fn to_stats(&self, endpoint: &str) -> EndpointLatencyStats {
        if self.samples_ms.is_empty() {
            return EndpointLatencyStats {
                endpoint: endpoint.to_owned(),
                request_count: self.request_count,
                error_count: self.error_count,
                ..Default::default()
            };
        }
        let mut sorted: Vec<f64> = self.samples_ms.iter().copied().collect();
        sorted.sort_by(f64::total_cmp);
        let avg_ms = if self.request_count > 0 {
            self.sum_ms / (self.request_count as f64)
        } else {
            0.0
        };
        EndpointLatencyStats {
            endpoint: endpoint.to_owned(),
            request_count: self.request_count,
            error_count: self.error_count,
            avg_ms,
            p50_ms: percentile_sorted(&sorted, 0.50),
            p90_ms: percentile_sorted(&sorted, 0.90),
            p99_ms: percentile_sorted(&sorted, 0.99),
            max_ms: self.max_ms,
            last_ms: self.last_ms,
        }
    }
}

pub fn percentile_sorted(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let pos = q.clamp(0.0, 1.0) * ((sorted.len() - 1) as f64);
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    let frac = pos - (lo as f64);
    sorted[lo] * (1.0 - frac) + sorted[hi] * frac
}

struct LatencyTracker {
    overall: RollingLatencyBucket,
    endpoints: BTreeMap<String, RollingLatencyBucket>,
    recent_samples: VecDeque<CloudLatencySample>,
}

impl Default for LatencyTracker {
    fn default() -> Self {
        Self {
            overall: RollingLatencyBucket::new(OVERALL_LATENCY_WINDOW),
            endpoints: BTreeMap::new(),
            recent_samples: VecDeque::with_capacity(RECENT_LATENCY_SAMPLES_CAP),
        }
    }
}

struct CloudMetrics {
    started_at: Instant,
    active_ws_clients: AtomicUsize,
    in_flight_http_requests: AtomicUsize,
    tile_hits: AtomicU64,
    tile_misses: AtomicU64,
    preload_hits: AtomicU64,
    preload_misses: AtomicU64,
    catalog_hits: AtomicU64,
    catalog_misses: AtomicU64,
    sha256_hits: AtomicU64,
    sha256_misses: AtomicU64,
    latency: Mutex<LatencyTracker>,
    cached_store_stats: Mutex<Option<(Instant, CloudStoreStats)>>,
}

impl Default for CloudMetrics {
    fn default() -> Self {
        Self {
            started_at: Instant::now(),
            active_ws_clients: AtomicUsize::new(0),
            in_flight_http_requests: AtomicUsize::new(0),
            tile_hits: AtomicU64::new(0),
            tile_misses: AtomicU64::new(0),
            preload_hits: AtomicU64::new(0),
            preload_misses: AtomicU64::new(0),
            catalog_hits: AtomicU64::new(0),
            catalog_misses: AtomicU64::new(0),
            sha256_hits: AtomicU64::new(0),
            sha256_misses: AtomicU64::new(0),
            latency: Mutex::new(LatencyTracker::default()),
            cached_store_stats: Mutex::new(None),
        }
    }
}

pub struct InFlightCounterGuard<'a> {
    counter: &'a AtomicUsize,
}

impl<'a> InFlightCounterGuard<'a> {
    fn new(counter: &'a AtomicUsize) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Self { counter }
    }
}

impl Drop for InFlightCounterGuard<'_> {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

struct EncodedTileLruCache {
    max_bytes: usize,
    used_bytes: usize,
    order: VecDeque<TileKey>,
    entries: HashMap<TileKey, Bytes>,
}

impl EncodedTileLruCache {
    fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            used_bytes: 0,
            order: VecDeque::new(),
            entries: HashMap::new(),
        }
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    fn current_bytes(&self) -> usize {
        self.used_bytes
    }

    fn get(&mut self, key: &TileKey) -> Option<Bytes> {
        let bytes = self.entries.get(key)?.clone();
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            self.order.remove(pos);
        }
        self.order.push_back(key.clone());
        Some(bytes)
    }

    fn insert(&mut self, key: TileKey, payload: Bytes) {
        let len = payload.len();
        if self.max_bytes == 0 || len > self.max_bytes {
            return;
        }
        self.remove_key(&key);
        while self.used_bytes.saturating_add(len) > self.max_bytes {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(removed) = self.entries.remove(&oldest) {
                self.used_bytes = self.used_bytes.saturating_sub(removed.len());
            }
        }
        self.used_bytes = self.used_bytes.saturating_add(len);
        self.order.push_back(key.clone());
        self.entries.insert(key, payload);
    }

    fn remove_key(&mut self, key: &TileKey) {
        if let Some(removed) = self.entries.remove(key) {
            self.used_bytes = self.used_bytes.saturating_sub(removed.len());
        }
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            self.order.remove(pos);
        }
    }

    fn remove_partition(&mut self, partition_id: &str) {
        let keys_to_remove: Vec<TileKey> = self
            .entries
            .keys()
            .filter(|k| k.partition_id == partition_id)
            .cloned()
            .collect();
        for key in keys_to_remove {
            self.remove_key(&key);
        }
    }

    fn evict_to_target_bytes(&mut self, target_bytes: usize) {
        while self.used_bytes > target_bytes {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(removed) = self.entries.remove(&oldest) {
                self.used_bytes = self.used_bytes.saturating_sub(removed.len());
            }
        }
    }
}

struct PreloadLruCache {
    max_bytes: usize,
    used_bytes: usize,
    entries: HashMap<String, Bytes>,
}

impl PreloadLruCache {
    fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            used_bytes: 0,
            entries: HashMap::new(),
        }
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    fn get(&mut self, partition_id: &str) -> Option<Bytes> {
        self.entries.get(partition_id).cloned()
    }

    /// Scan-resistant admission: web clients always scan all partitions sequentially on boot.
    /// A standard FIFO/LRU cache smaller than the total catalog preload footprint would suffer
    /// 0% hit rate due to cyclic eviction. By pinning admitted entries up to `max_bytes`
    /// (and only replacing on explicit partition invalidation or same-key update), repeated
    /// sequential catalog scans deterministically hit the cached working set.
    fn insert(&mut self, partition_id: String, payload: Bytes) {
        let len = payload.len();
        if self.max_bytes == 0 || len > self.max_bytes {
            return;
        }
        self.remove(&partition_id);
        if self.used_bytes.saturating_add(len) > self.max_bytes {
            return;
        }
        self.used_bytes = self.used_bytes.saturating_add(len);
        self.entries.insert(partition_id, payload);
    }

    fn remove(&mut self, partition_id: &str) {
        if let Some(removed) = self.entries.remove(partition_id) {
            self.used_bytes = self.used_bytes.saturating_sub(removed.len());
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

fn resolve_default_max_concurrent_decodes() -> usize {
    env::var("DAQCLOUD_MAX_CONCURRENT_DECODES")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&n| n >= 1)
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get().clamp(2, 4))
                .unwrap_or(DEFAULT_MAX_CONCURRENT_DECODES)
        })
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

type Sha256CacheKey = (path::PathBuf, u64, u64);

#[derive(Clone)]
pub struct AppState {
    pub registry: Registry,
    pub store_dir: path::PathBuf,
    pub web_dist_dir: path::PathBuf,
    pub ws_tx: broadcast::Sender<CloudWsMessage>,
    max_cache_bytes: usize,
    sha256_cache: Arc<Mutex<HashMap<Sha256CacheKey, String>>>,
    tile_cache: Arc<Mutex<EncodedTileLruCache>>,
    preload_cache: Arc<Mutex<PreloadLruCache>>,
    catalog_cache: Arc<Mutex<Option<Bytes>>>,
    decode_semaphore: Arc<Semaphore>,
    metrics: Arc<CloudMetrics>,
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
        let preload_bytes = max_cache_bytes / 10;
        let tile_bytes = max_cache_bytes.saturating_sub(preload_bytes);
        let max_decodes = resolve_default_max_concurrent_decodes();
        Ok(Self {
            registry,
            store_dir,
            web_dist_dir: resolve_default_web_dist_dir(),
            ws_tx,
            max_cache_bytes,
            sha256_cache: Arc::new(Mutex::new(HashMap::new())),
            tile_cache: Arc::new(Mutex::new(EncodedTileLruCache::new(tile_bytes))),
            preload_cache: Arc::new(Mutex::new(PreloadLruCache::new(preload_bytes))),
            catalog_cache: Arc::new(Mutex::new(None)),
            decode_semaphore: Arc::new(Semaphore::new(max_decodes)),
            metrics: Arc::new(CloudMetrics::default()),
        })
    }

    pub fn decode_semaphore(&self) -> &Arc<Semaphore> {
        &self.decode_semaphore
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

    pub fn track_http_in_flight(&self) -> InFlightCounterGuard<'_> {
        InFlightCounterGuard::new(&self.metrics.in_flight_http_requests)
    }

    pub fn track_ws_client(&self) -> InFlightCounterGuard<'_> {
        InFlightCounterGuard::new(&self.metrics.active_ws_clients)
    }

    pub fn record_http_request(
        &self,
        endpoint_category: &str,
        status: u16,
        elapsed_ms: f64,
        cache_hit: bool,
        include_in_overall: bool,
    ) {
        let is_error = status >= 400;
        let now_ms = Time::now().unix_millis();
        if let Ok(mut guard) = self.metrics.latency.lock() {
            if include_in_overall {
                guard.overall.record(elapsed_ms, is_error);
                if guard.recent_samples.len() >= RECENT_LATENCY_SAMPLES_CAP {
                    guard.recent_samples.pop_front();
                }
                guard.recent_samples.push_back(CloudLatencySample {
                    utc_ms: now_ms,
                    elapsed_ms,
                    endpoint: endpoint_category.to_owned(),
                    status,
                    cache_hit,
                });
            }
            guard
                .endpoints
                .entry(endpoint_category.to_owned())
                .or_insert_with(|| RollingLatencyBucket::new(ENDPOINT_LATENCY_WINDOW))
                .record(elapsed_ms, is_error);
        }
    }

    pub fn invalidate_store_disk_cache(&self) {
        if let Ok(mut guard) = self.metrics.cached_store_stats.lock() {
            *guard = None;
        }
    }

    pub fn cached_file_sha256(&self, path: &path::Path) -> io::Result<(u64, String)> {
        let meta = fs::metadata(path)?;
        let size = meta.len();
        let mtime_secs = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        let key = (path.to_path_buf(), size, mtime_secs);
        if let Ok(guard) = self.sha256_cache.lock()
            && let Some(cached) = guard.get(&key)
        {
            self.metrics.sha256_hits.fetch_add(1, Ordering::Relaxed);
            return Ok((size, cached.clone()));
        }
        self.metrics.sha256_misses.fetch_add(1, Ordering::Relaxed);
        let (computed_size, digest) = compute_file_sha256_hex(path)?;
        if let Ok(mut guard) = self.sha256_cache.lock() {
            if guard.len() >= 4096 {
                guard.clear();
            }
            guard.insert(
                (path.to_path_buf(), computed_size, mtime_secs),
                digest.clone(),
            );
        }
        Ok((computed_size, digest))
    }

    pub fn insert_cached_sha256(&self, path: path::PathBuf, size: u64, sha256_hex: String) {
        let mtime_secs = fs::metadata(&path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        if let Ok(mut guard) = self.sha256_cache.lock() {
            if guard.len() >= 4096 {
                guard.clear();
            }
            guard.retain(|(p, _, _), _| p != &path);
            guard.insert((path, size, mtime_secs), sha256_hex);
        }
        self.invalidate_store_disk_cache();
    }

    pub fn get_cached_preload(&self, partition_id: &str) -> Option<Bytes> {
        let res = self
            .preload_cache
            .lock()
            .ok()
            .and_then(|mut g| g.get(partition_id));
        if res.is_some() {
            self.metrics.preload_hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.metrics.preload_misses.fetch_add(1, Ordering::Relaxed);
        }
        res
    }

    pub fn insert_cached_preload(&self, partition_id: String, payload: Bytes) {
        if let Ok(mut g) = self.preload_cache.lock() {
            g.insert(partition_id, payload);
        }
        self.enforce_memory_budget();
    }

    pub fn get_cached_tile(&self, key: &TileKey) -> Option<Bytes> {
        let res = self.tile_cache.lock().ok().and_then(|mut g| g.get(key));
        if res.is_some() {
            self.metrics.tile_hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.metrics.tile_misses.fetch_add(1, Ordering::Relaxed);
        }
        res
    }

    pub fn insert_cached_tile(&self, key: TileKey, payload: Bytes) {
        if self.max_cache_bytes == 0 {
            return;
        }
        if let Ok(mut g) = self.tile_cache.lock() {
            g.insert(key, payload);
        }
        self.enforce_memory_budget();
    }

    pub fn invalidate_partition_cache(&self, partition_id: &str) {
        if let Ok(mut g) = self.preload_cache.lock() {
            g.remove(partition_id);
        }
        if let Ok(mut g) = self.tile_cache.lock() {
            g.remove_partition(partition_id);
        }
        self.invalidate_catalog_cache();
        self.invalidate_store_disk_cache();
    }

    pub fn get_cached_catalog(&self) -> Option<Bytes> {
        let res = self.catalog_cache.lock().ok().and_then(|g| g.clone());
        if res.is_some() {
            self.metrics.catalog_hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.metrics.catalog_misses.fetch_add(1, Ordering::Relaxed);
        }
        res
    }

    pub fn insert_cached_catalog(&self, payload: Bytes) {
        if let Ok(mut g) = self.catalog_cache.lock() {
            *g = Some(payload);
        }
    }

    pub fn invalidate_catalog_cache(&self) {
        if let Ok(mut g) = self.catalog_cache.lock() {
            *g = None;
        }
        self.invalidate_store_disk_cache();
    }

    pub fn enforce_memory_budget(&self) {
        if self.max_cache_bytes == 0 {
            return;
        }
        let (used_bytes, max_bytes) = self.cache_usage_bytes();
        if used_bytes > max_bytes {
            let excess = used_bytes.saturating_sub(max_bytes);
            if let Ok(mut g) = self.tile_cache.lock() {
                let target = g.current_bytes().saturating_sub(excess);
                g.evict_to_target_bytes(target);
            }
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

    pub fn process_virtual_bytes() -> Option<u64> {
        let statm = fs::read_to_string("/proc/self/statm").ok()?;
        let virtual_pages: u64 = statm.split_whitespace().next()?.parse().ok()?;
        Some(virtual_pages.saturating_mul(4096))
    }

    pub fn collect_memory_stats() -> CloudMemoryStats {
        let mut stats = fs::read_to_string("/proc/meminfo")
            .ok()
            .map(|content| parse_proc_meminfo(&content))
            .unwrap_or_default();
        stats.process_rss_bytes = Self::process_rss_bytes();
        stats.process_virtual_bytes = Self::process_virtual_bytes();
        stats
    }

    pub fn collect_cache_stats(&self) -> CloudCacheStats {
        let (tile_used, tile_max, tile_entries) = self
            .tile_cache
            .lock()
            .map(|g| (g.current_bytes() as u64, g.max_bytes() as u64, g.len()))
            .unwrap_or((0, 0, 0));
        let (preload_used, preload_max, preload_entries) = self
            .preload_cache
            .lock()
            .map(|g| (g.used_bytes as u64, g.max_bytes() as u64, g.len()))
            .unwrap_or((0, 0, 0));
        let catalog_used = self
            .catalog_cache
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|b| b.len() as u64))
            .unwrap_or(0);
        let sha256_entries = self.sha256_cache.lock().map(|g| g.len()).unwrap_or(0);

        CloudCacheStats {
            total_used_bytes: tile_used.saturating_add(preload_used),
            total_max_bytes: self.max_cache_bytes as u64,
            tile_used_bytes: tile_used,
            tile_max_bytes: tile_max,
            tile_entries,
            tile_hits: self.metrics.tile_hits.load(Ordering::Relaxed),
            tile_misses: self.metrics.tile_misses.load(Ordering::Relaxed),
            preload_used_bytes: preload_used,
            preload_max_bytes: preload_max,
            preload_entries,
            preload_hits: self.metrics.preload_hits.load(Ordering::Relaxed),
            preload_misses: self.metrics.preload_misses.load(Ordering::Relaxed),
            catalog_used_bytes: catalog_used,
            catalog_hits: self.metrics.catalog_hits.load(Ordering::Relaxed),
            catalog_misses: self.metrics.catalog_misses.load(Ordering::Relaxed),
            sha256_entries,
            sha256_hits: self.metrics.sha256_hits.load(Ordering::Relaxed),
            sha256_misses: self.metrics.sha256_misses.load(Ordering::Relaxed),
        }
    }

    pub fn store_disk_bytes(&self) -> u64 {
        self.collect_store_stats().store_disk_bytes
    }

    pub fn collect_store_stats(&self) -> CloudStoreStats {
        if let Ok(guard) = self.metrics.cached_store_stats.lock()
            && let Some((ts, ref stats)) = *guard
            && ts.elapsed() < STORE_DISK_CACHE_TTL
        {
            return stats.clone();
        }
        let track_days = self
            .registry
            .list_track_days()
            .map(|v| v.len())
            .unwrap_or(0);
        let partitions = self
            .registry
            .list_all_partitions()
            .map(|v| v.len())
            .unwrap_or(0);
        let annotations = self
            .registry
            .list_annotations_updated_since(0)
            .map(|v| v.into_iter().filter(|a| !a.deleted).count())
            .unwrap_or(0);
        let derived_signals = self
            .registry
            .list_derived_signals(false)
            .map(|v| v.len())
            .unwrap_or(0);
        let stats = CloudStoreStats {
            track_days,
            partitions,
            annotations,
            derived_signals,
            store_disk_bytes: compute_dir_size_bytes(&self.store_dir),
        };
        if let Ok(mut guard) = self.metrics.cached_store_stats.lock() {
            *guard = Some((Instant::now(), stats.clone()));
        }
        stats
    }

    pub fn collect_stats_response(&self) -> CloudStatsResponse {
        let (overall_latency, endpoints, recent_latency_samples) = if let Ok(guard) =
            self.metrics.latency.lock()
        {
            let overall = guard.overall.to_stats("All Data Requests");
            let mut ep_list: Vec<EndpointLatencyStats> =
                guard.endpoints.iter().map(|(k, b)| b.to_stats(k)).collect();
            ep_list.sort_by(|a, b| {
                b.request_count
                    .cmp(&a.request_count)
                    .then_with(|| a.endpoint.cmp(&b.endpoint))
            });
            let recent: Vec<CloudLatencySample> = guard.recent_samples.iter().cloned().collect();
            (overall, ep_list, recent)
        } else {
            (EndpointLatencyStats::default(), Vec::new(), Vec::new())
        };

        CloudStatsResponse {
            collected_at_utc_ms: Time::now().unix_millis(),
            uptime_secs: self.metrics.started_at.elapsed().as_secs_f64(),
            active_ws_clients: self.metrics.active_ws_clients.load(Ordering::Relaxed),
            in_flight_http_requests: self.metrics.in_flight_http_requests.load(Ordering::Relaxed),
            overall_latency,
            endpoints,
            recent_latency_samples,
            cache: self.collect_cache_stats(),
            memory: Self::collect_memory_stats(),
            store: self.collect_store_stats(),
        }
    }
}

pub fn parse_proc_meminfo(content: &str) -> CloudMemoryStats {
    let mut mem_total_kb: Option<u64> = None;
    let mut mem_free_kb: Option<u64> = None;
    let mut mem_available_kb: Option<u64> = None;
    let mut buffers_kb: u64 = 0;
    let mut cached_kb: u64 = 0;
    let mut has_buffers_or_cached = false;
    let mut swap_total_kb: Option<u64> = None;
    let mut swap_free_kb: Option<u64> = None;

    for line in content.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let Some(val_str) = rest.split_whitespace().next() else {
            continue;
        };
        let Ok(val_kb) = val_str.parse::<u64>() else {
            continue;
        };
        match key.trim() {
            "MemTotal" => mem_total_kb = Some(val_kb),
            "MemFree" => mem_free_kb = Some(val_kb),
            "MemAvailable" => mem_available_kb = Some(val_kb),
            "Buffers" => {
                buffers_kb = buffers_kb.saturating_add(val_kb);
                has_buffers_or_cached = true;
            }
            "Cached" | "SReclaimable" => {
                cached_kb = cached_kb.saturating_add(val_kb);
                has_buffers_or_cached = true;
            }
            "SwapTotal" => swap_total_kb = Some(val_kb),
            "SwapFree" => swap_free_kb = Some(val_kb),
            _ => {}
        }
    }

    CloudMemoryStats {
        process_rss_bytes: None,
        process_virtual_bytes: None,
        vps_total_bytes: mem_total_kb.map(|kb| kb.saturating_mul(1024)),
        vps_free_bytes: mem_free_kb.map(|kb| kb.saturating_mul(1024)),
        vps_available_bytes: mem_available_kb
            .or(mem_free_kb)
            .map(|kb| kb.saturating_mul(1024)),
        vps_buffers_cached_bytes: has_buffers_or_cached
            .then(|| buffers_kb.saturating_add(cached_kb).saturating_mul(1024)),
        vps_swap_total_bytes: swap_total_kb.map(|kb| kb.saturating_mul(1024)),
        vps_swap_free_bytes: swap_free_kb.map(|kb| kb.saturating_mul(1024)),
    }
}

fn compute_dir_size_bytes(dir: &path::Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(cur) = stack.pop() {
        let Ok(entries) = fs::read_dir(&cur) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_proc_meminfo_extracts_vps_ram_and_swap() {
        let sample = "\
MemTotal:        4029440 kB
MemFree:          512000 kB
MemAvailable:    2048000 kB
Buffers:          128000 kB
Cached:          1024000 kB
SwapTotal:       1048576 kB
SwapFree:         524288 kB
";
        let stats = parse_proc_meminfo(sample);
        assert_eq!(stats.vps_total_bytes, Some(4_029_440 * 1024));
        assert_eq!(stats.vps_free_bytes, Some(512_000 * 1024));
        assert_eq!(stats.vps_available_bytes, Some(2_048_000 * 1024));
        assert_eq!(
            stats.vps_buffers_cached_bytes,
            Some((128_000 + 1_024_000) * 1024)
        );
        assert_eq!(stats.vps_swap_total_bytes, Some(1_048_576 * 1024));
        assert_eq!(stats.vps_swap_free_bytes, Some(524_288 * 1024));
    }

    #[test]
    fn test_percentile_sorted_interpolates_accurately() {
        let vals = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];
        assert!((percentile_sorted(&vals, 0.50) - 5.5).abs() < 1e-9);
        assert!((percentile_sorted(&vals, 0.90) - 9.1).abs() < 1e-9);
        assert!((percentile_sorted(&vals, 0.0) - 1.0).abs() < 1e-9);
        assert!((percentile_sorted(&vals, 1.0) - 10.0).abs() < 1e-9);
    }

    #[test]
    fn test_preload_cache_scan_resistant_admission() {
        let mut cache = PreloadLruCache::new(10);
        cache.insert("p1".to_owned(), Bytes::from_static(b"12345"));
        cache.insert("p2".to_owned(), Bytes::from_static(b"67890"));
        // Cache is now full (10/10 bytes). A sequential scan over p3..p5 must not evict p1/p2.
        cache.insert("p3".to_owned(), Bytes::from_static(b"abcde"));
        assert!(cache.get("p1").is_some());
        assert!(cache.get("p2").is_some());
        assert!(cache.get("p3").is_none());
        // Explicit invalidation frees space for new admissions.
        cache.remove("p1");
        cache.insert("p3".to_owned(), Bytes::from_static(b"abcde"));
        assert!(cache.get("p3").is_some());
    }
}
