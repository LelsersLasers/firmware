use crate::{
    analysis::{
        AnalysisError, DerivedSignalDef, TargetGrid, canonicalize_signal_ref,
        resample_continuous_linear, resample_discrete_hold, series_to_timestamp_values,
    },
    router::{Resolution, RouterError, SignalSeries, VirtualTimelineRouter},
};
use globset::GlobBuilder;
use indexmap::IndexMap;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    io::{self, BufWriter, Write},
    ops::RangeInclusive,
    path::{Path, PathBuf},
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
    Format(String),
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
            Self::Format(msg) => write!(f, "format error during export: {msg}"),
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

/// Serializable request payload for dataset export (used by `ViewerPlatform::execute_dataset_export`
/// and `POST /api/v1/export` on `daqcloud`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DatasetExportRequest {
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    pub signal_patterns: Vec<String>,
    pub sampling_mode: SamplingMode,
    pub format: ExportFormat,
    #[serde(default)]
    pub output_path: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub custom_formulas: Vec<DerivedSignalDef>,
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

/// Summary returned after writing an [`ExportTable`] to disk or browser download.
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
) -> IndexMap<String, ExportSignalMeta> {
    discover_available_signals_with_engine(router, router.formula_engine(), range)
}

fn discover_available_signals_with_engine(
    router: &VirtualTimelineRouter,
    formula_engine: & crate::analysis::FormulaEngine,
    range: RangeInclusive<i64>,
) -> IndexMap<String, ExportSignalMeta> {
    let mut catalog: IndexMap<String, ExportSignalMeta> = IndexMap::new();

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

    for compiled in formula_engine.definitions() {
        let raw_deps_available =
            if let Ok((_, leaf_raw)) = formula_engine.execution_plan_for_expr(&compiled.ast) {
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

fn match_patterns_against_catalog(
    signal_patterns: &[String],
    catalog: &IndexMap<String, ExportSignalMeta>,
    require_active: bool,
) -> Result<IndexMap<String, ExportSignalMeta>, ExportError> {
    let mut selected: IndexMap<String, ExportSignalMeta> = IndexMap::new();
    for raw_pattern in signal_patterns {
        let pattern = canonicalize_signal_ref(raw_pattern);
        if pattern.is_empty() {
            continue;
        }
        let is_wildcard = pattern.contains('*') || pattern.contains('?');
        if !is_wildcard {
            if let Some(meta) = catalog.get(&pattern)
                && (!require_active || meta.has_samples_in_range)
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
            if glob.is_match(&meta.signal_id) && (!require_active || meta.has_samples_in_range) {
                selected.insert(meta.signal_id.clone(), meta.clone());
            }
        }
    }
    Ok(selected)
}

/// Expand a list of signal patterns against the signals available in `router` over `range`.
pub fn expand_signal_patterns(
    signal_patterns: &[String],
    router: &VirtualTimelineRouter,
    range: RangeInclusive<i64>,
) -> Result<Vec<ExportSignalMeta>, ExportError> {
    expand_signal_patterns_with_engine(signal_patterns, router, router.formula_engine(), range)
}

fn expand_signal_patterns_with_engine(
    signal_patterns: &[String],
    router: &VirtualTimelineRouter,
    formula_engine: &crate::analysis::FormulaEngine,
    range: RangeInclusive<i64>,
) -> Result<Vec<ExportSignalMeta>, ExportError> {
    let catalog = discover_available_signals_with_engine(router, formula_engine, range);
    let any_active_in_range = catalog.values().any(|m| m.has_samples_in_range);
    let mut selected =
        match_patterns_against_catalog(signal_patterns, &catalog, any_active_in_range)?;

    if selected.is_empty() && any_active_in_range {
        selected = match_patterns_against_catalog(signal_patterns, &catalog, false)?;
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
) -> Result<ExportTable, ExportError> {
    materialize_export_table_with_formulas(router, range, signal_patterns, sampling_mode, &[])
}

/// Materialize a synchronized [`ExportTable`] with additional ad-hoc `custom_formulas` merged into
/// the router's [`FormulaEngine`].
pub fn materialize_export_table_with_formulas(
    router: &VirtualTimelineRouter,
    range: RangeInclusive<i64>,
    signal_patterns: &[String],
    sampling_mode: SamplingMode,
    custom_formulas: &[DerivedSignalDef],
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

    let mut owned_engine = None;
    let formula_engine = if custom_formulas.is_empty() {
        router.formula_engine()
    } else {
        let mut eng = router.formula_engine().clone();
        eng.merge_defs(custom_formulas)?;
        owned_engine.insert(eng)
    };

    let metas = expand_signal_patterns_with_engine(
        signal_patterns,
        router,
        formula_engine,
        range.clone(),
    )?;

    let mut raw_ids_needed: BTreeSet<String> = BTreeSet::new();
    for meta in &metas {
        if meta.is_derived {
            if let Some(compiled) = formula_engine.get(&meta.signal_id) {
                let (_, leaves) = formula_engine.execution_plan_for_expr(&compiled.ast)?;
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

    let max_staleness_ms = formula_engine.max_staleness_ms();
    let explicit_grid = TargetGrid::Explicit(timestamps_utc_ms.clone());
    let mut columns = Vec::with_capacity(metas.len());

    for meta in metas {
        let values = if meta.is_derived {
            let series = formula_engine.evaluate_channel_with(
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

/// Resolve an annotation name or ID against the router's annotations and return `(annotation_name, start_utc_ms..=end_utc_ms)`.
///
/// Prefers annotations belonging to the router's currently selected `active_track_day_id` before
/// falling back to annotations on other track days.
pub fn resolve_annotation_time_range(
    router: &VirtualTimelineRouter,
    event_query: &str,
) -> Result<(String, RangeInclusive<i64>), ExportError> {
    let query_trimmed = event_query.trim();
    if query_trimmed.is_empty() {
        return Err(ExportError::UnknownAnnotation(String::new()));
    }
    let query_lower = query_trimmed.to_ascii_lowercase();
    let all_annotations = router.query_annotations_in_range(i64::MIN..=i64::MAX)?;
    let active_day_id = router.timeline().active_track_day_id();

    let find_in = |filter_active_day: bool| -> Option<&crate::log_parse::auto_annotate::Annotation> {
        let day_pred = |a: &&crate::log_parse::auto_annotate::Annotation| -> bool {
            if filter_active_day {
                active_day_id.is_some_and(|day_id| a.track_day_id == day_id)
            } else {
                true
            }
        };
        all_annotations
            .iter()
            .filter(day_pred)
            .find(|a| {
                a.id.eq_ignore_ascii_case(query_trimmed)
                    || a.name.eq_ignore_ascii_case(query_trimmed)
            })
            .or_else(|| {
                all_annotations
                    .iter()
                    .filter(day_pred)
                    .find(|a| a.name.to_ascii_lowercase().contains(&query_lower))
            })
    };

    let matched = find_in(true)
        .or_else(|| find_in(false))
        .ok_or_else(|| ExportError::UnknownAnnotation(query_trimmed.to_owned()))?;

    let start_ms = matched.start_utc_ms;
    let end_ms = matched.end_utc_ms.unwrap_or(start_ms);
    Ok((matched.name.clone(), start_ms..=end_ms.max(start_ms)))
}

/// Write an [`ExportTable`] to an in-memory RFC-4180 CSV byte vector.
pub fn write_csv_bytes(table: &ExportTable) -> Result<Vec<u8>, ExportError> {
    let mut out = Vec::with_capacity(table.row_count() * (table.column_count() * 10 + 24));
    write_csv_to_writer(table, &mut out)?;
    Ok(out)
}

/// Write an [`ExportTable`] to an RFC-4180 CSV file (`utc_ms,elapsed_s,<signal_id_1>,...`).
pub fn write_csv(table: &ExportTable, path: &Path) -> Result<(), ExportError> {
    let file = fs::File::create(path)?;
    let mut out = BufWriter::new(file);
    write_csv_to_writer(table, &mut out)
}

pub fn write_csv_to_writer<W: Write>(table: &ExportTable, out: &mut W) -> Result<(), ExportError> {
    write!(out, "utc_ms,elapsed_s")?;
    for col in &table.columns {
        write!(out, ",")?;
        write_csv_escaped(out, &col.signal_id)?;
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

/// Sanitize a canonical `msg_name.sig_name` into a valid MATLAB Level 5 variable identifier.
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

/// Write an [`ExportTable`] to an in-memory MATLAB Level 5 `.mat` byte vector.
pub fn write_matlab_mat_bytes(table: &ExportTable) -> Result<Vec<u8>, ExportError> {
    let mut out = Vec::with_capacity(256 + table.row_count() * (table.column_count() + 2) * 8);
    write_matlab_mat_to_writer(table, &mut out)?;
    Ok(out)
}

/// Write an [`ExportTable`] to a native MATLAB Level 5 `.mat` file.
pub fn write_matlab_mat(table: &ExportTable, path: &Path) -> Result<(), ExportError> {
    let file = fs::File::create(path)?;
    let mut out = BufWriter::new(file);
    write_matlab_mat_to_writer(table, &mut out)
}

pub fn write_matlab_mat_to_writer<W: Write>(
    table: &ExportTable,
    out: &mut W,
) -> Result<(), ExportError> {
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
    write_mat_double_column(out, "utc_ms", &utc_f64)?;
    write_mat_double_column(out, "elapsed_s", &table.elapsed_s)?;

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
        write_mat_double_column(out, &var_name, &col.values)?;
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

    matrix_body.extend_from_slice(&MI_UINT32.to_le_bytes());
    matrix_body.extend_from_slice(&8u32.to_le_bytes());
    matrix_body.extend_from_slice(&MX_DOUBLE_CLASS.to_le_bytes());
    matrix_body.extend_from_slice(&0u32.to_le_bytes());

    let rows = i32::try_from(values.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "MAT-file column length exceeds i32::MAX",
        )
    })?;
    matrix_body.extend_from_slice(&MI_INT32.to_le_bytes());
    matrix_body.extend_from_slice(&8u32.to_le_bytes());
    matrix_body.extend_from_slice(&rows.to_le_bytes());
    matrix_body.extend_from_slice(&1i32.to_le_bytes());

    let name_bytes = var_name.as_bytes();
    let name_len = u32::try_from(name_bytes.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "MAT-file variable name exceeds u32::MAX",
        )
    })?;
    matrix_body.extend_from_slice(&MI_INT8.to_le_bytes());
    matrix_body.extend_from_slice(&name_len.to_le_bytes());
    matrix_body.extend_from_slice(name_bytes);
    let name_pad = (8 - (name_bytes.len() % 8)) % 8;
    matrix_body.resize(matrix_body.len() + name_pad, 0);

    let data_bytes_len = u32::try_from(values.len().saturating_mul(8)).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "MAT-file column byte size exceeds u32::MAX",
        )
    })?;
    matrix_body.extend_from_slice(&MI_DOUBLE.to_le_bytes());
    matrix_body.extend_from_slice(&data_bytes_len.to_le_bytes());
    for &v in values {
        matrix_body.extend_from_slice(&v.to_le_bytes());
    }

    let total_body_len = u32::try_from(matrix_body.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "MAT-file matrix element exceeds u32::MAX",
        )
    })?;
    out.write_all(&MI_MATRIX.to_le_bytes())?;
    out.write_all(&total_body_len.to_le_bytes())?;
    out.write_all(&matrix_body)?;
    Ok(())
}

/// Read numeric double vectors from a Level 5 `.mat` file written by [`write_matlab_mat`].
pub fn read_matlab_mat_vars(path: &Path) -> Result<IndexMap<String, Vec<f64>>, ExportError> {
    let bytes = fs::read(path)?;
    read_matlab_mat_vars_bytes(&bytes)
}

/// Read numeric double vectors from an in-memory Level 5 `.mat` byte slice.
pub fn read_matlab_mat_vars_bytes(bytes: &[u8]) -> Result<IndexMap<String, Vec<f64>>, ExportError> {
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
    if elem.len() < 40 {
        return None;
    }
    let mut idx = 32usize;

    let name_type = u32::from_le_bytes(elem.get(idx..idx + 4)?.try_into().ok()?);
    let name_len = u32::from_le_bytes(elem.get(idx + 4..idx + 8)?.try_into().ok()?) as usize;
    idx += 8;
    if name_type != MI_INT8 || idx + name_len > elem.len() {
        return None;
    }
    let name = String::from_utf8_lossy(&elem[idx..idx + name_len]).into_owned();
    let name_pad = (8 - (name_len % 8)) % 8;
    idx = idx.saturating_add(name_len).saturating_add(name_pad);

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
