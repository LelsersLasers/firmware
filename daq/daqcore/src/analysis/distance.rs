use crate::{
    analysis::align::{resample_continuous_linear, series_to_timestamp_values},
    router::SignalSeries,
};
use std::ops;

/// Default maximum gap (in milliseconds) across which vehicle velocity is trapezoidally
/// integrated before pausing distance accumulation across a stop/reboot gap.
pub const DEFAULT_SPEED_INTEGRATION_GAP_MS: i64 = 2_000;

/// A signal series indexed by cumulative track distance `distances_m` (in meters)
/// alongside its original `timestamps_ms` and `values`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DistanceSeries {
    pub timestamps_ms: Vec<i64>,
    pub distances_m: Vec<f64>,
    pub values: Vec<f64>,
}

impl DistanceSeries {
    pub fn len(&self) -> usize {
        self.distances_m.len()
    }

    pub fn is_empty(&self) -> bool {
        self.distances_m.is_empty()
    }

    pub fn total_distance_m(&self) -> f64 {
        self.distances_m.last().copied().unwrap_or(0.0)
    }
}

/// Integrate vehicle velocity $s(t) = \int_{t_{\text{start}}}^{t} v(\tau)\,d\tau$ in meters
/// over `range` (`[start_utc_ms..=end_utc_ms]`).
///
/// Returns a monotonically non-decreasing `Vec<f64>` of cumulative distances in meters
/// corresponding 1-to-1 with the samples of `speed_series.slice_range(range)`.
pub fn compute_distance_axis(
    speed_series: &SignalSeries,
    range: ops::RangeInclusive<i64>,
) -> Vec<f64> {
    let default_gap = match speed_series {
        SignalSeries::Exact { .. } => DEFAULT_SPEED_INTEGRATION_GAP_MS,
        SignalSeries::Envelope(_) => DEFAULT_SPEED_INTEGRATION_GAP_MS.max(2_500),
    };
    compute_distance_axis_with_max_gap(speed_series, range, default_gap)
}

/// Integrate vehicle velocity $s(t)$ in meters over `range`, pausing integration across
/// any adjacent timestamp gap larger than `max_gap_ms`.
pub fn compute_distance_axis_with_max_gap(
    speed_series: &SignalSeries,
    range: ops::RangeInclusive<i64>,
    max_gap_ms: i64,
) -> Vec<f64> {
    let sliced = speed_series.slice_range(range);
    let (ts, vals) = series_to_timestamp_values(&sliced);
    integrate_velocity_samples(&ts, &vals, max_gap_ms)
}

/// Integrate raw `(timestamps_ms, speed_mps)` samples into cumulative distance in meters.
///
/// Negative speed noise is clamped to `0.0`, and gaps where `dt > max_gap_ms` or either
/// endpoint is non-finite contribute `0.0 m`.
pub fn integrate_velocity_samples(
    timestamps_ms: &[i64],
    speed_mps: &[f64],
    max_gap_ms: i64,
) -> Vec<f64> {
    let n = timestamps_ms.len().min(speed_mps.len());
    if n == 0 {
        return Vec::new();
    }
    let gap_limit = max_gap_ms.max(1);
    let mut distances = Vec::with_capacity(n);
    let mut cumulative_m = 0.0;
    distances.push(0.0);

    for i in 1..n {
        let dt_ms = timestamps_ms[i].saturating_sub(timestamps_ms[i - 1]);
        let v0 = speed_mps[i - 1];
        let v1 = speed_mps[i];
        if dt_ms > 0 && dt_ms <= gap_limit && v0.is_finite() && v1.is_finite() {
            let v0_clamped = v0.max(0.0);
            let v1_clamped = v1.max(0.0);
            cumulative_m += 0.5 * (v0_clamped + v1_clamped) * (dt_ms as f64 / 1000.0);
        }
        distances.push(cumulative_m);
    }

    distances
}

/// Re-index any `signal_series` $Y(t)$ onto cumulative track distance $s(t) \in [0, L]\text{ m}$
/// integrated from `speed_series` over `range`.
pub fn map_series_to_distance(
    signal_series: &SignalSeries,
    speed_series: &SignalSeries,
    range: ops::RangeInclusive<i64>,
    max_staleness_ms: i64,
) -> DistanceSeries {
    let sliced_sig = signal_series.slice_range(range.clone());
    let sliced_spd = speed_series.slice_range(range);
    let (sig_ts, sig_vals) = series_to_timestamp_values(&sliced_sig);
    if sig_ts.is_empty() {
        return DistanceSeries::default();
    }
    let (spd_ts, spd_vals) = series_to_timestamp_values(&sliced_spd);
    let speed_staleness = max_staleness_ms.max(DEFAULT_SPEED_INTEGRATION_GAP_MS);
    let resampled_speed = resample_continuous_linear(&spd_ts, &spd_vals, &sig_ts, speed_staleness);
    let distances_m = integrate_velocity_samples(&sig_ts, &resampled_speed, speed_staleness);

    DistanceSeries {
        timestamps_ms: sig_ts,
        distances_m,
        values: sig_vals,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integrates_constant_and_linear_velocity_and_pauses_across_gaps() {
        // 0..=4000 ms at constant 20 m/s -> 80.0 m
        // Then a 10,000 ms paddock stop gap (4000 -> 14000 ms)
        // Then 14000..=16000 ms (2s) ramping from 10 m/s to 30 m/s (mean 20 m/s * 2s = 40.0 m) -> total 120.0 m
        let speed = SignalSeries::Exact {
            timestamps_ms: vec![0, 1000, 2000, 3000, 4000, 14000, 15000, 16000],
            values: vec![20.0, 20.0, 20.0, 20.0, 20.0, 10.0, 20.0, 30.0],
        };

        let dist = compute_distance_axis(&speed, 0..=16000);
        assert_eq!(dist.len(), 8);
        assert!((dist[0] - 0.0).abs() < 1e-9);
        assert!((dist[1] - 20.0).abs() < 1e-9);
        assert!((dist[2] - 40.0).abs() < 1e-9);
        assert!((dist[3] - 60.0).abs() < 1e-9);
        assert!((dist[4] - 80.0).abs() < 1e-9);
        // Paused across the 10-second gap!
        assert!((dist[5] - 80.0).abs() < 1e-9);
        assert!((dist[6] - 95.0).abs() < 1e-9);
        assert!((dist[7] - 120.0).abs() < 1e-9);
    }

    #[test]
    fn maps_high_rate_signal_onto_lower_rate_speed_distance_axis() {
        // Speed at 10 Hz (every 100 ms): constant 10 m/s over 0..=200 ms -> total 2.0 m
        let speed = SignalSeries::Exact {
            timestamps_ms: vec![0, 100, 200],
            values: vec![10.0, 10.0, 10.0],
        };
        // Shock pot at 20 Hz (every 50 ms): 0, 50, 100, 150, 200 ms
        let shock = SignalSeries::Exact {
            timestamps_ms: vec![0, 50, 100, 150, 200],
            values: vec![12.0, 14.0, 16.0, 15.0, 13.0],
        };

        let mapped = map_series_to_distance(&shock, &speed, 0..=200, 250);
        assert_eq!(mapped.len(), 5);
        assert!((mapped.distances_m[0] - 0.0).abs() < 1e-9);
        assert!((mapped.distances_m[1] - 0.5).abs() < 1e-9);
        assert!((mapped.distances_m[2] - 1.0).abs() < 1e-9);
        assert!((mapped.distances_m[3] - 1.5).abs() < 1e-9);
        assert!((mapped.distances_m[4] - 2.0).abs() < 1e-9);
        assert_eq!(mapped.values, vec![12.0, 14.0, 16.0, 15.0, 13.0]);
    }
}
