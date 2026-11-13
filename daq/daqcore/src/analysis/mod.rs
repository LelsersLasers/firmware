pub mod align;
pub mod distance;
pub mod expr;

pub use align::{
    AlignedXY, AlignedXYZ, TargetGrid, align_series_map, align_xy_pairs, align_xyz_triples,
    resample_continuous_linear, resample_discrete_hold, series_to_timestamp_values,
};
pub use distance::{
    DEFAULT_SPEED_INTEGRATION_GAP_MS, DistanceSeries, compute_distance_axis,
    compute_distance_axis_with_max_gap, integrate_velocity_samples, map_series_to_distance,
};
pub use expr::{
    AnalysisError, BinaryOp, CompiledDerivedSignal, DEFAULT_DERIVATIVE_WINDOW_MS,
    DEFAULT_MAX_STALENESS_MS, DerivedSignalDef, DerivedSignalsConfigFile, Expr, ScalarFn, SeriesFn,
    UnaryOp, canonicalize_signal_ref, compile_and_sort_derived_signals, eval_series_fn,
};

use crate::router::{
    BUCKET_1HZ_MS, BUCKET_10HZ_MS, EnvelopePoint, Resolution, SignalSeries, build_envelope_buckets,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, path,
};

/// Embedded JSON configuration from `daq/config/derived_signals.json`.
pub const BUILTIN_DERIVED_SIGNALS_JSON: &str =
    include_str!("../../../config/derived_signals.json");

/// Native vectorized formula and multi-rate alignment engine.
///
/// Evaluates both repo-tracked derived signals (`daq/config/derived_signals.json`) and
/// ad-hoc user formulas across raw CAN signals and nested derived channels.
#[derive(Debug, Clone)]
pub struct FormulaEngine {
    ordered: Vec<CompiledDerivedSignal>,
    index_by_id: BTreeMap<String, usize>,
    max_staleness_ms: i64,
}

impl Default for FormulaEngine {
    fn default() -> Self {
        Self::with_builtin_config()
    }
}

impl FormulaEngine {
    /// Create an empty `FormulaEngine` with no pre-registered derived channels.
    pub fn empty() -> Self {
        Self {
            ordered: Vec::new(),
            index_by_id: BTreeMap::new(),
            max_staleness_ms: DEFAULT_MAX_STALENESS_MS,
        }
    }

    /// Create a `FormulaEngine` pre-populated with `daq/config/derived_signals.json`.
    pub fn with_builtin_config() -> Self {
        Self::from_json_str(BUILTIN_DERIVED_SIGNALS_JSON)
            .expect("embedded daq/config/derived_signals.json must be valid")
    }

    /// Construct a `FormulaEngine` from a slice of [`DerivedSignalDef`]s.
    pub fn from_defs(defs: &[DerivedSignalDef]) -> Result<Self, AnalysisError> {
        let ordered = compile_and_sort_derived_signals(defs)?;
        let index_by_id = ordered
            .iter()
            .enumerate()
            .map(|(i, c)| (c.def.id.clone(), i))
            .collect();
        Ok(Self {
            ordered,
            index_by_id,
            max_staleness_ms: DEFAULT_MAX_STALENESS_MS,
        })
    }

    /// Construct a `FormulaEngine` from a JSON string (accepting either `{"derived_signals": [...]}`
    /// or a top-level JSON array `[...]`).
    pub fn from_json_str(json: &str) -> Result<Self, AnalysisError> {
        let defs = parse_derived_signals_json(json)?;
        Self::from_defs(&defs)
    }

    /// Load a `FormulaEngine` from a `.json` file on disk.
    pub fn load_json_file(path: impl AsRef<path::Path>) -> Result<Self, AnalysisError> {
        let content = fs::read_to_string(path.as_ref())
            .map_err(|e| AnalysisError::ConfigParse(e.to_string()))?;
        Self::from_json_str(&content)
    }

    pub fn max_staleness_ms(&self) -> i64 {
        self.max_staleness_ms
    }

    pub fn set_max_staleness_ms(&mut self, max_staleness_ms: i64) {
        self.max_staleness_ms = max_staleness_ms.max(1);
    }

    pub fn contains(&self, id: &str) -> bool {
        self.index_by_id.contains_key(id.trim())
    }

    pub fn get(&self, id: &str) -> Option<&CompiledDerivedSignal> {
        let idx = *self.index_by_id.get(id.trim())?;
        self.ordered.get(idx)
    }

    pub fn definitions(&self) -> &[CompiledDerivedSignal] {
        &self.ordered
    }

    pub fn unit_for(&self, id: &str) -> Option<&str> {
        self.get(id).map(|c| c.def.unit.as_str())
    }

    pub fn label_for(&self, id: &str) -> Option<&str> {
        self.get(id).map(|c| c.def.label.as_str())
    }

    /// Insert or update a derived channel definition, re-verifying topological order and
    /// rejecting cycles or syntax errors without mutating `self` on failure.
    pub fn upsert_formula(&mut self, def: DerivedSignalDef) -> Result<(), AnalysisError> {
        let target_id = def.id.trim();
        if target_id.is_empty() {
            return Err(AnalysisError::EmptyChannelId);
        }
        let mut current_defs: Vec<DerivedSignalDef> =
            self.ordered.iter().map(|c| c.def.clone()).collect();
        if let Some(existing) = current_defs.iter_mut().find(|d| d.id == target_id) {
            *existing = def;
        } else {
            current_defs.push(def);
        }
        let updated = Self::from_defs(&current_defs)?;
        self.ordered = updated.ordered;
        self.index_by_id = updated.index_by_id;
        Ok(())
    }

    /// Merge multiple derived channel definitions into `self`.
    pub fn merge_defs(&mut self, defs: &[DerivedSignalDef]) -> Result<(), AnalysisError> {
        let mut current_defs: Vec<DerivedSignalDef> =
            self.ordered.iter().map(|c| c.def.clone()).collect();
        for def in defs {
            let target_id = def.id.trim();
            if target_id.is_empty() {
                return Err(AnalysisError::EmptyChannelId);
            }
            if let Some(existing) = current_defs.iter_mut().find(|d| d.id == target_id) {
                *existing = def.clone();
            } else {
                current_defs.push(def.clone());
            }
        }
        let updated = Self::from_defs(&current_defs)?;
        self.ordered = updated.ordered;
        self.index_by_id = updated.index_by_id;
        Ok(())
    }

    /// Remove a derived channel by ID. Returns `true` if it was present.
    pub fn remove_formula(&mut self, id: &str) -> bool {
        let target_id = id.trim();
        if !self.index_by_id.contains_key(target_id) {
            return false;
        }
        let remaining: Vec<DerivedSignalDef> = self
            .ordered
            .iter()
            .filter(|c| c.def.id != target_id)
            .map(|c| c.def.clone())
            .collect();
        if let Ok(updated) = Self::from_defs(&remaining) {
            self.ordered = updated.ordered;
            self.index_by_id = updated.index_by_id;
            true
        } else {
            false
        }
    }

    /// Compute the topological execution plan for `expr`:
    /// returns `(ordered_derived_channels, leaf_raw_signal_ids)`.
    pub fn execution_plan_for_expr(
        &self,
        expr: &Expr,
    ) -> Result<(Vec<CompiledDerivedSignal>, Vec<String>), AnalysisError> {
        let mut needed_derived_ids = BTreeSet::new();
        let mut leaf_raw_ids = BTreeSet::new();
        let mut stack = expr.referenced_signals();

        while let Some(ref_id) = stack.pop() {
            if let Some(compiled) = self.get(&ref_id) {
                if needed_derived_ids.insert(compiled.def.id.clone()) {
                    for dep in &compiled.dependencies {
                        stack.push(dep.clone());
                    }
                }
            } else {
                leaf_raw_ids.insert(ref_id);
            }
        }

        // Preserve the precomputed topological order in `self.ordered`
        let ordered_derived: Vec<CompiledDerivedSignal> = self
            .ordered
            .iter()
            .filter(|c| needed_derived_ids.contains(&c.def.id))
            .cloned()
            .collect();

        Ok((ordered_derived, leaf_raw_ids.into_iter().collect()))
    }

    /// Evaluate a registered derived channel by `channel_id` using `fetch_raw` to retrieve
    /// any underlying raw CAN signals.
    pub fn evaluate_channel_with<F>(
        &self,
        channel_id: &str,
        grid: &TargetGrid,
        resolution: Resolution,
        fetch_raw: F,
    ) -> Result<SignalSeries, AnalysisError>
    where
        F: FnMut(&str) -> SignalSeries,
    {
        let canonical = canonicalize_signal_ref(channel_id);
        let compiled = self
            .get(channel_id)
            .or_else(|| self.get(&canonical))
            .ok_or_else(|| AnalysisError::UnknownChannel(channel_id.to_owned()))?;
        let ast = compiled.ast.clone();
        self.evaluate_expr_with(&ast, grid, resolution, fetch_raw)
    }

    /// Evaluate an arbitrary [`Expr`] AST using `fetch_raw` to retrieve underlying raw CAN signals.
    pub fn evaluate_expr_with<F>(
        &self,
        expr: &Expr,
        grid: &TargetGrid,
        resolution: Resolution,
        mut fetch_raw: F,
    ) -> Result<SignalSeries, AnalysisError>
    where
        F: FnMut(&str) -> SignalSeries,
    {
        let (derived_plan, raw_ids) = self.execution_plan_for_expr(expr)?;

        let mut raw_series_map: BTreeMap<String, SignalSeries> = BTreeMap::new();
        for raw_id in &raw_ids {
            raw_series_map.insert(raw_id.clone(), fetch_raw(raw_id));
        }

        if !raw_ids.is_empty() && raw_series_map.values().all(SignalSeries::is_empty) {
            return Ok(SignalSeries::empty_for(resolution));
        }

        let any_envelope = raw_series_map
            .values()
            .any(|s| matches!(s, SignalSeries::Envelope(_)));
        let all_envelope = !raw_series_map.is_empty()
            && raw_series_map
                .values()
                .filter(|s| !s.is_empty())
                .all(|s| matches!(s, SignalSeries::Envelope(_)));

        let effective_staleness_ms = if resolution == Resolution::Overview1Hz || any_envelope {
            self.max_staleness_ms.max(BUCKET_1HZ_MS * 2)
        } else if resolution == Resolution::Overview10Hz {
            self.max_staleness_ms.max(BUCKET_10HZ_MS * 2)
        } else {
            self.max_staleness_ms
        };

        let (timestamps_ms, mut mean_cols) =
            align_series_map(&raw_series_map, grid, effective_staleness_ms);
        if timestamps_ms.is_empty() {
            return Ok(SignalSeries::empty_for(resolution));
        }

        for step in &derived_plan {
            let vals = step
                .ast
                .eval_aligned(&timestamps_ms, &mean_cols, effective_staleness_ms)?;
            mean_cols.insert(step.def.id.clone(), vals);
        }
        let mean_out = expr.eval_aligned(&timestamps_ms, &mean_cols, effective_staleness_ms)?;

        // If an overview resolution was requested (or all inputs were fallback Envelopes),
        // produce a SignalSeries::Envelope preserving min/max bounds.
        if resolution != Resolution::FullRes || all_envelope {
            if all_envelope {
                let mut min_cols =
                    align_envelope_bound(&raw_series_map, &timestamps_ms, effective_staleness_ms, false);
                let mut max_cols =
                    align_envelope_bound(&raw_series_map, &timestamps_ms, effective_staleness_ms, true);

                for step in &derived_plan {
                    if let Ok(v) =
                        step.ast
                            .eval_aligned(&timestamps_ms, &min_cols, effective_staleness_ms)
                    {
                        min_cols.insert(step.def.id.clone(), v);
                    }
                    if let Ok(v) =
                        step.ast
                            .eval_aligned(&timestamps_ms, &max_cols, effective_staleness_ms)
                    {
                        max_cols.insert(step.def.id.clone(), v);
                    }
                }

                let low_out = expr
                    .eval_aligned(&timestamps_ms, &min_cols, effective_staleness_ms)
                    .unwrap_or_else(|_| mean_out.clone());
                let high_out = expr
                    .eval_aligned(&timestamps_ms, &max_cols, effective_staleness_ms)
                    .unwrap_or_else(|_| mean_out.clone());

                let mut buckets = Vec::with_capacity(timestamps_ms.len());
                for i in 0..timestamps_ms.len() {
                    let mean = mean_out[i];
                    if !mean.is_finite() {
                        continue;
                    }
                    let lo = if low_out[i].is_finite() {
                        low_out[i]
                    } else {
                        mean
                    };
                    let hi = if high_out[i].is_finite() {
                        high_out[i]
                    } else {
                        mean
                    };
                    let min = mean.min(lo).min(hi);
                    let max = mean.max(lo).max(hi);
                    buckets.push(EnvelopePoint {
                        utc_ms: timestamps_ms[i],
                        min,
                        max,
                        mean,
                    });
                }
                return Ok(SignalSeries::Envelope(buckets));
            }

            let bucket_width_ms = resolution.bucket_width_ms().unwrap_or(BUCKET_10HZ_MS);
            let buckets = build_envelope_buckets(
                timestamps_ms.into_iter().zip(mean_out),
                bucket_width_ms,
            );
            return Ok(SignalSeries::Envelope(buckets));
        }

        Ok(SignalSeries::Exact {
            timestamps_ms,
            values: mean_out,
        })
    }
}

fn align_envelope_bound(
    series_map: &BTreeMap<String, SignalSeries>,
    target_ts: &[i64],
    max_staleness_ms: i64,
    use_max: bool,
) -> BTreeMap<String, Vec<f64>> {
    let mut out = BTreeMap::new();
    for (name, series) in series_map {
        let (ts, vals) = match series {
            SignalSeries::Exact {
                timestamps_ms,
                values,
            } => (timestamps_ms.clone(), values.clone()),
            SignalSeries::Envelope(buckets) => {
                let ts: Vec<i64> = buckets.iter().map(|b| b.utc_ms).collect();
                let vals: Vec<f64> = buckets
                    .iter()
                    .map(|b| if use_max { b.max } else { b.min })
                    .collect();
                (ts, vals)
            }
        };
        let aligned = resample_continuous_linear(&ts, &vals, target_ts, max_staleness_ms);
        out.insert(name.clone(), aligned);
    }
    out
}

fn parse_derived_signals_json(json: &str) -> Result<Vec<DerivedSignalDef>, AnalysisError> {
    let trimmed = json.trim();
    if trimmed.starts_with('[') {
        serde_json::from_str::<Vec<DerivedSignalDef>>(trimmed)
            .map_err(|e| AnalysisError::ConfigParse(e.to_string()))
    } else {
        serde_json::from_str::<DerivedSignalsConfigFile>(trimmed)
            .map(|cfg| cfg.derived_signals)
            .map_err(|e| AnalysisError::ConfigParse(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_derived_signals_json_compiles_and_sorts_cleanly() {
        let engine = FormulaEngine::with_builtin_config();
        assert!(engine.contains("battery.voltage_v"));
        assert!(engine.contains("battery.power_w"));
        assert!(engine.contains("battery.power_kw"));
        assert!(engine.contains("battery.energy_wh"));
        assert!(engine.contains("vd.fl_damper_vel_mms"));
        assert!(engine.contains("vd.understeer_gradient"));
        assert!(engine.contains("thermals.max_igbt_temp_c"));
        assert_eq!(engine.unit_for("battery.power_w"), Some("W"));
    }

    /// Milestone 4 Verification (Roadmap Step 4.4):
    /// Multi-rate synthetic inputs (`100 Hz` voltage × `500 Hz` current with a 10-second gap in the middle)
    /// verifying exact numerical output and gap handling (`NaN` across the 10-second gap).
    #[test]
    fn formula_engine_evaluates_multirate_100hz_voltage_times_500hz_current_with_10s_gap() {
        let engine = FormulaEngine::with_builtin_config();

        // Stint 1: 0 ..= 100 ms
        // Stint 2: 10,100 ..= 10,200 ms (after a 10,000 ms gap)
        // `pack_stats.vbatt_adc` at 100 Hz (every 10 ms):
        //   Stint 1: constant raw ADC = 1000.0 / 0.14659808276 -> voltage = 1000.0 * 0.14659808276 = 146.59808276 V
        //   Wait: let's set vbatt_adc = 2000.0 in Stint 1 (V = 293.19616552 V) and 3000.0 in Stint 2 (V = 439.79424828 V)
        let mut v_ts = Vec::new();
        let mut v_vals = Vec::new();
        for step in 0..=10 {
            v_ts.push(step * 10);
            v_vals.push(2000.0);
        }
        for step in 0..=10 {
            v_ts.push(10_100 + step * 10);
            v_vals.push(3000.0);
        }

        // `pack_stats.pack_current` at 500 Hz (every 2 ms):
        //   Stint 1: linear ramp I(t) = 10.0 + 0.1 * t_ms (at t=0 -> 10A, at t=100 -> 20A)
        //    Plus an extra sample at t = 5,000 ms (in the middle of the 10s voltage gap!) to verify
        //   that union-grid timestamps inside the gap emit NaN!
        //   Stint 2: constant I(t) = 50.0 A
        let mut i_ts = Vec::new();
        let mut i_vals = Vec::new();
        for step in 0..=50 {
            let t = step * 2;
            i_ts.push(t);
            i_vals.push(10.0 + 0.1 * (t as f64));
        }
        i_ts.push(5_000);
        i_vals.push(999.0);
        for step in 0..=50 {
            let t = 10_100 + step * 2;
            i_ts.push(t);
            i_vals.push(50.0);
        }

        let series_v = SignalSeries::Exact {
            timestamps_ms: v_ts,
            values: v_vals,
        };
        let series_i = SignalSeries::Exact {
            timestamps_ms: i_ts,
            values: i_vals,
        };

        let power_series = engine
            .evaluate_channel_with(
                "battery.power_w",
                &TargetGrid::Union,
                Resolution::FullRes,
                |raw_id| match raw_id {
                    "pack_stats.vbatt_adc" => series_v.clone(),
                    "pack_stats.pack_current" => series_i.clone(),
                    other => panic!("unexpected raw signal request: {other}"),
                },
            )
            .expect("evaluate battery.power_w");

        let SignalSeries::Exact {
            timestamps_ms,
            values,
        } = power_series
        else {
            panic!("expected Exact series at FullRes");
        };

        // Total union points: 51 (Stint 1 at 2ms) + 1 (at 5000ms) + 51 (Stint 2 at 2ms) = 103
        assert_eq!(timestamps_ms.len(), 103);

        let v1_expected = 2000.0 * 0.14659808276;
        for idx in 0..=50 {
            let t = (idx * 2) as f64;
            let expected_w = v1_expected * (10.0 + 0.1 * t);
            assert!(
                (values[idx] - expected_w).abs() < 1e-6,
                "mismatch at idx={idx} (t={t}): got {}, expected {expected_w}",
                values[idx]
            );
        }

        // At idx = 51 (t = 5,000 ms, inside the 10-second gap), voltage is stale -> NaN!
        assert_eq!(timestamps_ms[51], 5_000);
        assert!(
            values[51].is_nan(),
            "expected NaN inside 10s gap at t=5000ms, got {}",
            values[51]
        );

        // Stint 2 (idx 52..=102, t = 10,100 ..= 10,200 ms)
        let v2_expected = 3000.0 * 0.14659808276;
        for idx in 52..=102 {
            let expected_w = v2_expected * 50.0;
            assert!(
                (values[idx] - expected_w).abs() < 1e-6,
                "mismatch at idx={idx}: got {}, expected {expected_w}",
                values[idx]
            );
        }
    }
}
