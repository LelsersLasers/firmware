use std::ops;

/// Window span threshold above which `Overview1Hz` (1,000 ms buckets) is selected (15 minutes).
pub const OVERVIEW_1HZ_THRESHOLD_MS: i64 = 15 * 60 * 1_000;

/// Window span threshold above which `Overview10Hz` (100 ms buckets) is selected (60 seconds).
pub const OVERVIEW_10HZ_THRESHOLD_MS: i64 = 60 * 1_000;

/// Bucket width in milliseconds for `Overview1Hz`.
pub const BUCKET_1HZ_MS: i64 = 1_000;

/// Bucket width in milliseconds for `Overview10Hz`.
pub const BUCKET_10HZ_MS: i64 = 100;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum Resolution {
    /// 1,000 ms min/max/mean buckets. Used when viewport span > 15 minutes.
    Overview1Hz,
    /// 100 ms min/max/mean buckets. Used when viewport span is 60s ..= 15 minutes.
    Overview10Hz,
    /// Exact `(utc_ms, value)` samples. Used when viewport span <= 60s (or during export).
    FullRes,
}

impl Resolution {
    /// Choose the pyramid resolution for a given viewport span (`window_ms`) and plot width in pixels.
    pub fn for_window_ms(window_ms: i64, plot_width_px: f32) -> Self {
        let span_ms = window_ms.max(0);
        if span_ms > OVERVIEW_1HZ_THRESHOLD_MS {
            return Self::Overview1Hz;
        }

        // If the plot is very narrow and 1Hz buckets already provide >= 4 buckets per pixel,
        // use 1Hz overview to avoid unnecessary work.
        if plot_width_px.is_finite() && plot_width_px > 0.0 {
            let buckets_1hz = (span_ms as f32) / (BUCKET_1HZ_MS as f32);
            if buckets_1hz >= plot_width_px * 4.0 {
                return Self::Overview1Hz;
            }
        }

        if span_ms > OVERVIEW_10HZ_THRESHOLD_MS {
            return Self::Overview10Hz;
        }

        if plot_width_px.is_finite() && plot_width_px > 0.0 {
            let buckets_10hz = (span_ms as f32) / (BUCKET_10HZ_MS as f32);
            if buckets_10hz >= plot_width_px * 8.0 {
                return Self::Overview10Hz;
            }
        }

        Self::FullRes
    }

    pub fn bucket_width_ms(self) -> Option<i64> {
        match self {
            Self::Overview1Hz => Some(BUCKET_1HZ_MS),
            Self::Overview10Hz => Some(BUCKET_10HZ_MS),
            Self::FullRes => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EnvelopePoint {
    pub utc_ms: i64,
    pub min: f64,
    pub max: f64,
    pub mean: f64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum SignalSeries {
    Exact {
        timestamps_ms: Vec<i64>,
        values: Vec<f64>,
    },
    Envelope(Vec<EnvelopePoint>),
}

impl SignalSeries {
    pub fn empty_for(resolution: Resolution) -> Self {
        match resolution {
            Resolution::FullRes => Self::Exact {
                timestamps_ms: Vec::new(),
                values: Vec::new(),
            },
            Resolution::Overview10Hz | Resolution::Overview1Hz => Self::Envelope(Vec::new()),
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Self::Exact { timestamps_ms, .. } => timestamps_ms.len(),
            Self::Envelope(points) => points.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Approximate heap footprint in bytes for LRU cache budget tracking.
    pub fn estimated_bytes(&self) -> usize {
        match self {
            Self::Exact {
                timestamps_ms,
                values,
            } => {
                std::mem::size_of::<Self>()
                    + timestamps_ms.len() * std::mem::size_of::<i64>()
                    + values.len() * std::mem::size_of::<f64>()
            }
            Self::Envelope(points) => {
                std::mem::size_of::<Self>() + points.len() * std::mem::size_of::<EnvelopePoint>()
            }
        }
    }

    pub fn time_bounds(&self) -> Option<(i64, i64)> {
        match self {
            Self::Exact { timestamps_ms, .. } => {
                Some((*timestamps_ms.first()?, *timestamps_ms.last()?))
            }
            Self::Envelope(points) => Some((points.first()?.utc_ms, points.last()?.utc_ms)),
        }
    }

    /// Return a new [`SignalSeries`] sliced to `[range.start(), range.end()]` using binary search.
    pub fn slice_range(&self, range: ops::RangeInclusive<i64>) -> Self {
        if range.is_empty() {
            return match self {
                Self::Exact { .. } => Self::Exact {
                    timestamps_ms: Vec::new(),
                    values: Vec::new(),
                },
                Self::Envelope(_) => Self::Envelope(Vec::new()),
            };
        }
        let start_ms = *range.start();
        let end_ms = *range.end();

        match self {
            Self::Exact {
                timestamps_ms,
                values,
            } => {
                let lo = timestamps_ms.partition_point(|&t| t < start_ms);
                let hi = timestamps_ms.partition_point(|&t| t <= end_ms);
                let lo = lo.min(timestamps_ms.len());
                let hi = hi.min(values.len()).max(lo);
                Self::Exact {
                    timestamps_ms: timestamps_ms[lo..hi].to_vec(),
                    values: values[lo..hi].to_vec(),
                }
            }
            Self::Envelope(points) => {
                let lo = points.partition_point(|p| p.utc_ms < start_ms);
                let hi = points.partition_point(|p| p.utc_ms <= end_ms);
                let lo = lo.min(points.len());
                let hi = hi.min(points.len()).max(lo);
                Self::Envelope(points[lo..hi].to_vec())
            }
        }
    }

    /// Find the latest sample `(utc_ms, value)` at or before `target_utc_ms`.
    pub fn latest_at_or_before(&self, target_utc_ms: i64) -> Option<(i64, f64)> {
        match self {
            Self::Exact {
                timestamps_ms,
                values,
            } => {
                let idx = timestamps_ms.partition_point(|&t| t <= target_utc_ms);
                let idx = idx.checked_sub(1)?;
                Some((*timestamps_ms.get(idx)?, *values.get(idx)?))
            }
            Self::Envelope(points) => {
                let idx = points.partition_point(|p| p.utc_ms <= target_utc_ms);
                let idx = idx.checked_sub(1)?;
                let pt = points.get(idx)?;
                Some((pt.utc_ms, pt.mean))
            }
        }
    }

    /// Stitch multiple chronological [`SignalSeries`] chunks into a single series.
    pub fn stitch(chunks: Vec<Self>, requested_resolution: Resolution) -> Self {
        let non_empty: Vec<Self> = chunks.into_iter().filter(|c| !c.is_empty()).collect();
        if non_empty.is_empty() {
            return Self::empty_for(requested_resolution);
        }
        if non_empty.len() == 1 {
            return non_empty.into_iter().next().unwrap();
        }

        let all_exact = non_empty.iter().all(|c| matches!(c, Self::Exact { .. }));
        let all_envelope = non_empty.iter().all(|c| matches!(c, Self::Envelope(_)));

        if all_exact {
            let total_len: usize = non_empty.iter().map(Self::len).sum();
            let mut timestamps_ms = Vec::with_capacity(total_len);
            let mut values = Vec::with_capacity(total_len);
            for chunk in non_empty {
                if let Self::Exact {
                    timestamps_ms: ts,
                    values: vs,
                } = chunk
                {
                    timestamps_ms.extend(ts);
                    values.extend(vs);
                }
            }
            Self::Exact {
                timestamps_ms,
                values,
            }
        } else if all_envelope || requested_resolution != Resolution::FullRes {
            let total_len: usize = non_empty.iter().map(Self::len).sum();
            let mut merged = Vec::with_capacity(total_len);
            for chunk in non_empty {
                match chunk {
                    Self::Envelope(pts) => merged.extend(pts),
                    Self::Exact {
                        timestamps_ms,
                        values,
                    } => {
                        merged.extend(timestamps_ms.into_iter().zip(values).map(|(utc_ms, val)| {
                            EnvelopePoint {
                                utc_ms,
                                min: val,
                                max: val,
                                mean: val,
                            }
                        }));
                    }
                }
            }
            Self::Envelope(merged)
        } else {
            // Mixed Exact + fallback Envelope when FullRes was requested:
            // preserve Exact samples and project fallback Envelope buckets via their mean.
            let total_len: usize = non_empty.iter().map(Self::len).sum();
            let mut timestamps_ms = Vec::with_capacity(total_len);
            let mut values = Vec::with_capacity(total_len);
            for chunk in non_empty {
                match chunk {
                    Self::Exact {
                        timestamps_ms: ts,
                        values: vs,
                    } => {
                        timestamps_ms.extend(ts);
                        values.extend(vs);
                    }
                    Self::Envelope(pts) => {
                        for pt in pts {
                            timestamps_ms.push(pt.utc_ms);
                            values.push(pt.mean);
                        }
                    }
                }
            }
            Self::Exact {
                timestamps_ms,
                values,
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StateInterval {
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    pub raw_int: i64,
    pub label: String,
}

impl StateInterval {
    pub fn overlaps(&self, range: &ops::RangeInclusive<i64>) -> bool {
        !range.is_empty() && self.start_utc_ms <= *range.end() && self.end_utc_ms >= *range.start()
    }

    pub fn clamped_to(&self, range: &ops::RangeInclusive<i64>) -> Option<Self> {
        if !self.overlaps(range) {
            return None;
        }
        let start_utc_ms = self.start_utc_ms.max(*range.start());
        let end_utc_ms = self.end_utc_ms.min(*range.end()).max(start_utc_ms);
        Some(Self {
            start_utc_ms,
            end_utc_ms,
            raw_int: self.raw_int,
            label: self.label.clone(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DataSourceKind {
    CanonicalSd,
    LiveProvisional,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CoverageSpan {
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    pub source: DataSourceKind,
    pub partition_id: Option<String>,
}

/// Aggregate ordered `(utc_ms, value)` pairs into fixed-width `[min, max, mean]` buckets.
pub fn build_envelope_buckets<'a>(
    samples: impl IntoIterator<Item = (i64, f64)> + 'a,
    bucket_width_ms: i64,
) -> Vec<EnvelopePoint> {
    let width = bucket_width_ms.max(1);
    let mut rows = Vec::new();
    let mut current_bucket_ms: Option<i64> = None;
    let mut min_val = f64::INFINITY;
    let mut max_val = f64::NEG_INFINITY;
    let mut sum_val = 0.0;
    let mut count = 0u32;

    for (utc_ms, val) in samples {
        if !val.is_finite() {
            continue;
        }
        let bucket_ms = utc_ms.div_euclid(width) * width;
        match current_bucket_ms {
            Some(active_bucket) if active_bucket == bucket_ms => {
                min_val = min_val.min(val);
                max_val = max_val.max(val);
                sum_val += val;
                count = count.saturating_add(1);
            }
            Some(active_bucket) => {
                if count > 0 {
                    rows.push(EnvelopePoint {
                        utc_ms: active_bucket,
                        min: min_val,
                        max: max_val,
                        mean: sum_val / f64::from(count),
                    });
                }
                current_bucket_ms = Some(bucket_ms);
                min_val = val;
                max_val = val;
                sum_val = val;
                count = 1;
            }
            None => {
                current_bucket_ms = Some(bucket_ms);
                min_val = val;
                max_val = val;
                sum_val = val;
                count = 1;
            }
        }
    }

    if let Some(active_bucket) = current_bucket_ms
        && count > 0
    {
        rows.push(EnvelopePoint {
            utc_ms: active_bucket,
            min: min_val,
            max: max_val,
            mean: sum_val / f64::from(count),
        });
    }

    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_selector_matches_roadmap_thresholds() {
        // <= 60s -> FullRes
        assert_eq!(
            Resolution::for_window_ms(10_000, 1200.0),
            Resolution::FullRes
        );
        assert_eq!(
            Resolution::for_window_ms(60_000, 1200.0),
            Resolution::FullRes
        );

        // 60s ..= 15 min -> Overview10Hz
        assert_eq!(
            Resolution::for_window_ms(60_001, 1200.0),
            Resolution::Overview10Hz
        );
        assert_eq!(
            Resolution::for_window_ms(900_000, 1200.0),
            Resolution::Overview10Hz
        );

        // > 15 min -> Overview1Hz
        assert_eq!(
            Resolution::for_window_ms(900_001, 1200.0),
            Resolution::Overview1Hz
        );
        assert_eq!(
            Resolution::for_window_ms(28_800_000, 1200.0),
            Resolution::Overview1Hz
        );
    }

    #[test]
    fn signal_series_slice_and_latest_lookup() {
        let series = SignalSeries::Exact {
            timestamps_ms: vec![1000, 1010, 1020, 1030, 1040],
            values: vec![1.0, 2.0, 3.0, 4.0, 5.0],
        };

        let sliced = series.slice_range(1010..=1030);
        assert_eq!(
            sliced,
            SignalSeries::Exact {
                timestamps_ms: vec![1010, 1020, 1030],
                values: vec![2.0, 3.0, 4.0],
            }
        );

        assert_eq!(series.latest_at_or_before(999), None);
        assert_eq!(series.latest_at_or_before(1000), Some((1000, 1.0)));
        assert_eq!(series.latest_at_or_before(1025), Some((1020, 3.0)));
        assert_eq!(series.latest_at_or_before(2000), Some((1040, 5.0)));
    }

    #[test]
    fn build_envelope_buckets_aggregates_min_max_mean() {
        let samples = vec![
            (1000, 10.0),
            (1200, 20.0),
            (1800, 15.0),
            (2100, 5.0),
            (2500, 15.0),
        ];
        let buckets = build_envelope_buckets(samples, 1000);
        assert_eq!(buckets.len(), 2);
        assert_eq!(
            buckets[0],
            EnvelopePoint {
                utc_ms: 1000,
                min: 10.0,
                max: 20.0,
                mean: 15.0,
            }
        );
        assert_eq!(
            buckets[1],
            EnvelopePoint {
                utc_ms: 2000,
                min: 5.0,
                max: 15.0,
                mean: 10.0,
            }
        );
    }
}
