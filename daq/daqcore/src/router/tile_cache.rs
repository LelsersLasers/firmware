use crate::{
    router::{
        types::{
            BUCKET_1HZ_MS, BUCKET_10HZ_MS, Resolution, SignalSeries, StateInterval,
        },
        wire::PartitionPreloadPayload,
    },
    storage::{self, RegisteredPartition, SignalId},
};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt, io, path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time,
};

/// Default maximum memory budget for `Overview10Hz` and `FullRes` tiles in [`TileCache`] (512 MiB).
pub const DEFAULT_TILE_CACHE_BYTES: usize = 512 * 1024 * 1024;

pub type RepaintCallback = Arc<dyn Fn() + Send + Sync>;

#[derive(Debug)]
pub enum RouterError {
    Io(io::Error),
    Format(String),
    Registry(String),
    InvalidColumn(&'static str),
}

impl fmt::Display for RouterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Format(msg) => write!(f, "Data format error: {msg}"),
            Self::Registry(msg) => write!(f, "Registry error: {msg}"),
            Self::InvalidColumn(msg) => write!(f, "Invalid column schema: {msg}"),
        }
    }
}

impl std::error::Error for RouterError {}

impl From<io::Error> for RouterError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TileKey {
    pub partition_id: String,
    pub signal_id: SignalId,
    pub resolution: Resolution,
}

#[derive(Debug, Clone)]
pub enum TileLookup {
    /// The requested resolution was resident in RAM and is returned directly.
    Ready(Arc<SignalSeries>),
    /// The requested resolution (`Overview10Hz` or `FullRes`) was not yet in RAM;
    /// a background read has been queued and a lower-resolution resident series is returned
    /// immediately so the UI thread never blocks.
    Fallback {
        series: Arc<SignalSeries>,
        actual_resolution: Resolution,
    },
}

impl TileLookup {
    pub fn series(&self) -> &Arc<SignalSeries> {
        match self {
            Self::Ready(series) | Self::Fallback { series, .. } => series,
        }
    }

    pub fn into_series(self) -> Arc<SignalSeries> {
        match self {
            Self::Ready(series) | Self::Fallback { series, .. } => series,
        }
    }

    pub fn is_fallback(&self) -> bool {
        matches!(self, Self::Fallback { .. })
    }
}

pub struct TileLoadResult {
    pub key: TileKey,
    pub result: Result<Arc<SignalSeries>, RouterError>,
}

/// Platform-specific tile loader plugged into [`TileCache`].
///
/// - On Desktop (`daqnative`), `NativeTileLoader` reads `.parquet` files on a background OS thread
///   (and fetches remote tiles from `daqcloud` when a partition is not yet localized on disk).
/// - On Web (`daqweb`), `WebTileLoader` fetches compact `DAQT\x01` binary tiles over HTTP.
pub trait TileLoader: Send + Sync {
    fn set_repaint_callback(&self, _callback: Option<RepaintCallback>) {}

    fn register_partition_dir(&self, _partition_id: &str, _partition_dir: &path::Path) {}

    fn clear(&self) {}

    fn can_load_async(&self, key: &TileKey) -> bool;

    fn request_load(&self, key: TileKey, res_tx: mpsc::Sender<TileLoadResult>);

    fn load_blocking(&self, key: &TileKey) -> Result<Option<Arc<SignalSeries>>, RouterError>;

    fn preload_partition_dir(
        &self,
        _partition_id: &str,
        _partition_dir: &path::Path,
        _known_signals: Option<&[storage::SignalManifestEntry]>,
    ) -> Result<Option<PartitionPreloadPayload>, RouterError> {
        Ok(None)
    }
}

struct LruEntry {
    generation: u64,
    bytes: usize,
    series: Arc<SignalSeries>,
}

/// Byte-bounded LRU cache for `(partition_id, signal_id, Resolution)` column tiles.
pub struct LruTileMap {
    entries: HashMap<TileKey, LruEntry>,
    order: BTreeMap<u64, TileKey>,
    next_generation: u64,
    current_bytes: usize,
    max_bytes: usize,
}

impl LruTileMap {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: BTreeMap::new(),
            next_generation: 0,
            current_bytes: 0,
            max_bytes: max_bytes.max(1),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn current_bytes(&self) -> usize {
        self.current_bytes
    }

    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.current_bytes = 0;
    }

    pub fn get(&mut self, key: &TileKey) -> Option<Arc<SignalSeries>> {
        let entry = self.entries.get_mut(key)?;
        self.order.remove(&entry.generation);
        self.next_generation = self.next_generation.wrapping_add(1);
        entry.generation = self.next_generation;
        self.order.insert(entry.generation, key.clone());
        Some(Arc::clone(&entry.series))
    }

    pub fn peek(&self, key: &TileKey) -> Option<&Arc<SignalSeries>> {
        self.entries.get(key).map(|e| &e.series)
    }

    pub fn insert(&mut self, key: TileKey, series: Arc<SignalSeries>) {
        let bytes = series.estimated_bytes();
        if let Some(prev) = self.entries.remove(&key) {
            self.order.remove(&prev.generation);
            self.current_bytes = self.current_bytes.saturating_sub(prev.bytes);
        }

        self.next_generation = self.next_generation.wrapping_add(1);
        let generation = self.next_generation;
        self.current_bytes = self.current_bytes.saturating_add(bytes);
        self.order.insert(generation, key.clone());
        self.entries.insert(
            key,
            LruEntry {
                generation,
                bytes,
                series,
            },
        );

        while self.current_bytes > self.max_bytes && self.entries.len() > 1 {
            let Some((_, oldest_key)) = self.order.pop_first() else {
                break;
            };
            if let Some(evicted) = self.entries.remove(&oldest_key) {
                self.current_bytes = self.current_bytes.saturating_sub(evicted.bytes);
            }
        }
    }
}

struct TileCacheDynamic {
    lru: LruTileMap,
    in_flight: HashSet<TileKey>,
    res_rx: mpsc::Receiver<TileLoadResult>,
}

/// Multi-resolution RAM tile cache for canonical telemetry partitions.
///
/// - `1 Hz` overviews and `states` are preloaded into resident memory for active partitions.
/// - `10 Hz` overviews and `FullRes` columns are loaded on demand via [`TileLoader`] and cached
///   in a byte-bounded LRU map (`DEFAULT_TILE_CACHE_BYTES`).
pub struct TileCache {
    resident_1hz: HashMap<String, HashMap<SignalId, Arc<SignalSeries>>>,
    resident_activity_1hz: HashMap<String, Arc<Vec<(i64, u32)>>>,
    resident_states: HashMap<String, HashMap<SignalId, Arc<Vec<StateInterval>>>>,
    partition_dirs: HashMap<String, path::PathBuf>,
    partition_signals: HashMap<String, HashSet<SignalId>>,
    dynamic: Mutex<TileCacheDynamic>,
    revision: AtomicU64,
    res_tx: mpsc::Sender<TileLoadResult>,
    loader: Option<Arc<dyn TileLoader>>,
    repaint_cb: Arc<Mutex<Option<RepaintCallback>>>,
}

impl Default for TileCache {
    fn default() -> Self {
        Self::new(DEFAULT_TILE_CACHE_BYTES)
    }
}

impl TileCache {
    pub fn new(max_lru_bytes: usize) -> Self {
        Self::with_loader(max_lru_bytes, None)
    }

    pub fn with_loader(max_lru_bytes: usize, loader: Option<Arc<dyn TileLoader>>) -> Self {
        let (res_tx, res_rx) = mpsc::channel::<TileLoadResult>();
        Self {
            resident_1hz: HashMap::new(),
            resident_activity_1hz: HashMap::new(),
            resident_states: HashMap::new(),
            partition_dirs: HashMap::new(),
            partition_signals: HashMap::new(),
            dynamic: Mutex::new(TileCacheDynamic {
                lru: LruTileMap::new(max_lru_bytes),
                in_flight: HashSet::new(),
                res_rx,
            }),
            revision: AtomicU64::new(1),
            res_tx,
            loader,
            repaint_cb: Arc::new(Mutex::new(None)),
        }
    }

    pub fn set_loader(&mut self, loader: Option<Arc<dyn TileLoader>>) {
        if let Some(l) = &loader {
            let cb = self.repaint_cb.lock().ok().and_then(|g| g.clone());
            l.set_repaint_callback(cb);
            for (part_id, dir) in &self.partition_dirs {
                l.register_partition_dir(part_id, dir);
            }
        }
        self.loader = loader;
    }

    pub fn loader(&self) -> Option<&Arc<dyn TileLoader>> {
        self.loader.as_ref()
    }

    pub fn result_sender(&self) -> mpsc::Sender<TileLoadResult> {
        self.res_tx.clone()
    }

    /// Monotonically increasing revision counter bumped whenever tiles finish loading,
    /// partitions are preloaded, or the cache is cleared.
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Relaxed)
    }

    /// Configure a callback (e.g. `move || ctx.request_repaint()`) invoked whenever the
    /// background loader finishes loading an on-demand `Overview10Hz` or `FullRes` tile.
    pub fn set_repaint_callback(&self, callback: Option<RepaintCallback>) {
        if let Ok(mut guard) = self.repaint_cb.lock() {
            *guard = callback.clone();
        }
        if let Some(loader) = &self.loader {
            loader.set_repaint_callback(callback);
        }
    }

    pub fn lru_len(&self) -> usize {
        self.dynamic.lock().map(|d| d.lru.len()).unwrap_or(0)
    }

    pub fn lru_bytes(&self) -> usize {
        self.dynamic
            .lock()
            .map(|d| d.lru.current_bytes())
            .unwrap_or(0)
    }

    pub fn max_lru_bytes(&self) -> usize {
        self.dynamic.lock().map(|d| d.lru.max_bytes()).unwrap_or(0)
    }

    pub fn in_flight_count(&self) -> usize {
        self.dynamic.lock().map(|d| d.in_flight.len()).unwrap_or(0)
    }

    pub fn is_partition_preloaded(&self, partition_id: &str) -> bool {
        self.resident_1hz.contains_key(partition_id)
    }

    pub fn clear(&mut self) {
        self.poll_completed();
        self.resident_1hz.clear();
        self.resident_activity_1hz.clear();
        self.resident_states.clear();
        self.partition_dirs.clear();
        self.partition_signals.clear();
        if let Some(loader) = &self.loader {
            loader.clear();
        }
        if let Ok(mut dyn_guard) = self.dynamic.lock() {
            dyn_guard.lru.clear();
            dyn_guard.in_flight.clear();
        }
        self.revision.fetch_add(1, Ordering::Relaxed);
    }

    /// Return the preloaded 1Hz sample-count activity buckets `(bucket_utc_ms, count)` for a partition.
    pub fn partition_activity_1hz(&self, partition_id: &str) -> Option<&[(i64, u32)]> {
        self.resident_activity_1hz
            .get(partition_id)
            .map(|v| v.as_slice())
    }

    /// Directly insert a decoded [`PartitionPreloadPayload`] into resident RAM.
    pub fn insert_preload_payload(&mut self, payload: PartitionPreloadPayload) {
        let mut signal_set: HashSet<SignalId> = payload.overview_1hz.keys().cloned().collect();
        signal_set.extend(payload.states.keys().cloned());
        for entry in &payload.signals {
            signal_set.insert(entry.signal_id.clone());
        }
        self.partition_signals
            .insert(payload.partition_id.clone(), signal_set);
        self.resident_1hz
            .insert(payload.partition_id.clone(), payload.overview_1hz);
        self.resident_activity_1hz.insert(
            payload.partition_id.clone(),
            Arc::new(payload.activity_1hz),
        );
        self.resident_states
            .insert(payload.partition_id, payload.states);
        self.revision.fetch_add(1, Ordering::Relaxed);
    }

    /// Export the resident 1Hz overview, activity, and states for `partition_id` into a [`PartitionPreloadPayload`].
    pub fn export_preload_payload(
        &self,
        partition_id: &str,
        signals: Vec<storage::SignalManifestEntry>,
    ) -> Option<PartitionPreloadPayload> {
        let overview_1hz = self.resident_1hz.get(partition_id)?.clone();
        let activity_1hz = self
            .resident_activity_1hz
            .get(partition_id)
            .map(|v| v.as_ref().clone())
            .unwrap_or_default();
        let states = self
            .resident_states
            .get(partition_id)
            .cloned()
            .unwrap_or_default();
        Some(PartitionPreloadPayload {
            partition_id: partition_id.to_owned(),
            overview_1hz,
            activity_1hz,
            states,
            signals,
        })
    }

    /// Preload `overview_1hz.parquet` and `states.parquet` for a single partition into resident RAM
    /// via the configured [`TileLoader`].
    pub fn preload_partition(
        &mut self,
        partition_id: &str,
        partition_dir: impl AsRef<path::Path>,
        known_signals: Option<&[storage::SignalManifestEntry]>,
    ) -> Result<(), RouterError> {
        let partition_dir = partition_dir.as_ref().to_path_buf();
        if let Some(loader) = &self.loader {
            loader.register_partition_dir(partition_id, &partition_dir);
        }
        if self.resident_1hz.contains_key(partition_id) {
            self.partition_dirs
                .insert(partition_id.to_owned(), partition_dir);
            return Ok(());
        }

        let Some(loader) = self.loader.clone() else {
            return Err(RouterError::Format(
                "No TileLoader configured on TileCache for filesystem preload".to_owned(),
            ));
        };
        let Some(payload) =
            loader.preload_partition_dir(partition_id, &partition_dir, known_signals)?
        else {
            return Ok(());
        };
        self.partition_dirs
            .insert(partition_id.to_owned(), partition_dir);
        self.insert_preload_payload(payload);
        Ok(())
    }

    /// Preload all partitions in `partitions` that have `overview_1hz.parquet` present in
    /// `<store_dir>/<relative_path>`.
    pub fn preload_registered_partitions(
        &mut self,
        store_dir: &path::Path,
        partitions: &[RegisteredPartition],
    ) -> Result<usize, RouterError> {
        let mut loaded = 0usize;
        for part in partitions {
            let part_dir = store_dir.join(&part.relative_path);
            if let Some(loader) = &self.loader {
                loader.register_partition_dir(&part.partition_id, &part_dir);
            }
            if self.resident_1hz.contains_key(&part.partition_id) {
                continue;
            }
            if part_dir.join("overview_1hz.parquet").is_file() {
                self.preload_partition(
                    &part.partition_id,
                    &part_dir,
                    Some(&part.manifest.signals),
                )?;
                loaded += 1;
            }
        }
        Ok(loaded)
    }

    /// Insert a tile directly into the LRU cache.
    pub fn insert_tile(&self, key: TileKey, series: Arc<SignalSeries>) {
        if let Ok(mut dyn_guard) = self.dynamic.lock() {
            dyn_guard.in_flight.remove(&key);
            dyn_guard.lru.insert(key, series);
            self.revision.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Drain any completed background tile loads into the LRU cache.
    pub fn poll_completed(&self) -> usize {
        let Ok(mut dyn_guard) = self.dynamic.lock() else {
            return 0;
        };
        Self::poll_completed_locked(&mut dyn_guard, &self.revision)
    }

    fn poll_completed_locked(dyn_guard: &mut TileCacheDynamic, revision: &AtomicU64) -> usize {
        let mut completed = 0usize;
        while let Ok(msg) = dyn_guard.res_rx.try_recv() {
            dyn_guard.in_flight.remove(&msg.key);
            match msg.result {
                Ok(series) => {
                    dyn_guard.lru.insert(msg.key, series);
                    completed += 1;
                }
                Err(err) => {
                    log::warn!(
                        "Failed to load tile ({}, {}, {:?}): {err}",
                        msg.key.partition_id,
                        msg.key.signal_id,
                        msg.key.resolution
                    );
                }
            }
        }
        if completed > 0 {
            revision.fetch_add(1, Ordering::Relaxed);
        }
        completed
    }

    /// Block until all currently in-flight background requests complete (or `timeout` elapses).
    pub fn wait_for_in_flight(&self, timeout: time::Duration) -> usize {
        let Ok(mut dyn_guard) = self.dynamic.lock() else {
            return 0;
        };
        let start = crate::time::Time::now();
        let timeout_ms = timeout.as_millis().min(i64::MAX as u128) as i64;
        let mut total = Self::poll_completed_locked(&mut dyn_guard, &self.revision);
        while !dyn_guard.in_flight.is_empty() {
            let elapsed_ms = crate::time::Time::now()
                .unix_millis()
                .saturating_sub(start.unix_millis());
            if elapsed_ms >= timeout_ms {
                break;
            }
            let remaining = time::Duration::from_millis((timeout_ms - elapsed_ms).max(1) as u64);
            match dyn_guard.res_rx.recv_timeout(remaining) {
                Ok(msg) => {
                    dyn_guard.in_flight.remove(&msg.key);
                    if let Ok(series) = msg.result {
                        dyn_guard.lru.insert(msg.key, series);
                        total += 1;
                        self.revision.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Err(_) => break,
            }
            total += Self::poll_completed_locked(&mut dyn_guard, &self.revision);
        }
        drop(dyn_guard);
        drop(self.repaint_cb.lock());
        total
    }

    /// Query a signal from a partition at `resolution` without blocking the calling thread.
    pub fn query_signal(
        &self,
        partition_id: &str,
        signal_id: &str,
        resolution: Resolution,
    ) -> Option<TileLookup> {
        if let Some(known) = self.partition_signals.get(partition_id)
            && !known.contains(signal_id)
        {
            return None;
        }

        let resident_1hz = self
            .resident_1hz
            .get(partition_id)
            .and_then(|map| map.get(signal_id))
            .cloned();

        let Ok(mut dyn_guard) = self.dynamic.lock() else {
            return resident_1hz.map(TileLookup::Ready);
        };
        Self::poll_completed_locked(&mut dyn_guard, &self.revision);

        if resolution == Resolution::Overview1Hz {
            return resident_1hz.map(TileLookup::Ready);
        }

        let key = TileKey {
            partition_id: partition_id.to_owned(),
            signal_id: signal_id.to_owned(),
            resolution,
        };

        if let Some(cached) = dyn_guard.lru.get(&key) {
            return Some(TileLookup::Ready(cached));
        }

        if let Some(loader) = &self.loader
            && loader.can_load_async(&key)
            && dyn_guard.in_flight.insert(key.clone())
        {
            loader.request_load(key, self.res_tx.clone());
        }

        if resolution == Resolution::FullRes {
            let key_10hz = TileKey {
                partition_id: partition_id.to_owned(),
                signal_id: signal_id.to_owned(),
                resolution: Resolution::Overview10Hz,
            };
            if let Some(cached_10hz) = dyn_guard.lru.get(&key_10hz) {
                return Some(TileLookup::Fallback {
                    series: cached_10hz,
                    actual_resolution: Resolution::Overview10Hz,
                });
            }
        }

        resident_1hz.map(|series| TileLookup::Fallback {
            series,
            actual_resolution: Resolution::Overview1Hz,
        })
    }

    /// Synchronously load and cache a signal at the exact requested `resolution`.
    pub fn query_signal_blocking(
        &self,
        partition_id: &str,
        signal_id: &str,
        resolution: Resolution,
    ) -> Result<Option<Arc<SignalSeries>>, RouterError> {
        if let Some(known) = self.partition_signals.get(partition_id)
            && !known.contains(signal_id)
        {
            return Ok(None);
        }

        if resolution == Resolution::Overview1Hz {
            self.poll_completed();
            return Ok(self
                .resident_1hz
                .get(partition_id)
                .and_then(|map| map.get(signal_id))
                .cloned());
        }

        let key = TileKey {
            partition_id: partition_id.to_owned(),
            signal_id: signal_id.to_owned(),
            resolution,
        };

        if let Ok(mut dyn_guard) = self.dynamic.lock() {
            Self::poll_completed_locked(&mut dyn_guard, &self.revision);
            if let Some(cached) = dyn_guard.lru.get(&key) {
                return Ok(Some(cached));
            }
        }

        let Some(loader) = &self.loader else {
            return Ok(self
                .resident_1hz
                .get(partition_id)
                .and_then(|map| map.get(signal_id))
                .cloned());
        };

        let Some(series) = loader.load_blocking(&key)? else {
            return Ok(None);
        };

        if series.is_empty() {
            return Ok(None);
        }

        if let Ok(mut dyn_guard) = self.dynamic.lock() {
            dyn_guard.lru.insert(key, Arc::clone(&series));
            self.revision.fetch_add(1, Ordering::Relaxed);
        }
        Ok(Some(series))
    }

    /// Query resident discrete state intervals for `(partition_id, signal_id)`.
    pub fn query_states(
        &self,
        partition_id: &str,
        signal_id: &str,
    ) -> Option<Arc<Vec<StateInterval>>> {
        self.resident_states
            .get(partition_id)
            .and_then(|map| map.get(signal_id))
            .cloned()
    }

    /// Peek the newest sample `(utc_ms, value)` at or before `target_utc_ms` using the
    /// highest-resolution resident series currently in RAM without blocking.
    pub fn latest_resident_at_or_before(
        &self,
        partition_id: &str,
        signal_id: &str,
        target_utc_ms: i64,
    ) -> Option<(i64, f64)> {
        let full_key = TileKey {
            partition_id: partition_id.to_owned(),
            signal_id: signal_id.to_owned(),
            resolution: Resolution::FullRes,
        };
        let key_10hz = TileKey {
            partition_id: partition_id.to_owned(),
            signal_id: signal_id.to_owned(),
            resolution: Resolution::Overview10Hz,
        };

        if let Ok(dyn_guard) = self.dynamic.lock() {
            if let Some(series) = dyn_guard.lru.peek(&full_key)
                && let Some(found) = series.latest_at_or_before(target_utc_ms)
            {
                return Some(found);
            }
            if let Some(series) = dyn_guard.lru.peek(&key_10hz)
                && let Some(found) = series.latest_at_or_before(target_utc_ms)
            {
                return Some(found);
            }
        }

        self.resident_1hz
            .get(partition_id)
            .and_then(|map| map.get(signal_id))
            .and_then(|series| series.latest_at_or_before(target_utc_ms))
    }

    /// Check whether `sig_entry` in `partition_id` has at least one sample overlapping `range`.
    pub fn partition_signal_has_samples_in_range(
        &self,
        partition_id: &str,
        sig_entry: &storage::SignalManifestEntry,
        range: std::ops::RangeInclusive<i64>,
    ) -> bool {
        if range.is_empty() || sig_entry.sample_count == 0 {
            return false;
        }
        let start_ms = *range.start();
        let end_ms = *range.end();
        if sig_entry.min_utc_ms > end_ms || sig_entry.max_utc_ms < start_ms {
            return false;
        }

        let signal_id = &sig_entry.signal_id;
        let full_key = TileKey {
            partition_id: partition_id.to_owned(),
            signal_id: signal_id.clone(),
            resolution: Resolution::FullRes,
        };
        let key_10hz = TileKey {
            partition_id: partition_id.to_owned(),
            signal_id: signal_id.clone(),
            resolution: Resolution::Overview10Hz,
        };

        if let Ok(dyn_guard) = self.dynamic.lock() {
            if let Some(series) = dyn_guard.lru.peek(&full_key) {
                return series.has_points_in_range(start_ms..=end_ms);
            }
            if let Some(series) = dyn_guard.lru.peek(&key_10hz) {
                let lo_10hz = start_ms
                    .div_euclid(BUCKET_10HZ_MS)
                    .saturating_mul(BUCKET_10HZ_MS);
                return series.has_points_in_range(lo_10hz..=end_ms);
            }
        }

        if let Some(map_1hz) = self.resident_1hz.get(partition_id) {
            let lo_1hz = start_ms
                .div_euclid(BUCKET_1HZ_MS)
                .saturating_mul(BUCKET_1HZ_MS);
            if let Some(series) = map_1hz.get(signal_id)
                && series.has_points_in_range(lo_1hz..=end_ms)
            {
                return true;
            }
            if let Some(states) = self
                .resident_states
                .get(partition_id)
                .and_then(|m| m.get(signal_id))
                && states.iter().any(|iv| iv.overlaps(&range))
            {
                return true;
            }
            return false;
        }

        true
    }

    /// Peek the newest sample `(utc_ms, value)` within `range` without blocking.
    pub fn latest_resident_in_range(
        &self,
        partition_id: &str,
        signal_id: &str,
        range: std::ops::RangeInclusive<i64>,
    ) -> Option<(i64, f64)> {
        if range.is_empty() {
            return None;
        }
        let start_ms = *range.start();
        let end_ms = *range.end();

        let full_key = TileKey {
            partition_id: partition_id.to_owned(),
            signal_id: signal_id.to_owned(),
            resolution: Resolution::FullRes,
        };
        let key_10hz = TileKey {
            partition_id: partition_id.to_owned(),
            signal_id: signal_id.to_owned(),
            resolution: Resolution::Overview10Hz,
        };

        if let Ok(dyn_guard) = self.dynamic.lock() {
            if let Some(series) = dyn_guard.lru.peek(&full_key) {
                let (t, v) = series.latest_at_or_before(end_ms)?;
                return (t >= start_ms).then_some((t, v));
            }
            if let Some(series) = dyn_guard.lru.peek(&key_10hz) {
                let lo_10hz = start_ms
                    .div_euclid(BUCKET_10HZ_MS)
                    .saturating_mul(BUCKET_10HZ_MS);
                let (t, v) = series.latest_at_or_before(end_ms)?;
                return (t >= lo_10hz).then_some((t.max(start_ms), v));
            }
        }

        let lo_1hz = start_ms
            .div_euclid(BUCKET_1HZ_MS)
            .saturating_mul(BUCKET_1HZ_MS);
        let (t, v) = self
            .resident_1hz
            .get(partition_id)
            .and_then(|map| map.get(signal_id))
            .and_then(|series| series.latest_at_or_before(end_ms))?;
        (t >= lo_1hz).then_some((t.max(start_ms), v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lru_tile_map_evicts_least_recently_used_when_budget_exceeded() {
        let series_a = Arc::new(SignalSeries::Exact {
            timestamps_ms: vec![1, 2, 3, 4, 5, 6, 7, 8],
            values: vec![1.0; 8],
        });
        let single_tile_bytes = series_a.estimated_bytes();
        let mut lru = LruTileMap::new(single_tile_bytes * 2);

        let key_a = TileKey {
            partition_id: "p1".to_owned(),
            signal_id: "sig.a".to_owned(),
            resolution: Resolution::FullRes,
        };
        let key_b = TileKey {
            partition_id: "p1".to_owned(),
            signal_id: "sig.b".to_owned(),
            resolution: Resolution::FullRes,
        };
        let key_c = TileKey {
            partition_id: "p1".to_owned(),
            signal_id: "sig.c".to_owned(),
            resolution: Resolution::FullRes,
        };

        lru.insert(key_a.clone(), Arc::clone(&series_a));
        lru.insert(key_b.clone(), Arc::clone(&series_a));
        assert_eq!(lru.len(), 2);

        assert!(lru.get(&key_a).is_some());

        lru.insert(key_c.clone(), Arc::clone(&series_a));
        assert_eq!(lru.len(), 2);
        assert!(lru.get(&key_a).is_some());
        assert!(lru.get(&key_b).is_none());
        assert!(lru.get(&key_c).is_some());
    }
}
