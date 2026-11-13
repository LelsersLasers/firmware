use crate::{
    router::types::{EnvelopePoint, Resolution, SignalSeries, StateInterval},
    storage::{self, RegisteredPartition, SignalId},
};
use arrow::array::{Array as _, DictionaryArray, Float64Array, Int64Array, StringArray};
use arrow::datatypes::Int32Type;
use parquet::arrow::{ProjectionMask, arrow_reader::ParquetRecordBatchReaderBuilder};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt, fs, io, path,
    sync::{Arc, Mutex, mpsc},
    thread, time,
};

/// Default maximum memory budget for `Overview10Hz` and `FullRes` tiles in [`TileCache`] (512 MiB).
pub const DEFAULT_TILE_CACHE_BYTES: usize = 512 * 1024 * 1024;

pub type RepaintCallback = Arc<dyn Fn() + Send + Sync>;

#[derive(Debug)]
pub enum RouterError {
    Io(io::Error),
    Parquet(parquet::errors::ParquetError),
    Arrow(arrow::error::ArrowError),
    Registry(storage::RegistryError),
    InvalidColumn(&'static str),
}

impl fmt::Display for RouterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Parquet(e) => write!(f, "Parquet error: {e}"),
            Self::Arrow(e) => write!(f, "Arrow error: {e}"),
            Self::Registry(e) => write!(f, "Registry error: {e}"),
            Self::InvalidColumn(msg) => write!(f, "Invalid Parquet column schema: {msg}"),
        }
    }
}

impl std::error::Error for RouterError {}

impl From<io::Error> for RouterError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<parquet::errors::ParquetError> for RouterError {
    fn from(value: parquet::errors::ParquetError) -> Self {
        Self::Parquet(value)
    }
}

impl From<arrow::error::ArrowError> for RouterError {
    fn from(value: arrow::error::ArrowError) -> Self {
        Self::Arrow(value)
    }
}

impl From<storage::RegistryError> for RouterError {
    fn from(value: storage::RegistryError) -> Self {
        Self::Registry(value)
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

struct LruEntry {
    generation: u64,
    bytes: usize,
    series: Arc<SignalSeries>,
}

/// Byte-bounded LRU cache for `(partition_id, signal_id, Resolution)` column tiles.
struct LruTileMap {
    entries: HashMap<TileKey, LruEntry>,
    order: BTreeMap<u64, TileKey>,
    next_generation: u64,
    current_bytes: usize,
    max_bytes: usize,
}

impl LruTileMap {
    fn new(max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: BTreeMap::new(),
            next_generation: 0,
            current_bytes: 0,
            max_bytes: max_bytes.max(1),
        }
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn current_bytes(&self) -> usize {
        self.current_bytes
    }

    fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.current_bytes = 0;
    }

    fn get(&mut self, key: &TileKey) -> Option<Arc<SignalSeries>> {
        let entry = self.entries.get_mut(key)?;
        self.order.remove(&entry.generation);
        self.next_generation = self.next_generation.wrapping_add(1);
        entry.generation = self.next_generation;
        self.order.insert(entry.generation, key.clone());
        Some(Arc::clone(&entry.series))
    }

    fn peek(&self, key: &TileKey) -> Option<&Arc<SignalSeries>> {
        self.entries.get(key).map(|e| &e.series)
    }

    fn insert(&mut self, key: TileKey, series: Arc<SignalSeries>) {
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

enum WorkerCommand {
    LoadTile {
        key: TileKey,
        partition_dir: path::PathBuf,
    },
    Shutdown,
}

struct WorkerResult {
    key: TileKey,
    result: Result<Arc<SignalSeries>, RouterError>,
}

struct TileCacheDynamic {
    lru: LruTileMap,
    in_flight: HashSet<TileKey>,
    res_rx: mpsc::Receiver<WorkerResult>,
}

/// Multi-resolution RAM tile cache for canonical Parquet partitions.
///
/// - `overview_1hz.parquet` and `states.parquet` are preloaded into resident memory for all
///   local partitions in the active track day.
/// - `overview_10hz.parquet` and `full_res.parquet` columns are loaded on demand by a background
///   worker thread and cached in a byte-bounded LRU map (`DEFAULT_TILE_CACHE_BYTES`).
pub struct TileCache {
    resident_1hz: HashMap<String, HashMap<SignalId, Arc<SignalSeries>>>,
    resident_states: HashMap<String, HashMap<SignalId, Arc<Vec<StateInterval>>>>,
    partition_dirs: HashMap<String, path::PathBuf>,
    partition_signals: HashMap<String, HashSet<SignalId>>,
    dynamic: Mutex<TileCacheDynamic>,
    cmd_tx: mpsc::Sender<WorkerCommand>,
    repaint_cb: Arc<Mutex<Option<RepaintCallback>>>,
    worker_handle: Option<thread::JoinHandle<()>>,
}

impl Default for TileCache {
    fn default() -> Self {
        Self::new(DEFAULT_TILE_CACHE_BYTES)
    }
}

impl TileCache {
    pub fn new(max_lru_bytes: usize) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel::<WorkerCommand>();
        let (res_tx, res_rx) = mpsc::channel::<WorkerResult>();
        let repaint_cb: Arc<Mutex<Option<RepaintCallback>>> = Arc::new(Mutex::new(None));
        let worker_repaint = Arc::clone(&repaint_cb);

        let worker_handle = thread::Builder::new()
            .name("daq-tile-cache-reader".to_owned())
            .spawn(move || {
                while let Ok(cmd) = cmd_rx.recv() {
                    match cmd {
                        WorkerCommand::LoadTile { key, partition_dir } => {
                            let result = match key.resolution {
                                Resolution::FullRes => read_single_signal_full_res(
                                    &partition_dir.join("full_res.parquet"),
                                    &key.signal_id,
                                ),
                                Resolution::Overview10Hz => read_single_signal_overview(
                                    &partition_dir.join("overview_10hz.parquet"),
                                    &key.signal_id,
                                ),
                                Resolution::Overview1Hz => read_single_signal_overview(
                                    &partition_dir.join("overview_1hz.parquet"),
                                    &key.signal_id,
                                ),
                            };
                            let repaint_guard = worker_repaint.lock().ok();
                            if res_tx.send(WorkerResult { key, result }).is_err() {
                                break;
                            }
                            if let Some(cb) = repaint_guard.as_ref().and_then(|g| g.as_ref()) {
                                cb();
                            }
                            drop(repaint_guard);
                        }
                        WorkerCommand::Shutdown => break,
                    }
                }
            })
            .ok();

        Self {
            resident_1hz: HashMap::new(),
            resident_states: HashMap::new(),
            partition_dirs: HashMap::new(),
            partition_signals: HashMap::new(),
            dynamic: Mutex::new(TileCacheDynamic {
                lru: LruTileMap::new(max_lru_bytes),
                in_flight: HashSet::new(),
                res_rx,
            }),
            cmd_tx,
            repaint_cb,
            worker_handle,
        }
    }

    /// Configure a callback (e.g. `move || ctx.request_repaint()`) invoked whenever the
    /// background reader thread finishes loading an on-demand `Overview10Hz` or `FullRes` tile.
    pub fn set_repaint_callback(&self, callback: Option<RepaintCallback>) {
        if let Ok(mut guard) = self.repaint_cb.lock() {
            *guard = callback;
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
        self.resident_states.clear();
        self.partition_dirs.clear();
        self.partition_signals.clear();
        if let Ok(mut dyn_guard) = self.dynamic.lock() {
            dyn_guard.lru.clear();
            dyn_guard.in_flight.clear();
        }
    }

    /// Preload `overview_1hz.parquet` and `states.parquet` for a single partition into resident RAM.
    pub fn preload_partition(
        &mut self,
        partition_id: &str,
        partition_dir: impl AsRef<path::Path>,
        known_signals: Option<&[storage::SignalManifestEntry]>,
    ) -> Result<(), RouterError> {
        let partition_dir = partition_dir.as_ref().to_path_buf();
        if self.resident_1hz.contains_key(partition_id) {
            self.partition_dirs
                .insert(partition_id.to_owned(), partition_dir);
            return Ok(());
        }

        let overview_1hz_path = partition_dir.join("overview_1hz.parquet");
        let states_path = partition_dir.join("states.parquet");

        let overview_map = read_all_signals_overview(&overview_1hz_path)?;
        let states_map = if states_path.exists() {
            read_all_signals_states(&states_path)?
        } else {
            HashMap::new()
        };

        let mut signal_set: HashSet<SignalId> = overview_map.keys().cloned().collect();
        signal_set.extend(states_map.keys().cloned());
        if let Some(entries) = known_signals {
            for entry in entries {
                signal_set.insert(entry.signal_id.clone());
            }
        }

        self.partition_dirs
            .insert(partition_id.to_owned(), partition_dir);
        self.partition_signals
            .insert(partition_id.to_owned(), signal_set);
        self.resident_1hz
            .insert(partition_id.to_owned(), overview_map);
        self.resident_states
            .insert(partition_id.to_owned(), states_map);

        Ok(())
    }

    /// Preload all local partitions in `partitions` from `<store_dir>/<relative_path>`.
    pub fn preload_registered_partitions(
        &mut self,
        store_dir: &path::Path,
        partitions: &[RegisteredPartition],
    ) -> Result<usize, RouterError> {
        let mut loaded = 0usize;
        for part in partitions {
            if !part.is_local {
                continue;
            }
            if self.resident_1hz.contains_key(&part.partition_id) {
                continue;
            }
            let part_dir = store_dir.join(&part.relative_path);
            if part_dir.exists() {
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

    /// Drain any completed background tile reads into the LRU cache.
    /// Returns the number of newly populated tiles.
    pub fn poll_completed(&self) -> usize {
        let Ok(mut dyn_guard) = self.dynamic.lock() else {
            return 0;
        };
        Self::poll_completed_locked(&mut dyn_guard)
    }

    fn poll_completed_locked(dyn_guard: &mut TileCacheDynamic) -> usize {
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
                        "Failed to read tile ({}, {}, {:?}): {err}",
                        msg.key.partition_id,
                        msg.key.signal_id,
                        msg.key.resolution
                    );
                }
            }
        }
        completed
    }

    /// Block until all currently in-flight background requests complete (or `timeout` elapses).
    pub fn wait_for_in_flight(&self, timeout: time::Duration) -> usize {
        let Ok(mut dyn_guard) = self.dynamic.lock() else {
            return 0;
        };
        let deadline = time::Instant::now() + timeout;
        let mut total = Self::poll_completed_locked(&mut dyn_guard);
        while !dyn_guard.in_flight.is_empty() {
            let now = time::Instant::now();
            if now >= deadline {
                break;
            }
            let remaining = deadline.saturating_duration_since(now);
            match dyn_guard.res_rx.recv_timeout(remaining) {
                Ok(msg) => {
                    dyn_guard.in_flight.remove(&msg.key);
                    if let Ok(series) = msg.result {
                        dyn_guard.lru.insert(msg.key, series);
                        total += 1;
                    }
                }
                Err(_) => break,
            }
            total += Self::poll_completed_locked(&mut dyn_guard);
        }
        drop(dyn_guard);
        drop(self.repaint_cb.lock());
        total
    }

    /// Query a signal from a partition at `resolution` without blocking the calling thread.
    ///
    /// - If `Overview1Hz` is requested, returns the resident 1 Hz series immediately.
    /// - If `Overview10Hz` or `FullRes` is requested and present in the LRU cache, returns it immediately.
    /// - If `Overview10Hz` or `FullRes` is requested and NOT in the LRU cache, enqueues a background
    ///   worker read and immediately returns the best resident overview (`Overview10Hz` or `Overview1Hz`).
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
        Self::poll_completed_locked(&mut dyn_guard);

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

        // Enqueue background read if not already in flight
        if let Some(partition_dir) = self.partition_dirs.get(partition_id)
            && dyn_guard.in_flight.insert(key.clone())
        {
            let _ = self.cmd_tx.send(WorkerCommand::LoadTile {
                key,
                partition_dir: partition_dir.clone(),
            });
        }

        // Return best available resident fallback immediately
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
            Self::poll_completed_locked(&mut dyn_guard);
            if let Some(cached) = dyn_guard.lru.get(&key) {
                return Ok(Some(cached));
            }
        }

        let Some(partition_dir) = self.partition_dirs.get(partition_id).cloned() else {
            return Ok(None);
        };

        let series = match resolution {
            Resolution::FullRes => {
                read_single_signal_full_res(&partition_dir.join("full_res.parquet"), signal_id)?
            }
            Resolution::Overview10Hz => read_single_signal_overview(
                &partition_dir.join("overview_10hz.parquet"),
                signal_id,
            )?,
            Resolution::Overview1Hz => {
                read_single_signal_overview(&partition_dir.join("overview_1hz.parquet"), signal_id)?
            }
        };

        if series.is_empty() {
            return Ok(None);
        }

        if let Ok(mut dyn_guard) = self.dynamic.lock() {
            dyn_guard.lru.insert(key, Arc::clone(&series));
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
}

impl Drop for TileCache {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(WorkerCommand::Shutdown);
        if let Some(handle) = self.worker_handle.take() {
            let _ = handle.join();
        }
    }
}

/// Find the dictionary key for `target_signal_id` inside a `DictionaryArray<Int32Type>`.
fn find_dictionary_key(
    dict_col: &DictionaryArray<Int32Type>,
    target_signal_id: &str,
) -> Result<Option<i32>, RouterError> {
    let values = dict_col
        .values()
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or(RouterError::InvalidColumn(
            "signal_id dictionary values must be Utf8",
        ))?;

    for idx in 0..values.len() {
        if !values.is_null(idx) && values.value(idx) == target_signal_id {
            return Ok(Some(idx as i32));
        }
    }
    Ok(None)
}

/// Read all signals from an overview Parquet file (`overview_1hz.parquet` or `overview_10hz.parquet`).
pub fn read_all_signals_overview(
    path: &path::Path,
) -> Result<HashMap<SignalId, Arc<SignalSeries>>, RouterError> {
    let file = fs::File::open(path)?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
    let mask = ProjectionMask::roots(builder.parquet_schema(), [0, 1, 2, 3, 4]);
    let reader = builder.with_projection(mask).build()?;

    let mut map: HashMap<SignalId, Vec<EnvelopePoint>> = HashMap::new();

    for batch_result in reader {
        let batch = batch_result?;
        let sig_col = batch
            .column(0)
            .as_any()
            .downcast_ref::<DictionaryArray<Int32Type>>()
            .ok_or(RouterError::InvalidColumn(
                "overview col 0 must be Dictionary<Int32, Utf8>",
            ))?;
        let dict_values = sig_col
            .values()
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or(RouterError::InvalidColumn(
                "overview dictionary values must be Utf8",
            ))?;
        let bucket_col = batch
            .column(1)
            .as_any()
            .downcast_ref::<Int64Array>()
            .ok_or(RouterError::InvalidColumn("bucket_utc_ms must be Int64"))?;
        let min_col = batch
            .column(2)
            .as_any()
            .downcast_ref::<Float64Array>()
            .ok_or(RouterError::InvalidColumn("min_val must be Float64"))?;
        let max_col = batch
            .column(3)
            .as_any()
            .downcast_ref::<Float64Array>()
            .ok_or(RouterError::InvalidColumn("max_val must be Float64"))?;
        let mean_col = batch
            .column(4)
            .as_any()
            .downcast_ref::<Float64Array>()
            .ok_or(RouterError::InvalidColumn("mean_val must be Float64"))?;

        let keys = sig_col.keys().values();
        let buckets = bucket_col.values();
        let mins = min_col.values();
        let maxs = max_col.values();
        let means = mean_col.values();

        let mut current_key: Option<i32> = None;
        let mut current_vec: Option<&mut Vec<EnvelopePoint>> = None;

        for row in 0..batch.num_rows() {
            let key = keys[row];
            if current_key != Some(key) {
                let sig_name = dict_values.value(key as usize);
                current_key = Some(key);
                current_vec = Some(map.entry(sig_name.to_owned()).or_default());
            }
            if let Some(target_vec) = current_vec.as_mut() {
                target_vec.push(EnvelopePoint {
                    utc_ms: buckets[row],
                    min: mins[row],
                    max: maxs[row],
                    mean: means[row],
                });
            }
        }
    }

    Ok(map
        .into_iter()
        .map(|(k, pts)| (k, Arc::new(SignalSeries::Envelope(pts))))
        .collect())
}

/// Read a single signal from an overview Parquet file (`overview_10hz.parquet` or `overview_1hz.parquet`).
pub fn read_single_signal_overview(
    path: &path::Path,
    target_signal_id: &str,
) -> Result<Arc<SignalSeries>, RouterError> {
    let file = fs::File::open(path)?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
    let mask = ProjectionMask::roots(builder.parquet_schema(), [0, 1, 2, 3, 4]);
    let reader = builder.with_projection(mask).build()?;

    let mut points = Vec::new();

    for batch_result in reader {
        let batch = batch_result?;
        let sig_col = batch
            .column(0)
            .as_any()
            .downcast_ref::<DictionaryArray<Int32Type>>()
            .ok_or(RouterError::InvalidColumn(
                "overview col 0 must be Dictionary<Int32, Utf8>",
            ))?;
        let Some(target_key) = find_dictionary_key(sig_col, target_signal_id)? else {
            continue;
        };

        let bucket_col = batch
            .column(1)
            .as_any()
            .downcast_ref::<Int64Array>()
            .ok_or(RouterError::InvalidColumn("bucket_utc_ms must be Int64"))?;
        let min_col = batch
            .column(2)
            .as_any()
            .downcast_ref::<Float64Array>()
            .ok_or(RouterError::InvalidColumn("min_val must be Float64"))?;
        let max_col = batch
            .column(3)
            .as_any()
            .downcast_ref::<Float64Array>()
            .ok_or(RouterError::InvalidColumn("max_val must be Float64"))?;
        let mean_col = batch
            .column(4)
            .as_any()
            .downcast_ref::<Float64Array>()
            .ok_or(RouterError::InvalidColumn("mean_val must be Float64"))?;

        let keys = sig_col.keys().values();
        let buckets = bucket_col.values();
        let mins = min_col.values();
        let maxs = max_col.values();
        let means = mean_col.values();

        for row in 0..batch.num_rows() {
            if keys[row] == target_key {
                points.push(EnvelopePoint {
                    utc_ms: buckets[row],
                    min: mins[row],
                    max: maxs[row],
                    mean: means[row],
                });
            }
        }
    }

    Ok(Arc::new(SignalSeries::Envelope(points)))
}

/// Read a single signal's exact `(utc_ms, value)` series from `full_res.parquet`, skipping `ticks_ms`.
pub fn read_single_signal_full_res(
    path: &path::Path,
    target_signal_id: &str,
) -> Result<Arc<SignalSeries>, RouterError> {
    let file = fs::File::open(path)?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
    // Project only col 0 (signal_id), col 1 (utc_ms), and col 3 (value); skip col 2 (ticks_ms).
    let mask = ProjectionMask::roots(builder.parquet_schema(), [0, 1, 3]);
    let reader = builder.with_projection(mask).build()?;

    let mut timestamps_ms = Vec::new();
    let mut values = Vec::new();

    for batch_result in reader {
        let batch = batch_result?;
        let sig_col = batch
            .column(0)
            .as_any()
            .downcast_ref::<DictionaryArray<Int32Type>>()
            .ok_or(RouterError::InvalidColumn(
                "full_res col 0 must be Dictionary<Int32, Utf8>",
            ))?;
        let Some(target_key) = find_dictionary_key(sig_col, target_signal_id)? else {
            continue;
        };

        let utc_col = batch
            .column(1)
            .as_any()
            .downcast_ref::<Int64Array>()
            .ok_or(RouterError::InvalidColumn("utc_ms must be Int64"))?;
        let val_col = batch
            .column(2)
            .as_any()
            .downcast_ref::<Float64Array>()
            .ok_or(RouterError::InvalidColumn("value must be Float64"))?;

        let keys = sig_col.keys().values();
        let ts_slice = utc_col.values();
        let val_slice = val_col.values();

        // Fast path: because full_res.parquet is written grouped by signal_id, locate the
        // first and last matching row in the batch and verify contiguity for direct slice copy.
        if let Some(first_idx) = keys.iter().position(|&k| k == target_key) {
            let last_idx = keys
                .iter()
                .rposition(|&k| k == target_key)
                .unwrap_or(first_idx);
            let span_len = last_idx - first_idx + 1;
            let is_contiguous = keys[first_idx..=last_idx].iter().all(|&k| k == target_key);
            if is_contiguous {
                timestamps_ms.reserve(span_len);
                values.reserve(span_len);
                timestamps_ms.extend_from_slice(&ts_slice[first_idx..=last_idx]);
                values.extend_from_slice(&val_slice[first_idx..=last_idx]);
            } else {
                for row in first_idx..=last_idx {
                    if keys[row] == target_key {
                        timestamps_ms.push(ts_slice[row]);
                        values.push(val_slice[row]);
                    }
                }
            }
        }
    }

    Ok(Arc::new(SignalSeries::Exact {
        timestamps_ms,
        values,
    }))
}

/// Read all discrete state intervals from `states.parquet`.
pub fn read_all_signals_states(
    path: &path::Path,
) -> Result<HashMap<SignalId, Arc<Vec<StateInterval>>>, RouterError> {
    let file = fs::File::open(path)?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
    let reader = builder.build()?;

    let mut map: HashMap<SignalId, Vec<StateInterval>> = HashMap::new();

    for batch_result in reader {
        let batch = batch_result?;
        let sig_col = batch
            .column(0)
            .as_any()
            .downcast_ref::<DictionaryArray<Int32Type>>()
            .ok_or(RouterError::InvalidColumn(
                "states col 0 must be Dictionary<Int32, Utf8>",
            ))?;
        let dict_values = sig_col
            .values()
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or(RouterError::InvalidColumn(
                "states dictionary values must be Utf8",
            ))?;
        let start_col = batch
            .column(1)
            .as_any()
            .downcast_ref::<Int64Array>()
            .ok_or(RouterError::InvalidColumn("start_utc_ms must be Int64"))?;
        let end_col = batch
            .column(2)
            .as_any()
            .downcast_ref::<Int64Array>()
            .ok_or(RouterError::InvalidColumn("end_utc_ms must be Int64"))?;
        let raw_col = batch
            .column(3)
            .as_any()
            .downcast_ref::<Int64Array>()
            .ok_or(RouterError::InvalidColumn("raw_int must be Int64"))?;
        let label_col = batch
            .column(4)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or(RouterError::InvalidColumn("label must be Utf8"))?;

        let keys = sig_col.keys().values();
        let starts = start_col.values();
        let ends = end_col.values();
        let raws = raw_col.values();

        for row in 0..batch.num_rows() {
            let sig_name = dict_values.value(keys[row] as usize);
            map.entry(sig_name.to_owned())
                .or_default()
                .push(StateInterval {
                    start_utc_ms: starts[row],
                    end_utc_ms: ends[row],
                    raw_int: raws[row],
                    label: label_col.value(row).to_owned(),
                });
        }
    }

    Ok(map.into_iter().map(|(k, v)| (k, Arc::new(v))).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log_parse::{correlate, parse};
    use crate::storage::write_correlated_epochs_to_store;
    use crate::superdbc::{database::Database, decode::DecodedMessage};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn test_database() -> Database {
        let json = r#"{
          "content_hash": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
          "versions": { "schema_version": "1.1", "hash": "test_v1" },
          "buses": {
            "VCAN": {
              "bus_id": 0,
              "baud_rate": 500000,
              "nodes": [{ "name": "MAIN_MODULE", "is_external": false }],
              "messages": [
                {
                  "id": 100,
                  "is_extended_id": false,
                  "message_name": "wheel_speeds",
                  "transmitter": "MAIN_MODULE",
                  "receivers": [],
                  "length_bytes": 8,
                  "nominal_period_ms": 100,
                  "priority": 1,
                  "description": "Wheel speed",
                  "signals": [
                    {
                      "signal_name": "speed_mps",
                      "description": "",
                      "data_type": "uint16_t",
                      "raw_type": "unsigned",
                      "start_bit": 0,
                      "bit_length": 16,
                      "byte_order": "little_endian",
                      "scale": 0.1,
                      "offset": 0.0,
                      "limits": null,
                      "unit": "m/s",
                      "choices": null,
                      "display_format": null
                    }
                  ]
                },
                {
                  "id": 200,
                  "is_extended_id": false,
                  "message_name": "main_hb",
                  "transmitter": "MAIN_MODULE",
                  "receivers": [],
                  "length_bytes": 1,
                  "nominal_period_ms": 100,
                  "priority": 2,
                  "description": "Main heartbeat",
                  "signals": [
                    {
                      "signal_name": "car_state",
                      "description": "",
                      "data_type": "uint8_t",
                      "raw_type": "unsigned",
                      "start_bit": 0,
                      "bit_length": 8,
                      "byte_order": "little_endian",
                      "scale": 1.0,
                      "offset": 0.0,
                      "limits": null,
                      "unit": "",
                      "choices": { "0": "IDLE", "1": "READY" },
                      "display_format": null
                    }
                  ]
                }
              ]
            }
          }
        }"#;
        Database::from_json(json).expect("valid test superdbc")
    }

    fn decode_msg(db: &Database, id: u32, payload: &[u8]) -> DecodedMessage {
        db.decode(0, id, payload).expect("decode message")
    }

    #[test]
    fn tile_cache_preloads_1hz_and_falls_back_non_blocking_before_loading_full_res() {
        let db = test_database();
        let tmp_dir =
            std::env::temp_dir().join(format!("daq_tile_cache_test_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&tmp_dir).unwrap();

        let base_utc_ms = 1_760_000_000_000i64;
        let mut parsed_msgs = Vec::new();
        for i in 0..25u32 {
            let ticks_ms = i * 100; // 10 Hz for 2.5 seconds
            let raw_speed = ((10 + i) * 10) as u16; // 10.0 .. 34.0 m/s
            let mut payload = [0u8; 8];
            payload[0..2].copy_from_slice(&raw_speed.to_le_bytes());
            parsed_msgs.push(parse::ParsedMessage {
                timestamp: ticks_ms,
                bus_name: "VCAN".to_owned(),
                source_file_idx: 0,
                decoded: decode_msg(&db, 100, &payload),
            });

            let state_val = u8::from(i >= 10);
            parsed_msgs.push(parse::ParsedMessage {
                timestamp: ticks_ms,
                bus_name: "VCAN".to_owned(),
                source_file_idx: 0,
                decoded: decode_msg(&db, 200, &[state_val]),
            });
        }

        let epoch = correlate::CorrelationChunkResult {
            parsed_msgs,
            clock_mapping: correlate::ClockMapping {
                slope: 1.0,
                intercept_ms: base_utc_ms as f64,
                confidence: correlate::ClockConfidence::RtcEstimated,
            },
        };
        let log_files = vec![parse::SourceLogFile {
            file_name: "log-2026-09-06--18-36-02.log".to_owned(),
            sha256_hex: "deadbeef".repeat(8),
            rtc_timestamp_ms: Some(base_utc_ms),
            mtime_unix_ms: base_utc_ms,
        }];

        let summary = write_correlated_epochs_to_store(&tmp_dir, &log_files, vec![epoch], &db)
            .expect("write partition");
        assert_eq!(summary.manifests.len(), 1);
        let manifest = &summary.manifests[0];
        let part_dir = tmp_dir.join("partitions").join(&manifest.partition_id);

        let mut cache = TileCache::new(DEFAULT_TILE_CACHE_BYTES);
        let repaints = Arc::new(AtomicUsize::new(0));
        let repaints_clone = Arc::clone(&repaints);
        cache.set_repaint_callback(Some(Arc::new(move || {
            repaints_clone.fetch_add(1, Ordering::SeqCst);
        })));

        cache
            .preload_partition(&manifest.partition_id, &part_dir, Some(&manifest.signals))
            .expect("preload partition");

        // 1. Resident 1Hz overview is immediately Ready
        let lookup_1hz = cache
            .query_signal(
                &manifest.partition_id,
                "wheel_speeds.speed_mps",
                Resolution::Overview1Hz,
            )
            .expect("signal exists");
        assert!(!lookup_1hz.is_fallback());
        assert_eq!(lookup_1hz.series().len(), 3);

        // 2. Resident discrete states are immediately available
        let states = cache
            .query_states(&manifest.partition_id, "main_hb.car_state")
            .expect("discrete state exists");
        assert_eq!(states.len(), 2);
        assert_eq!(states[0].label, "IDLE");
        assert_eq!(states[1].label, "READY");

        // 3. Cold FullRes query immediately returns resident Overview1Hz Fallback without blocking
        let cold_full = cache
            .query_signal(
                &manifest.partition_id,
                "wheel_speeds.speed_mps",
                Resolution::FullRes,
            )
            .expect("signal exists");
        assert!(cold_full.is_fallback());
        assert!(matches!(
            cold_full.series().as_ref(),
            SignalSeries::Envelope(_)
        ));

        // 4. Wait for background worker to finish and verify RepaintCallback + Ready FullRes
        let completed = cache.wait_for_in_flight(time::Duration::from_secs(2));
        assert_eq!(completed, 1);
        assert!(repaints.load(Ordering::SeqCst) >= 1);

        let warm_full = cache
            .query_signal(
                &manifest.partition_id,
                "wheel_speeds.speed_mps",
                Resolution::FullRes,
            )
            .expect("signal exists");
        assert!(!warm_full.is_fallback());
        match warm_full.series().as_ref() {
            SignalSeries::Exact {
                timestamps_ms,
                values,
            } => {
                assert_eq!(timestamps_ms.len(), 25);
                assert!((values[0] - 10.0).abs() < 1e-6);
                assert!((values[24] - 34.0).abs() < 1e-6);
            }
            other => panic!("Expected Exact series, got {other:?}"),
        }

        let _ = fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn lru_tile_map_evicts_least_recently_used_when_budget_exceeded() {
        let series_a = Arc::new(SignalSeries::Exact {
            timestamps_ms: vec![1, 2, 3, 4, 5, 6, 7, 8],
            values: vec![1.0; 8],
        });
        let single_tile_bytes = series_a.estimated_bytes();
        // Allow at most 2 tiles in the budget
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

        // Touch key_a so key_b becomes the least-recently-used entry
        assert!(lru.get(&key_a).is_some());

        // Inserting key_c exceeds the 2-tile byte budget and must evict key_b
        lru.insert(key_c.clone(), Arc::clone(&series_a));
        assert_eq!(lru.len(), 2);
        assert!(lru.get(&key_a).is_some());
        assert!(lru.get(&key_b).is_none());
        assert!(lru.get(&key_c).is_some());
    }
}
