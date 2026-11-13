use crate::router::{SignalSeries, StateInterval};
use std::collections::BTreeMap;

/// Strategy for choosing the common timestamp grid when aligning multiple signals.
#[derive(Debug, Clone, PartialEq)]
pub enum TargetGrid {
    /// Sorted, deduplicated union of all timestamps present across the input series.
    Union,
    /// Timestamps of whichever input series has the greatest sample count in the window.
    HighestRate,
    /// Uniformly spaced grid from `start_utc_ms` to `end_utc_ms` (inclusive) at `rate_hz`.
    UniformGridHz {
        start_utc_ms: i64,
        end_utc_ms: i64,
        rate_hz: f64,
    },
    /// Caller-supplied sorted timestamp vector (in `utc_ms`).
    Explicit(Vec<i64>),
}

impl TargetGrid {
    /// Materialize the sorted, deduplicated timestamp vector `Vec<i64>` for the given input series.
    pub fn build_timestamps<'a>(
        &self,
        inputs: impl IntoIterator<Item = &'a [i64]>,
    ) -> Vec<i64> {
        match self {
            Self::Union => {
                let slices: Vec<&'a [i64]> = inputs.into_iter().filter(|s| !s.is_empty()).collect();
                if slices.is_empty() {
                    return Vec::new();
                }
                if slices.len() == 1 {
                    return dedup_sorted_i64(slices[0]);
                }
                let total_cap: usize = slices.iter().map(|s| s.len()).sum();
                let mut merged = Vec::with_capacity(total_cap);
                for s in slices {
                    merged.extend_from_slice(s);
                }
                merged.sort_unstable();
                merged.dedup();
                merged
            }
            Self::HighestRate => {
                let best = inputs
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .max_by_key(|s| s.len());
                match best {
                    Some(ts) => dedup_sorted_i64(ts),
                    None => Vec::new(),
                }
            }
            Self::UniformGridHz {
                start_utc_ms,
                end_utc_ms,
                rate_hz,
            } => {
                if *end_utc_ms < *start_utc_ms || !rate_hz.is_finite() || *rate_hz <= 0.0 {
                    return Vec::new();
                }
                let span_ms = (*end_utc_ms - *start_utc_ms) as f64;
                let period_ms = 1000.0 / *rate_hz;
                if period_ms <= 0.0 || !period_ms.is_finite() {
                    return Vec::new();
                }
                let steps = (span_ms / period_ms).floor() as usize;
                let mut grid = Vec::with_capacity(steps.saturating_add(1));
                let mut prev: Option<i64> = None;
                for i in 0..=steps {
                    let t = start_utc_ms
                        .saturating_add((i as f64 * period_ms).round() as i64)
                        .min(*end_utc_ms);
                    if prev != Some(t) {
                        grid.push(t);
                        prev = Some(t);
                    }
                }
                grid
            }
            Self::Explicit(ts) => {
                let mut out = ts.clone();
                out.sort_unstable();
                out.dedup();
                out
            }
        }
    }
}

fn dedup_sorted_i64(ts: &[i64]) -> Vec<i64> {
    let mut out = Vec::with_capacity(ts.len());
    for &t in ts {
        if out.last().copied() != Some(t) {
            out.push(t);
        }
    }
    if !out.windows(2).all(|w| w[0] < w[1]) {
        out.sort_unstable();
        out.dedup();
    }
    out
}

/// Collapse duplicate timestamps in `(timestamps_ms, values)`, keeping the last finite value
/// at each unique timestamp and ensuring strictly increasing timestamp order.
fn normalize_source_samples(timestamps_ms: &[i64], values: &[f64]) -> (Vec<i64>, Vec<f64>) {
    let n = timestamps_ms.len().min(values.len());
    if n == 0 {
        return (Vec::new(), Vec::new());
    }

    // Fast path when already strictly increasing
    if timestamps_ms[..n].windows(2).all(|w| w[0] < w[1]) {
        return (timestamps_ms[..n].to_vec(), values[..n].to_vec());
    }

    let mut pairs: Vec<(i64, f64)> = timestamps_ms[..n]
        .iter()
        .copied()
        .zip(values[..n].iter().copied())
        .collect();
    // Stable sort preserves chronological arrival order for identical millisecond timestamps
    pairs.sort_by_key(|&(t, _)| t);

    let mut out_ts: Vec<i64> = Vec::with_capacity(n);
    let mut out_vals: Vec<f64> = Vec::with_capacity(n);

    for (t, v) in pairs {
        if out_ts.last().copied() == Some(t) {
            if v.is_finite() {
                if let Some(last_v) = out_vals.last_mut() {
                    *last_v = v;
                }
            }
        } else {
            out_ts.push(t);
            out_vals.push(v);
        }
    }

    (out_ts, out_vals)
}

/// Resample a continuous signal `(timestamps_ms, values)` onto `target_grid_ms` using linear
/// interpolation with a maximum staleness threshold `max_staleness_ms`.
///
/// - Exact timestamp hits `t == t_i` emit `values[i]`.
/// - Interior points `t_0 < t < t_1` linearly interpolate between `v_0` and `v_1` **only if**
///   `t_1 - t_0 <= max_staleness_ms` and both `v_0` and `v_1` are finite.
/// - Points falling inside a gap where `t_1 - t_0 > max_staleness_ms` or outside `[t_first, t_last]`
///   emit `f64::NAN` so plots break the trace instead of drawing a fake line across a break.
pub fn resample_continuous_linear(
    timestamps_ms: &[i64],
    values: &[f64],
    target_grid_ms: &[i64],
    max_staleness_ms: i64,
) -> Vec<f64> {
    if target_grid_ms.is_empty() {
        return Vec::new();
    }
    let (src_ts, src_vals) = normalize_source_samples(timestamps_ms, values);
    if src_ts.is_empty() {
        return vec![f64::NAN; target_grid_ms.len()];
    }

    let staleness = max_staleness_ms.max(0);
    let src_len = src_ts.len();
    let mut out = Vec::with_capacity(target_grid_ms.len());
    let mut idx = 0usize;

    for &t in target_grid_ms {
        if t < src_ts[0] {
            out.push(f64::NAN);
            continue;
        }

        // Advance `idx` so that `src_ts[idx] <= t` and either `idx + 1 == src_len` or `src_ts[idx + 1] > t`
        while idx + 1 < src_len && src_ts[idx + 1] <= t {
            idx += 1;
        }

        let t0 = src_ts[idx];
        let v0 = src_vals[idx];

        if t == t0 {
            out.push(v0);
            continue;
        }

        if idx + 1 >= src_len {
            // Past the last sample
            out.push(f64::NAN);
            continue;
        }

        let t1 = src_ts[idx + 1];
        let v1 = src_vals[idx + 1];
        let span = t1.saturating_sub(t0);

        if span <= staleness && span > 0 && v0.is_finite() && v1.is_finite() {
            let alpha = (t - t0) as f64 / (span as f64);
            out.push(v0 + alpha * (v1 - v0));
        } else {
            out.push(f64::NAN);
        }
    }

    out
}

/// Resample discrete state intervals onto `target_grid_ms` using zero-order hold
/// (previous-value hold within each interval `[start_utc_ms, end_utc_ms]`).
///
/// Timestamps outside all intervals emit `f64::NAN`.
pub fn resample_discrete_hold(intervals: &[StateInterval], target_grid_ms: &[i64]) -> Vec<f64> {
    if target_grid_ms.is_empty() {
        return Vec::new();
    }
    if intervals.is_empty() {
        return vec![f64::NAN; target_grid_ms.len()];
    }

    let mut out = Vec::with_capacity(target_grid_ms.len());
    let mut idx = 0usize;
    let len = intervals.len();

    for &t in target_grid_ms {
        while idx < len && intervals[idx].end_utc_ms < t {
            idx += 1;
        }
        if idx < len && intervals[idx].start_utc_ms <= t && t <= intervals[idx].end_utc_ms {
            out.push(intervals[idx].raw_int as f64);
        } else {
            out.push(f64::NAN);
        }
    }

    out
}

/// Extract `(timestamps_ms, values)` from a [`SignalSeries`] (using bucket `mean` when given an `Envelope`).
pub fn series_to_timestamp_values(series: &SignalSeries) -> (Vec<i64>, Vec<f64>) {
    match series {
        SignalSeries::Exact {
            timestamps_ms,
            values,
        } => (timestamps_ms.clone(), values.clone()),
        SignalSeries::Envelope(buckets) => {
            let mut ts = Vec::with_capacity(buckets.len());
            let mut vals = Vec::with_capacity(buckets.len());
            for b in buckets {
                ts.push(b.utc_ms);
                vals.push(b.mean);
            }
            (ts, vals)
        }
    }
}

/// Align a map of named [`SignalSeries`] onto a shared timestamp grid using staleness-aware
/// linear interpolation.
pub fn align_series_map(
    series_map: &BTreeMap<String, SignalSeries>,
    grid: &TargetGrid,
    max_staleness_ms: i64,
) -> (Vec<i64>, BTreeMap<String, Vec<f64>>) {
    let extracted: BTreeMap<String, (Vec<i64>, Vec<f64>)> = series_map
        .iter()
        .map(|(k, s)| (k.clone(), series_to_timestamp_values(s)))
        .collect();

    let target_ts = grid.build_timestamps(extracted.values().map(|(ts, _)| ts.as_slice()));
    let mut aligned = BTreeMap::new();

    for (name, (ts, vals)) in &extracted {
        let col = resample_continuous_linear(ts, vals, &target_ts, max_staleness_ms);
        aligned.insert(name.clone(), col);
    }

    (target_ts, aligned)
}

/// Synchronized `(utc_ms, x, y)` sample pairs with all `NaN`/stale rows filtered out.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AlignedXY {
    pub timestamps_ms: Vec<i64>,
    pub x_values: Vec<f64>,
    pub y_values: Vec<f64>,
}

/// Align two [`SignalSeries`] (`x` and `y`) onto their union timestamp grid using
/// `max_staleness_ms` and return only rows where both `x` and `y` are finite.
pub fn align_xy_pairs(
    x_series: &SignalSeries,
    y_series: &SignalSeries,
    max_staleness_ms: i64,
) -> AlignedXY {
    let (x_ts, x_vals) = series_to_timestamp_values(x_series);
    let (y_ts, y_vals) = series_to_timestamp_values(y_series);
    let grid = TargetGrid::Union.build_timestamps([x_ts.as_slice(), y_ts.as_slice()]);
    let x_aligned = resample_continuous_linear(&x_ts, &x_vals, &grid, max_staleness_ms);
    let y_aligned = resample_continuous_linear(&y_ts, &y_vals, &grid, max_staleness_ms);

    let mut out = AlignedXY::default();
    for ((t, x), y) in grid.into_iter().zip(x_aligned).zip(y_aligned) {
        if x.is_finite() && y.is_finite() {
            out.timestamps_ms.push(t);
            out.x_values.push(x);
            out.y_values.push(y);
        }
    }
    out
}

/// Synchronized `(utc_ms, x, y, z)` sample triples with all `NaN`/stale rows filtered out.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AlignedXYZ {
    pub timestamps_ms: Vec<i64>,
    pub x_values: Vec<f64>,
    pub y_values: Vec<f64>,
    pub z_values: Vec<f64>,
}

/// Align three [`SignalSeries`] (`x`, `y`, `z`) onto their union timestamp grid using
/// `max_staleness_ms` and return only rows where all three coordinates are finite.
pub fn align_xyz_triples(
    x_series: &SignalSeries,
    y_series: &SignalSeries,
    z_series: &SignalSeries,
    max_staleness_ms: i64,
) -> AlignedXYZ {
    let (x_ts, x_vals) = series_to_timestamp_values(x_series);
    let (y_ts, y_vals) = series_to_timestamp_values(y_series);
    let (z_ts, z_vals) = series_to_timestamp_values(z_series);
    let grid =
        TargetGrid::Union.build_timestamps([x_ts.as_slice(), y_ts.as_slice(), z_ts.as_slice()]);
    let x_aligned = resample_continuous_linear(&x_ts, &x_vals, &grid, max_staleness_ms);
    let y_aligned = resample_continuous_linear(&y_ts, &y_vals, &grid, max_staleness_ms);
    let z_aligned = resample_continuous_linear(&z_ts, &z_vals, &grid, max_staleness_ms);

    let mut out = AlignedXYZ::default();
    for (((t, x), y), z) in grid
        .into_iter()
        .zip(x_aligned)
        .zip(y_aligned)
        .zip(z_aligned)
    {
        if x.is_finite() && y.is_finite() && z.is_finite() {
            out.timestamps_ms.push(t);
            out.x_values.push(x);
            out.y_values.push(y);
            out.z_values.push(z);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resamples_continuous_signals_and_emits_nan_across_staleness_gaps() {
        // Signal sampled at t = 0, 100, 200 ms, then a 1,000 ms gap, then t = 1200, 1300 ms
        let src_ts = vec![0, 100, 200, 1200, 1300];
        let src_vals = vec![0.0, 10.0, 20.0, 100.0, 110.0];
        let target = vec![-50, 0, 50, 100, 150, 200, 500, 900, 1200, 1250, 1300, 1400];

        let out = resample_continuous_linear(&src_ts, &src_vals, &target, 250);
        assert!(out[0].is_nan()); // -50 before start
        assert_eq!(out[1], 0.0); // exact t=0
        assert!((out[2] - 5.0).abs() < 1e-9); // interpolated t=50
        assert_eq!(out[3], 10.0); // exact t=100
        assert!((out[4] - 15.0).abs() < 1e-9); // interpolated t=150
        assert_eq!(out[5], 20.0); // exact t=200
        assert!(out[6].is_nan()); // t=500 inside 1000ms gap (> 250ms staleness)
        assert!(out[7].is_nan()); // t=900 inside 1000ms gap
        assert_eq!(out[8], 100.0); // exact t=1200
        assert!((out[9] - 105.0).abs() < 1e-9); // interpolated t=1250
        assert_eq!(out[10], 110.0); // exact t=1300
        assert!(out[11].is_nan()); // t=1400 after end
    }

    #[test]
    fn resamples_discrete_state_intervals_with_zero_order_hold() {
        let intervals = vec![
            StateInterval {
                start_utc_ms: 100,
                end_utc_ms: 300,
                raw_int: 1,
                label: "IDLE".to_owned(),
            },
            StateInterval {
                start_utc_ms: 500,
                end_utc_ms: 800,
                raw_int: 2,
                label: "DRIVE".to_owned(),
            },
        ];
        let target = vec![50, 100, 200, 300, 400, 500, 750, 900];
        let held = resample_discrete_hold(&intervals, &target);

        assert!(held[0].is_nan());
        assert_eq!(held[1], 1.0);
        assert_eq!(held[2], 1.0);
        assert_eq!(held[3], 1.0);
        assert!(held[4].is_nan());
        assert_eq!(held[5], 2.0);
        assert_eq!(held[6], 2.0);
        assert!(held[7].is_nan());
    }

    #[test]
    fn uniform_grid_hz_and_xy_pair_alignment() {
        let grid = TargetGrid::UniformGridHz {
            start_utc_ms: 1000,
            end_utc_ms: 1100,
            rate_hz: 20.0, // 50 ms step
        }
        .build_timestamps([]);
        assert_eq!(grid, vec![1000, 1050, 1100]);

        let x = SignalSeries::Exact {
            timestamps_ms: vec![1000, 1100],
            values: vec![1.0, 3.0],
        };
        let y = SignalSeries::Exact {
            timestamps_ms: vec![1000, 1050, 1100],
            values: vec![10.0, 20.0, 30.0],
        };
        let xy = align_xy_pairs(&x, &y, 250);
        assert_eq!(xy.timestamps_ms, vec![1000, 1050, 1100]);
        assert_eq!(xy.x_values, vec![1.0, 2.0, 3.0]);
        assert_eq!(xy.y_values, vec![10.0, 20.0, 30.0]);
    }
}
