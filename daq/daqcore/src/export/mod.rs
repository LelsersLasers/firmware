use crate::{
    analysis::{
        AnalysisError, TargetGrid, canonicalize_signal_ref, resample_continuous_linear,
        resample_discrete_hold, series_to_timestamp_values,
    },
    router::{Resolution, RouterError, SignalSeries, VirtualTimelineRouter},
    superdbc::database::Database,
};
use arrow::{
    array::{ArrayRef, Float64Array, Int64Array, RecordBatch},
    datatypes::{DataType, Field, Schema},
};
use globset::GlobBuilder;
use indexmap::IndexMap;
use parquet::{
    arrow::{ArrowWriter, arrow_reader::ParquetRecordBatchReaderBuilder},
    basic::{Compression, ZstdLevel},
    file::properties::WriterProperties,
};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt, fs,
    io::{self, BufWriter, Write},
    ops::RangeInclusive,
    path::{Path, PathBuf},
    sync::Arc,
};

/// Embedded JSON configuration from `daq/config/export_presets.json`.
pub const BUILTIN_EXPORT_PRESETS_JSON: &str = include_str!("../../../config/export_presets.json");

/// Default sampling rate (`Hz`) when a uniform grid is requested without an explicit rate.
pub const DEFAULT_UNIFORM_EXPORT_RATE_HZ: f64 = 100.0;

#[derive(Debug)]
pub enum ExportError {
    Io(io::Error),
    Router(RouterError),
    Analysis(AnalysisError),
    Arrow(arrow::error::ArrowError),
    Parquet(parquet::errors::ParquetError),
    ConfigParse(String),
    UnknownPreset(String),
    UnknownAnnotation(String),
    InvalidTimeRange { start_utc_ms: i64, end_utc_ms: i64 },
    InvalidSamplingRate(f64),
    NoMatchingSignals,
    EmptyDataset,
}

impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error during export: {e}"),
            Self::Router(e) => write!(f, "router error during export: {e}"),
            Self::Analysis(e) => write!(f, "formula evaluation error during export: {e}"),
            Self::Arrow(e) => write!(f, "Arrow error during export: {e}"),
            Self::Parquet(e) => write!(f, "Parquet error during export: {e}"),
            Self::ConfigParse(msg) => write!(f, "failed to parse export presets config: {msg}"),
            Self::UnknownPreset(id) => write!(f, "unknown export preset '{id}'"),
            Self::UnknownAnnotation(name) => {
                write!(f, "no event annotation found matching '{name}'")
            }
            Self::InvalidTimeRange {
                start_utc_ms,
                end_utc_ms,
            } => write!(
                f,
                "invalid export time range [{start_utc_ms}..={end_utc_ms}]"
            ),
            Self::InvalidSamplingRate(rate) => {
                write!(
                    f,
                    "invalid uniform grid sampling rate {rate} Hz (must be > 0)"
                )
            }
            Self::NoMatchingSignals => {
                write!(
                    f,
                    "none of the requested signal patterns matched available data"
                )
            }
            Self::EmptyDataset => {
                write!(f, "no samples found in the requested export time range")
            }
        }
    }
}

impl std::error::Error for ExportError {}

impl From<io::Error> for ExportError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<RouterError> for ExportError {
    fn from(value: RouterError) -> Self {
        Self::Router(value)
    }
}

impl From<AnalysisError> for ExportError {
    fn from(value: AnalysisError) -> Self {
        Self::Analysis(value)
    }
}

impl From<arrow::error::ArrowError> for ExportError {
    fn from(value: arrow::error::ArrowError) -> Self {
        Self::Arrow(value)
    }
}

impl From<parquet::errors::ParquetError> for ExportError {
    fn from(value: parquet::errors::ParquetError) -> Self {
        Self::Parquet(value)
    }
}

/// Time-grid alignment mode used when exporting multi-rate CAN signals and derived channels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SamplingMode {
    /// Synchronized timetable at a uniform frequency (`rate_hz`), using staleness-aware linear
    /// interpolation for continuous signals and zero-order hold for discrete states.
    UniformGridHz(f64),
    /// Sorted union of all native sample timestamps across the exported signals.
    SparseRaw,
}

impl Default for SamplingMode {
    fn default() -> Self {
        Self::UniformGridHz(DEFAULT_UNIFORM_EXPORT_RATE_HZ)
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct RawSamplingConfig {
    mode: String,
    #[serde(default)]
    rate_hz: Option<f64>,
}

impl serde::Serialize for SamplingMode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let raw = match *self {
            Self::UniformGridHz(rate_hz) => RawSamplingConfig {
                mode: "uniform_grid_hz".to_owned(),
                rate_hz: Some(rate_hz),
            },
            Self::SparseRaw => RawSamplingConfig {
                mode: "sparse_raw".to_owned(),
                rate_hz: None,
            },
        };
        raw.serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for SamplingMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawSamplingConfig::deserialize(deserializer)?;
        match raw.mode.trim().to_ascii_lowercase().as_str() {
            "uniform_grid_hz" | "uniform" => {
                let rate = raw.rate_hz.unwrap_or(DEFAULT_UNIFORM_EXPORT_RATE_HZ);
                if !rate.is_finite() || rate <= 0.0 {
                    return Err(serde::de::Error::custom(format!(
                        "invalid rate_hz {rate} for uniform_grid_hz"
                    )));
                }
                Ok(Self::UniformGridHz(rate))
            }
            "sparse_raw" | "sparse" | "raw" => Ok(Self::SparseRaw),
            other => Err(serde::de::Error::custom(format!(
                "unknown sampling mode '{other}' (expected 'uniform_grid_hz' or 'sparse_raw')"
            ))),
        }
    }
}

/// Output file format for signal dataset export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    #[default]
    Parquet,
    Csv,
    MatlabMat,
}

impl ExportFormat {
    pub fn parse_format(input: &str) -> Option<Self> {
        match input
            .trim()
            .trim_start_matches('.')
            .to_ascii_lowercase()
            .as_str()
        {
            "parquet" | "pq" => Some(Self::Parquet),
            "csv" => Some(Self::Csv),
            "mat" | "matlab" | "matlab_mat" => Some(Self::MatlabMat),
            _ => None,
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Parquet => "parquet",
            Self::Csv => "csv",
            Self::MatlabMat => "mat",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Parquet => "Parquet (.parquet)",
            Self::Csv => "CSV (.csv)",
            Self::MatlabMat => "MATLAB (.mat)",
        }
    }
}

/// Subteam signal export preset loaded from `daq/config/export_presets.json`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExportPresetDef {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub default_sampling: SamplingMode,
    pub signals: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExportPresetsConfigFile {
    pub presets: Vec<ExportPresetDef>,
}

/// Registry of subteam signal export presets.
#[derive(Debug, Clone)]
pub struct ExportPresetRegistry {
    presets: Vec<ExportPresetDef>,
    index_by_id: BTreeMap<String, usize>,
}

impl Default for ExportPresetRegistry {
    fn default() -> Self {
        Self::with_builtin()
    }
}

impl ExportPresetRegistry {
    pub fn with_builtin() -> Self {
        // The embedded `daq/config/export_presets.json` is version-controlled and validated by unit tests.
        Self::from_json_str(BUILTIN_EXPORT_PRESETS_JSON)
            .expect("embedded daq/config/export_presets.json must be valid JSON")
    }

    pub fn from_defs(defs: Vec<ExportPresetDef>) -> Result<Self, ExportError> {
        let mut index_by_id = BTreeMap::new();
        for (idx, preset) in defs.iter().enumerate() {
            let clean_id = preset.id.trim().to_ascii_lowercase();
            if clean_id.is_empty() {
                return Err(ExportError::ConfigParse(
                    "export preset id must not be empty".to_owned(),
                ));
            }
            if index_by_id.insert(clean_id.clone(), idx).is_some() {
                return Err(ExportError::ConfigParse(format!(
                    "duplicate export preset id '{clean_id}'"
                )));
            }
        }
        Ok(Self {
            presets: defs,
            index_by_id,
        })
    }

    pub fn from_json_str(json: &str) -> Result<Self, ExportError> {
        let parsed: ExportPresetsConfigFile = serde_json::from_str(json)
            .map_err(|e| ExportError::ConfigParse(format!("invalid export_presets.json: {e}")))?;
        Self::from_defs(parsed.presets)
    }

    pub fn load_json_file(path: impl AsRef<Path>) -> Result<Self, ExportError> {
        let content = fs::read_to_string(path.as_ref())?;
        Self::from_json_str(&content)
    }

    pub fn presets(&self) -> &[ExportPresetDef] {
        &self.presets
    }

    pub fn get(&self, id: &str) -> Option<&ExportPresetDef> {
        let key = id.trim().to_ascii_lowercase();
        let idx = *self.index_by_id.get(&key)?;
        self.presets.get(idx)
    }
}

/// Metadata for a candidate exportable signal or derived formula channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportSignalMeta {
    pub signal_id: String,
    pub label: String,
    pub unit: String,
    pub is_derived: bool,
    pub is_discrete: bool,
    pub has_samples_in_range: bool,
}

/// A single materialized signal column inside an [`ExportTable`].
#[derive(Debug, Clone, PartialEq)]
pub struct ExportColumn {
    pub signal_id: String,
    pub label: String,
    pub unit: String,
    pub values: Vec<f64>,
}

/// Materialized time-aligned table ready to be written to Parquet, CSV, or MATLAB `.mat`.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportTable {
    pub timestamps_utc_ms: Vec<i64>,
    pub elapsed_s: Vec<f64>,
    pub columns: Vec<ExportColumn>,
}

impl ExportTable {
    pub fn row_count(&self) -> usize {
        self.timestamps_utc_ms.len()
    }

    pub fn column_count(&self) -> usize {
        self.columns.len()
    }

    pub fn column(&self, signal_id: &str) -> Option<&ExportColumn> {
        let canonical = canonicalize_signal_ref(signal_id);
        self.columns
            .iter()
            .find(|c| c.signal_id == signal_id || c.signal_id == canonical)
    }
}

/// Summary returned after writing an [`ExportTable`] to disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportSummary {
    pub output_path: PathBuf,
    pub format: ExportFormat,
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    pub row_count: usize,
    pub signal_count: usize,
    pub signals: Vec<(String, String)>,
}

/// Discover all raw signals and derived channels that have samples in `router` across `range`.
pub fn discover_available_signals(
    router: &VirtualTimelineRouter,
    range: RangeInclusive<i64>,
    _database: Option<&Database>,
) -> IndexMap<String, ExportSignalMeta> {
    let mut catalog: IndexMap<String, ExportSignalMeta> = IndexMap::new();

    // 1. Raw signals active in `range` (from overlapping partitions and live buffer)
    for meta in router.active_signals_in_range(range) {
        catalog.insert(
            meta.signal_id.clone(),
            ExportSignalMeta {
                signal_id: meta.signal_id.clone(),
                label: meta.signal_id,
                unit: meta.unit,
                is_derived: false,
                is_discrete: meta.is_discrete,
                has_samples_in_range: true,
            },
        );
    }

    // 2. Derived signals from FormulaEngine whose leaf dependencies exist in `range`
    for compiled in router.formula_engine().definitions() {
        let raw_deps_available = if let Ok((_, leaf_raw)) = router
            .formula_engine()
            .execution_plan_for_expr(&compiled.ast)
        {
            !leaf_raw.is_empty()
                && leaf_raw.iter().all(|raw_id| {
                    catalog
                        .get(raw_id)
                        .is_some_and(|meta| meta.has_samples_in_range)
                })
        } else {
            false
        };
        if raw_deps_available {
            catalog.insert(
                compiled.def.id.clone(),
                ExportSignalMeta {
                    signal_id: compiled.def.id.clone(),
                    label: compiled.def.label.clone(),
                    unit: compiled.def.unit.clone(),
                    is_derived: true,
                    is_discrete: false,
                    has_samples_in_range: true,
                },
            );
        }
    }

    catalog
}

/// Expand a list of signal patterns (e.g. `["IMU_acceleration.*", "battery.power_w"]`)
/// against the signals available in `router` over `range`.
///
/// Wildcard patterns only include signals that actually have samples (or active leaf dependencies)
/// in `range` when active signals exist, preventing unpopulated DBC signals from cluttering exports.
pub fn expand_signal_patterns(
    signal_patterns: &[String],
    router: &VirtualTimelineRouter,
    range: RangeInclusive<i64>,
    database: Option<&Database>,
) -> Result<Vec<ExportSignalMeta>, ExportError> {
    let catalog = discover_available_signals(router, range, database);
    let any_active_in_range = catalog.values().any(|m| m.has_samples_in_range);
    let mut selected: IndexMap<String, ExportSignalMeta> = IndexMap::new();

    for raw_pattern in signal_patterns {
        let pattern = canonicalize_signal_ref(raw_pattern);
        if pattern.is_empty() {
            continue;
        }
        let is_wildcard = pattern.contains('*') || pattern.contains('?');
        if !is_wildcard {
            if let Some(meta) = catalog.get(&pattern)
                && (!any_active_in_range || meta.has_samples_in_range)
            {
                selected.insert(meta.signal_id.clone(), meta.clone());
            }
            continue;
        }

        let glob = GlobBuilder::new(&pattern)
            .case_insensitive(false)
            .build()
            .map_err(|e| ExportError::ConfigParse(format!("invalid glob '{pattern}': {e}")))?
            .compile_matcher();

        for meta in catalog.values() {
            if glob.is_match(&meta.signal_id) && (!any_active_in_range || meta.has_samples_in_range)
            {
                selected.insert(meta.signal_id.clone(), meta.clone());
            }
        }
    }

    // If strict active filtering yielded nothing (e.g. querying a synthetic/empty range),
    // fall back to matching against the full catalog without the `has_samples_in_range` filter.
    if selected.is_empty() {
        for raw_pattern in signal_patterns {
            let pattern = canonicalize_signal_ref(raw_pattern);
            if pattern.is_empty() {
                continue;
            }
            let is_wildcard = pattern.contains('*') || pattern.contains('?');
            if !is_wildcard {
                if let Some(meta) = catalog.get(&pattern) {
                    selected.insert(meta.signal_id.clone(), meta.clone());
                }
                continue;
            }
            let glob = GlobBuilder::new(&pattern)
                .case_insensitive(false)
                .build()
                .map_err(|e| ExportError::ConfigParse(format!("invalid glob '{pattern}': {e}")))?
                .compile_matcher();
            for meta in catalog.values() {
                if glob.is_match(&meta.signal_id) {
                    selected.insert(meta.signal_id.clone(), meta.clone());
                }
            }
        }
    }

    if selected.is_empty() {
        return Err(ExportError::NoMatchingSignals);
    }

    Ok(selected.into_values().collect())
}

/// Materialize a synchronized [`ExportTable`] over `range` for `signal_patterns` at `sampling_mode`.
pub fn materialize_export_table(
    router: &VirtualTimelineRouter,
    range: RangeInclusive<i64>,
    signal_patterns: &[String],
    sampling_mode: SamplingMode,
    database: Option<&Database>,
) -> Result<ExportTable, ExportError> {
    let start_utc_ms = *range.start();
    let end_utc_ms = *range.end();
    if end_utc_ms < start_utc_ms {
        return Err(ExportError::InvalidTimeRange {
            start_utc_ms,
            end_utc_ms,
        });
    }
    if let SamplingMode::UniformGridHz(rate_hz) = sampling_mode
        && (!rate_hz.is_finite() || rate_hz <= 0.0)
    {
        return Err(ExportError::InvalidSamplingRate(rate_hz));
    }

    let metas = expand_signal_patterns(signal_patterns, router, range.clone(), database)?;

    // Collect all required raw leaf signals (both directly requested and referenced by derived formulas)
    let mut raw_ids_needed: BTreeSet<String> = BTreeSet::new();
    for meta in &metas {
        if meta.is_derived {
            if let Some(compiled) = router.formula_engine().get(&meta.signal_id) {
                let (_, leaves) = router
                    .formula_engine()
                    .execution_plan_for_expr(&compiled.ast)?;
                for leaf in leaves {
                    raw_ids_needed.insert(leaf);
                }
            }
        } else {
            raw_ids_needed.insert(meta.signal_id.clone());
        }
    }

    let mut raw_series_map: BTreeMap<String, SignalSeries> = BTreeMap::new();
    let mut raw_samples_map: BTreeMap<String, (Vec<i64>, Vec<f64>)> = BTreeMap::new();
    for raw_id in &raw_ids_needed {
        let series =
            router.query_raw_signal_blocking(raw_id, range.clone(), Resolution::FullRes)?;
        let (ts, vals) = series_to_timestamp_values(&series);
        raw_series_map.insert(raw_id.clone(), series);
        raw_samples_map.insert(raw_id.clone(), (ts, vals));
    }

    // Build target timestamp vector
    let target_grid = match sampling_mode {
        SamplingMode::UniformGridHz(rate_hz) => TargetGrid::UniformGridHz {
            start_utc_ms,
            end_utc_ms,
            rate_hz,
        },
        SamplingMode::SparseRaw => TargetGrid::Union,
    };
    let ts_slices: Vec<&[i64]> = raw_samples_map
        .values()
        .map(|(ts, _)| ts.as_slice())
        .collect();
    let timestamps_utc_ms = target_grid.build_timestamps(ts_slices);
    if timestamps_utc_ms.is_empty() {
        return Err(ExportError::EmptyDataset);
    }

    let origin_ms = match sampling_mode {
        SamplingMode::UniformGridHz(_) => start_utc_ms,
        SamplingMode::SparseRaw => timestamps_utc_ms[0],
    };
    let elapsed_s: Vec<f64> = timestamps_utc_ms
        .iter()
        .map(|&t| (t - origin_ms) as f64 / 1000.0)
        .collect();

    let max_staleness_ms = router.formula_engine().max_staleness_ms();
    let explicit_grid = TargetGrid::Explicit(timestamps_utc_ms.clone());
    let mut columns = Vec::with_capacity(metas.len());

    for meta in metas {
        let values = if meta.is_derived {
            let series = router.formula_engine().evaluate_channel_with(
                &meta.signal_id,
                &explicit_grid,
                Resolution::FullRes,
                |raw_id| {
                    raw_series_map
                        .get(raw_id)
                        .cloned()
                        .unwrap_or_else(|| SignalSeries::empty_for(Resolution::FullRes))
                },
            )?;
            let (eval_ts, eval_vals) = series_to_timestamp_values(&series);
            if eval_ts.len() == timestamps_utc_ms.len() && eval_ts == timestamps_utc_ms {
                eval_vals
            } else {
                resample_continuous_linear(
                    &eval_ts,
                    &eval_vals,
                    &timestamps_utc_ms,
                    max_staleness_ms,
                )
            }
        } else {
            let (src_ts, src_vals) = raw_samples_map
                .get(&meta.signal_id)
                .map(|(ts, vals)| (ts.as_slice(), vals.as_slice()))
                .unwrap_or((&[], &[]));

            match sampling_mode {
                SamplingMode::SparseRaw => {
                    resample_sparse_exact(src_ts, src_vals, &timestamps_utc_ms)
                }
                SamplingMode::UniformGridHz(_) => {
                    if meta.is_discrete {
                        let states = router.query_states(&meta.signal_id, range.clone());
                        if !states.is_empty() {
                            resample_discrete_hold(&states, &timestamps_utc_ms)
                        } else {
                            resample_continuous_linear(
                                src_ts,
                                src_vals,
                                &timestamps_utc_ms,
                                max_staleness_ms,
                            )
                        }
                    } else {
                        resample_continuous_linear(
                            src_ts,
                            src_vals,
                            &timestamps_utc_ms,
                            max_staleness_ms,
                        )
                    }
                }
            }
        };

        // In SparseRaw mode, omit columns that turned out to have zero finite values in the window
        if sampling_mode == SamplingMode::SparseRaw && !values.iter().any(|v| v.is_finite()) {
            continue;
        }

        columns.push(ExportColumn {
            signal_id: meta.signal_id,
            label: meta.label,
            unit: meta.unit,
            values,
        });
    }

    if columns.is_empty() {
        return Err(ExportError::EmptyDataset);
    }

    Ok(ExportTable {
        timestamps_utc_ms,
        elapsed_s,
        columns,
    })
}

/// Place exact samples `(src_ts, src_vals)` onto a superset sorted timestamp grid `target_grid_ms`,
/// emitting `f64::NAN` at timestamps where this signal did not record a sample.
fn resample_sparse_exact(src_ts: &[i64], src_vals: &[f64], target_grid_ms: &[i64]) -> Vec<f64> {
    let n = src_ts.len().min(src_vals.len());
    if n == 0 || target_grid_ms.is_empty() {
        return vec![f64::NAN; target_grid_ms.len()];
    }
    let mut out = Vec::with_capacity(target_grid_ms.len());
    let mut idx = 0usize;
    for &t in target_grid_ms {
        while idx < n && src_ts[idx] < t {
            idx += 1;
        }
        if idx < n && src_ts[idx] == t {
            // If multiple samples exist at the same ms, take the last one
            let mut val = src_vals[idx];
            while idx + 1 < n && src_ts[idx + 1] == t {
                idx += 1;
                if src_vals[idx].is_finite() {
                    val = src_vals[idx];
                }
            }
            out.push(val);
        } else {
            out.push(f64::NAN);
        }
    }
    out
}

/// Resolve an annotation name or ID against the router's registry (case-insensitive exact match
/// first, then substring match) and return `(annotation_name, start_utc_ms..=end_utc_ms)`.
pub fn resolve_annotation_time_range(
    router: &VirtualTimelineRouter,
    event_query: &str,
) -> Result<(String, RangeInclusive<i64>), ExportError> {
    let query_trimmed = event_query.trim();
    if query_trimmed.is_empty() {
        return Err(ExportError::UnknownAnnotation(String::new()));
    }
    let query_lower = query_trimmed.to_ascii_lowercase();
    let days = router
        .registry()
        .list_track_days()
        .map_err(RouterError::from)?;
    let mut all_annotations = Vec::new();
    for day in &days {
        let mut day_anns = router
            .registry()
            .list_annotations_for_day(&day.id, false)
            .map_err(RouterError::from)?;
        all_annotations.append(&mut day_anns);
    }

    let matched = all_annotations
        .iter()
        .find(|a| {
            a.id.eq_ignore_ascii_case(query_trimmed) || a.name.eq_ignore_ascii_case(query_trimmed)
        })
        .or_else(|| {
            all_annotations
                .iter()
                .find(|a| a.name.to_ascii_lowercase().contains(&query_lower))
        })
        .ok_or_else(|| ExportError::UnknownAnnotation(query_trimmed.to_owned()))?;

    let start_ms = matched.start_utc_ms;
    let end_ms = matched.end_utc_ms.unwrap_or(start_ms);
    Ok((matched.name.clone(), start_ms..=end_ms.max(start_ms)))
}

/// Materialize and write a dataset to `output_path` in `format`.
pub fn export_dataset(
    router: &VirtualTimelineRouter,
    range: RangeInclusive<i64>,
    signal_patterns: &[String],
    sampling_mode: SamplingMode,
    format: ExportFormat,
    output_path: impl AsRef<Path>,
    database: Option<&Database>,
) -> Result<ExportSummary, ExportError> {
    let table = materialize_export_table(router, range, signal_patterns, sampling_mode, database)?;
    let out_path = output_path.as_ref().to_path_buf();
    if let Some(parent) = out_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }

    match format {
        ExportFormat::Parquet => write_parquet(&table, &out_path)?,
        ExportFormat::Csv => write_csv(&table, &out_path)?,
        ExportFormat::MatlabMat => write_matlab_mat(&table, &out_path)?,
    }

    let start_utc_ms = table.timestamps_utc_ms.first().copied().unwrap_or(0);
    let end_utc_ms = table.timestamps_utc_ms.last().copied().unwrap_or(0);
    let signals = table
        .columns
        .iter()
        .map(|c| (c.signal_id.clone(), c.unit.clone()))
        .collect();

    Ok(ExportSummary {
        output_path: out_path,
        format,
        start_utc_ms,
        end_utc_ms,
        row_count: table.row_count(),
        signal_count: table.column_count(),
        signals,
    })
}

/// Write an [`ExportTable`] to a ZSTD-compressed Apache Parquet file, embedding `"unit"` and
/// `"label"` metadata on each Arrow [`Field`].
pub fn write_parquet(table: &ExportTable, path: &Path) -> Result<(), ExportError> {
    let mut fields = Vec::with_capacity(table.columns.len() + 2);

    let mut utc_meta = HashMap::new();
    utc_meta.insert("unit".to_owned(), "ms".to_owned());
    utc_meta.insert("label".to_owned(), "UTC Timestamp (ms)".to_owned());
    fields.push(Field::new("utc_ms", DataType::Int64, false).with_metadata(utc_meta));

    let mut elapsed_meta = HashMap::new();
    elapsed_meta.insert("unit".to_owned(), "s".to_owned());
    elapsed_meta.insert("label".to_owned(), "Elapsed Time (s)".to_owned());
    fields.push(Field::new("elapsed_s", DataType::Float64, false).with_metadata(elapsed_meta));

    for col in &table.columns {
        let mut meta = HashMap::new();
        meta.insert("unit".to_owned(), col.unit.clone());
        meta.insert("label".to_owned(), col.label.clone());
        fields.push(Field::new(&col.signal_id, DataType::Float64, true).with_metadata(meta));
    }

    let schema = Arc::new(Schema::new(fields));
    let mut arrays: Vec<ArrayRef> = Vec::with_capacity(table.columns.len() + 2);
    arrays.push(Arc::new(Int64Array::from(table.timestamps_utc_ms.clone())));
    arrays.push(Arc::new(Float64Array::from(table.elapsed_s.clone())));

    for col in &table.columns {
        let opt_vals: Vec<Option<f64>> = col
            .values
            .iter()
            .map(|&v| if v.is_finite() { Some(v) } else { None })
            .collect();
        arrays.push(Arc::new(Float64Array::from(opt_vals)));
    }

    let batch = RecordBatch::try_new(schema.clone(), arrays)?;
    let file = fs::File::create(path)?;
    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::default()))
        .build();
    let mut writer = ArrowWriter::try_new(file, schema, Some(props))?;
    writer.write(&batch)?;
    writer.close()?;
    Ok(())
}

/// Write an [`ExportTable`] to an RFC-4180 CSV file (`utc_ms,elapsed_s,<signal_id_1>,...`).
/// Non-finite (`NaN`) cells are emitted as empty cells so Python/MATLAB parse numeric columns cleanly.
pub fn write_csv(table: &ExportTable, path: &Path) -> Result<(), ExportError> {
    let file = fs::File::create(path)?;
    let mut out = BufWriter::new(file);

    write!(out, "utc_ms,elapsed_s")?;
    for col in &table.columns {
        write!(out, ",")?;
        write_csv_escaped(&mut out, &col.signal_id)?;
    }
    writeln!(out)?;

    let row_count = table.row_count();
    for row_idx in 0..row_count {
        write!(
            out,
            "{},{:.6}",
            table.timestamps_utc_ms[row_idx], table.elapsed_s[row_idx]
        )?;
        for col in &table.columns {
            let val = col.values[row_idx];
            if val.is_finite() {
                write!(out, ",{val}")?;
            } else {
                write!(out, ",")?;
            }
        }
        writeln!(out)?;
    }
    out.flush()?;
    Ok(())
}

fn write_csv_escaped<W: Write>(out: &mut W, text: &str) -> io::Result<()> {
    if text.contains([',', '"', '\n', '\r']) {
        write!(out, "\"{}\"", text.replace('"', "\"\""))
    } else {
        write!(out, "{text}")
    }
}

// ============================================================================
// Pure-Rust MATLAB Level 5 MAT-File (.mat) Writer & Reader
// ============================================================================

const MAT_L5_HEADER_LEN: usize = 128;
const MI_INT8: u32 = 1;
const MI_INT32: u32 = 5;
const MI_UINT32: u32 = 6;
const MI_DOUBLE: u32 = 9;
const MI_MATRIX: u32 = 14;
const MX_DOUBLE_CLASS: u32 = 6;

/// Sanitize a canonical `msg_name.sig_name` into a valid MATLAB Level 5 variable identifier
/// (`[a-zA-Z][a-zA-Z0-9_]*`, max 63 characters).
pub fn sanitize_matlab_var_name(signal_id: &str) -> String {
    let mut out = String::with_capacity(signal_id.len().min(63));
    for ch in signal_id.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() || out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert(0, 'v');
    }
    if out.len() > 63 {
        out.truncate(63);
    }
    out
}

/// Write an [`ExportTable`] to a native MATLAB Level 5 `.mat` file.
///
/// Writes `utc_ms` (`N x 1 double`), `elapsed_s` (`N x 1 double`), and one `N x 1 double`
/// vector for each signal column (with deduplicated MATLAB-safe variable names).
pub fn write_matlab_mat(table: &ExportTable, path: &Path) -> Result<(), ExportError> {
    let file = fs::File::create(path)?;
    let mut out = BufWriter::new(file);

    // 128-byte Level 5 header: 116-byte text + 8-byte subsys offset (zeros) + version (0x0100) + endian ("IM")
    let mut header = [b' '; MAT_L5_HEADER_LEN];
    let desc = b"MATLAB 5.0 MAT-file, Platform: PER DAQ (daqcore), Created by Option A v2";
    header[..desc.len()].copy_from_slice(desc);
    for b in &mut header[116..124] {
        *b = 0;
    }
    header[124..126].copy_from_slice(&0x0100u16.to_le_bytes());
    header[126..128].copy_from_slice(b"IM");
    out.write_all(&header)?;

    let utc_f64: Vec<f64> = table.timestamps_utc_ms.iter().map(|&t| t as f64).collect();
    write_mat_double_column(&mut out, "utc_ms", &utc_f64)?;
    write_mat_double_column(&mut out, "elapsed_s", &table.elapsed_s)?;

    let mut used_names: BTreeSet<String> = BTreeSet::new();
    used_names.insert("utc_ms".to_owned());
    used_names.insert("elapsed_s".to_owned());

    for col in &table.columns {
        let base_name = sanitize_matlab_var_name(&col.signal_id);
        let mut var_name = base_name.clone();
        let mut suffix = 2usize;
        while used_names.contains(&var_name) {
            let suffix_str = format!("_{suffix}");
            let max_base = 63usize.saturating_sub(suffix_str.len());
            let prefix: String = base_name.chars().take(max_base).collect();
            var_name = format!("{prefix}{suffix_str}");
            suffix += 1;
        }
        used_names.insert(var_name.clone());
        write_mat_double_column(&mut out, &var_name, &col.values)?;
    }

    out.flush()?;
    Ok(())
}

fn write_mat_double_column<W: Write>(
    out: &mut W,
    var_name: &str,
    values: &[f64],
) -> io::Result<()> {
    let mut matrix_body: Vec<u8> = Vec::with_capacity(64 + values.len() * 8);

    // 1. Array Flags subelement: tag (MI_UINT32, 8) + [MX_DOUBLE_CLASS, 0]
    matrix_body.extend_from_slice(&MI_UINT32.to_le_bytes());
    matrix_body.extend_from_slice(&8u32.to_le_bytes());
    matrix_body.extend_from_slice(&MX_DOUBLE_CLASS.to_le_bytes());
    matrix_body.extend_from_slice(&0u32.to_le_bytes());

    // 2. Dimensions Array subelement: tag (MI_INT32, 8) + [rows = N, cols = 1]
    let rows = i32::try_from(values.len()).unwrap_or(i32::MAX);
    matrix_body.extend_from_slice(&MI_INT32.to_le_bytes());
    matrix_body.extend_from_slice(&8u32.to_le_bytes());
    matrix_body.extend_from_slice(&rows.to_le_bytes());
    matrix_body.extend_from_slice(&1i32.to_le_bytes());

    // 3. Array Name subelement: tag (MI_INT8, name_len) + padded to 8-byte boundary
    let name_bytes = var_name.as_bytes();
    let name_len = u32::try_from(name_bytes.len()).unwrap_or(0);
    matrix_body.extend_from_slice(&MI_INT8.to_le_bytes());
    matrix_body.extend_from_slice(&name_len.to_le_bytes());
    matrix_body.extend_from_slice(name_bytes);
    let name_pad = (8 - (name_bytes.len() % 8)) % 8;
    matrix_body.resize(matrix_body.len() + name_pad, 0);

    // 4. Real part (`pr`) subelement: tag (MI_DOUBLE, values.len() * 8) + f64 little-endian bytes
    let data_bytes_len = u32::try_from(values.len().saturating_mul(8)).unwrap_or(0);
    matrix_body.extend_from_slice(&MI_DOUBLE.to_le_bytes());
    matrix_body.extend_from_slice(&data_bytes_len.to_le_bytes());
    for &v in values {
        matrix_body.extend_from_slice(&v.to_le_bytes());
    }

    // Outer miMATRIX tag + body
    let total_body_len = u32::try_from(matrix_body.len()).unwrap_or(0);
    out.write_all(&MI_MATRIX.to_le_bytes())?;
    out.write_all(&total_body_len.to_le_bytes())?;
    out.write_all(&matrix_body)?;
    Ok(())
}

/// Read numeric double vectors from a Level 5 `.mat` file written by [`write_matlab_mat`].
pub fn read_matlab_mat_vars(path: &Path) -> Result<IndexMap<String, Vec<f64>>, ExportError> {
    let bytes = fs::read(path)?;
    if bytes.len() < MAT_L5_HEADER_LEN || &bytes[126..128] != b"IM" {
        return Err(ExportError::ConfigParse(
            "invalid Level 5 MAT-file header".to_owned(),
        ));
    }

    let mut pos = MAT_L5_HEADER_LEN;
    let mut vars = IndexMap::new();

    while pos + 8 <= bytes.len() {
        let tag_type =
            u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]);
        let tag_len = u32::from_le_bytes([
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]) as usize;
        pos += 8;
        if pos + tag_len > bytes.len() {
            break;
        }
        if tag_type == MI_MATRIX {
            let elem = &bytes[pos..pos + tag_len];
            if let Some((name, vals)) = parse_mat_matrix_element(elem) {
                vars.insert(name, vals);
            }
        }
        let pad = (8 - (tag_len % 8)) % 8;
        pos = pos.saturating_add(tag_len).saturating_add(pad);
    }

    Ok(vars)
}

fn parse_mat_matrix_element(elem: &[u8]) -> Option<(String, Vec<f64>)> {
    // Skip Array Flags (16 bytes) + Dimensions (16 bytes) = 32 bytes
    if elem.len() < 40 {
        return None;
    }
    let mut idx = 32usize;

    // Name subelement
    let name_type = u32::from_le_bytes(elem.get(idx..idx + 4)?.try_into().ok()?);
    let name_len = u32::from_le_bytes(elem.get(idx + 4..idx + 8)?.try_into().ok()?) as usize;
    idx += 8;
    if name_type != MI_INT8 || idx + name_len > elem.len() {
        return None;
    }
    let name = String::from_utf8_lossy(&elem[idx..idx + name_len]).into_owned();
    let name_pad = (8 - (name_len % 8)) % 8;
    idx = idx.saturating_add(name_len).saturating_add(name_pad);

    // Data subelement
    let data_type = u32::from_le_bytes(elem.get(idx..idx + 4)?.try_into().ok()?);
    let data_len = u32::from_le_bytes(elem.get(idx + 4..idx + 8)?.try_into().ok()?) as usize;
    idx += 8;
    if data_type != MI_DOUBLE || idx + data_len > elem.len() || !data_len.is_multiple_of(8) {
        return None;
    }

    let count = data_len / 8;
    let mut values = Vec::with_capacity(count);
    for i in 0..count {
        let start = idx + i * 8;
        let v = f64::from_le_bytes(elem.get(start..start + 8)?.try_into().ok()?);
        values.push(v);
    }
    Some((name, values))
}

/// Read back an exported Parquet file's column headers, units (from Arrow field metadata),
/// and row count for verification.
pub fn read_exported_parquet_info(
    path: &Path,
) -> Result<(usize, Vec<(String, String)>), ExportError> {
    let file = fs::File::open(path)?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
    let schema = builder.schema().clone();
    let mut columns = Vec::with_capacity(schema.fields().len());
    for field in schema.fields() {
        let unit = field.metadata().get("unit").cloned().unwrap_or_default();
        columns.push((field.name().clone(), unit));
    }
    let reader = builder.build()?;
    let mut total_rows = 0usize;
    for batch in reader {
        let b = batch?;
        total_rows += b.num_rows();
    }
    Ok((total_rows, columns))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        frame::{FrameKind, ParsedFrame},
        time::Time,
    };

    #[test]
    fn builtin_export_presets_load_and_validate() {
        let registry = ExportPresetRegistry::with_builtin();
        assert!(registry.presets().len() >= 5);

        let vd = registry.get("vd").expect("vd preset should exist");
        assert_eq!(vd.name, "Vehicle Dynamics (VD)");
        assert_eq!(vd.default_sampling, SamplingMode::UniformGridHz(100.0));
        assert!(vd.signals.iter().any(|s| s == "IMU_acceleration.*"));
        assert!(vd.signals.iter().any(|s| s == "vd.*"));

        let aero = registry.get("aero").expect("aero preset should exist");
        assert_eq!(aero.default_sampling, SamplingMode::UniformGridHz(50.0));

        let pt = registry
            .get("powertrain_thermals")
            .expect("powertrain_thermals preset should exist");
        assert_eq!(pt.default_sampling, SamplingMode::UniformGridHz(50.0));

        let raw_all = registry
            .get("raw_all")
            .expect("raw_all preset should exist");
        assert_eq!(raw_all.default_sampling, SamplingMode::SparseRaw);
    }

    #[test]
    fn export_parquet_csv_and_mat_round_trip_with_per26_data() {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let dbc_path = manifest_dir.join("../../superdbc/superdbc_per26.json");
        let log_path =
            manifest_dir.join("../../PER26/2026-09-06/2_regen/raw/log-2026-09-06--20-21-44.log");
        if !dbc_path.is_file() || !log_path.is_file() {
            return;
        }

        let db = Database::load(&dbc_path).expect("load PER26 database");
        let tmp_raw = tempfile::tempdir().expect("temp raw dir");
        fs::copy(
            &log_path,
            tmp_raw.path().join("log-2026-09-06--20-21-44.log"),
        )
        .expect("copy PER26 log");

        let tmp_store = tempfile::tempdir().expect("temp store dir");
        let summary = crate::log_parse::import_logs_to_store(tmp_raw.path(), tmp_store.path(), &db)
            .expect("import PER26 log");
        assert!(!summary.manifests.is_empty());

        let m0 = &summary.manifests[0];
        let mut router = VirtualTimelineRouter::open_store(
            tmp_store.path(),
            Time::from_unix_millis(m0.end_utc_ms),
            30.0,
        )
        .expect("open store");

        // Export a 10-second window using the "vd" preset to Parquet, CSV, and MATLAB .mat
        let window_start = m0.start_utc_ms;
        let window_end = (m0.start_utc_ms + 10_000).min(m0.end_utc_ms);
        let range = window_start..=window_end;

        let presets = ExportPresetRegistry::with_builtin();
        let vd_preset = presets.get("vd").expect("vd preset");

        let tmp_out = tempfile::tempdir().expect("temp out dir");
        let pq_path = tmp_out.path().join("vd_10s.parquet");
        let csv_path = tmp_out.path().join("vd_10s.csv");
        let mat_path = tmp_out.path().join("vd_10s.mat");

        let pq_summary = export_dataset(
            &router,
            range.clone(),
            &vd_preset.signals,
            vd_preset.default_sampling,
            ExportFormat::Parquet,
            &pq_path,
            Some(&db),
        )
        .expect("export vd parquet");
        assert_eq!(pq_summary.row_count, 1001); // 10.0s at 100 Hz inclusive
        assert!(pq_summary.signal_count >= 10);

        let (pq_rows, pq_cols) =
            read_exported_parquet_info(&pq_path).expect("read exported parquet info");
        assert_eq!(pq_rows, 1001);
        assert_eq!(pq_cols[0], ("utc_ms".to_owned(), "ms".to_owned()));
        assert_eq!(pq_cols[1], ("elapsed_s".to_owned(), "s".to_owned()));
        assert!(
            pq_cols
                .iter()
                .any(|(name, unit)| name == "IMU_acceleration.X_axis" && unit == "g")
        );
        assert!(
            pq_cols
                .iter()
                .any(|(name, unit)| name == "battery.power_w" && unit == "W")
        );

        let csv_summary = export_dataset(
            &router,
            range.clone(),
            &vd_preset.signals,
            vd_preset.default_sampling,
            ExportFormat::Csv,
            &csv_path,
            Some(&db),
        )
        .expect("export vd csv");
        assert_eq!(csv_summary.row_count, 1001);
        let csv_text = fs::read_to_string(&csv_path).expect("read csv");
        let mut lines = csv_text.lines();
        let header = lines.next().expect("csv header");
        assert!(header.starts_with("utc_ms,elapsed_s,"));
        assert!(header.contains("IMU_acceleration.X_axis"));
        assert!(header.contains("battery.power_w"));
        assert_eq!(lines.count(), 1001);

        let mat_summary = export_dataset(
            &router,
            range,
            &vd_preset.signals,
            vd_preset.default_sampling,
            ExportFormat::MatlabMat,
            &mat_path,
            Some(&db),
        )
        .expect("export vd mat");
        assert_eq!(mat_summary.row_count, 1001);
        let mat_vars = read_matlab_mat_vars(&mat_path).expect("read mat vars");
        assert_eq!(mat_vars.get("utc_ms").map(Vec::len), Some(1001));
        assert_eq!(mat_vars.get("elapsed_s").map(Vec::len), Some(1001));
        assert_eq!(
            mat_vars.get("IMU_acceleration_X_axis").map(Vec::len),
            Some(1001)
        );
        assert_eq!(mat_vars.get("battery_power_w").map(Vec::len), Some(1001));

        // Verify annotation-based lookup on both auto-generated annotations and "Driving Stint 1"
        if let Some(first_auto) = m0.annotations.first() {
            let (auto_name, auto_range) =
                resolve_annotation_time_range(&router, &first_auto.name).expect("resolve auto ann");
            assert_eq!(auto_name, first_auto.name);
            assert!(!auto_range.is_empty());
        }

        let track_day_id = router
            .timeline()
            .active_track_day_id()
            .unwrap_or("2026-09-06")
            .to_owned();
        router
            .upsert_annotation(&crate::storage::Annotation {
                id: "stint_1".to_owned(),
                track_day_id,
                name: "Driving Stint 1".to_owned(),
                category: "driving_stint".to_owned(),
                color: "#2ECC71".to_owned(),
                start_utc_ms: window_start,
                end_utc_ms: Some(window_end),
                author: "engineer".to_owned(),
                source: "manual".to_owned(),
                notes: String::new(),
                updated_at_ms: window_end,
                deleted: false,
            })
            .expect("upsert Driving Stint 1");
        let (ann_name, ann_range) =
            resolve_annotation_time_range(&router, "Driving Stint 1").expect("resolve stint 1");
        assert_eq!(ann_name, "Driving Stint 1");
        assert_eq!(ann_range, window_start..=window_end);
    }

    #[test]
    fn sparse_raw_export_preserves_exact_timestamps_on_live_data() {
        let t0 = Time::from_unix_millis(1_794_500_000_000);
        let mut router = VirtualTimelineRouter::live(t0, 30.0).expect("live router");
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let dbc_path = manifest_dir.join("../../superdbc/superdbc_per26.json");
        if !dbc_path.is_file() {
            return;
        }
        let db = Database::load(&dbc_path).expect("load PER26 db");
        let msg = db
            .messages()
            .find(|m| m.name() == "pack_stats")
            .expect("pack_stats message");
        let key = msg.key();

        for i in 0..5 {
            let raw = vec![0x10, 0x27, 0x30, 0x00, 0x50, 0x00, 0x00, 0x00];
            let decoded = db.decode(key.bus_id, key.identity, &raw).ok();
            router.ingest_frame(ParsedFrame {
                timestamp: t0.offset(i * 20),
                bus_id: key.bus_id,
                identity: key.identity,
                kind: FrameKind::Data,
                dlc: 8,
                raw_bytes: raw,
                decoded,
            });
        }

        let table = materialize_export_table(
            &router,
            t0.unix_millis()..=(t0.unix_millis() + 100),
            &["pack_stats.*".to_owned(), "battery.power_w".to_owned()],
            SamplingMode::SparseRaw,
            Some(&db),
        )
        .expect("materialize sparse table");
        assert_eq!(table.row_count(), 5);
        assert!(table.column("pack_stats.vbatt_adc").is_some());
        assert!(table.column("battery.power_w").is_some());
    }
}
