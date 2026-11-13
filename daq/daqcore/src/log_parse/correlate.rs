use crate::{log_parse, superdbc, time};
use chrono::TimeZone as _;

// In that case we use this fallback slope value. It is generally safe to assume a
// slope of 1.0 since the DAQ runs at 1 tick = 1 ms, but in post-processing there
// is no reason not to run a proper linear regression when multiple GPS points exist.
const REGRESSION_FALLBACK_SLOPE: f64 = 1.0;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ClockConfidence {
    GpsSynced {
        rms_error_ms: f64,
        point_count: usize,
    },
    RtcEstimated,
    FileMtimeEstimated,
    ManualOffset,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClockMapping {
    /// real_time ~= slope * log_time_ms + intercept_ms
    ///
    /// Stored as:
    /// utc_ms = round(slope * ticks_ms + intercept_ms)
    pub slope: f64,
    pub intercept_ms: f64,
    pub confidence: ClockConfidence,
}

impl ClockMapping {
    pub fn ticks_to_utc_ms(&self, ticks_ms: u32) -> i64 {
        (self.slope * f64::from(ticks_ms) + self.intercept_ms).round() as i64
    }

    pub fn correlate(&self, log_ts: u32) -> Option<chrono::DateTime<chrono::Utc>> {
        let unix_ms = self.ticks_to_utc_ms(log_ts);
        match chrono::DateTime::from_timestamp_millis(unix_ms) {
            Some(dt) => Some(dt),
            None => {
                log::error!(
                    "Correlated time {} ms for log time {} ms is out of range for chrono::DateTime",
                    unix_ms,
                    log_ts
                );
                None
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct CorrelationChunkResult {
    pub parsed_msgs: Vec<log_parse::parse::ParsedMessage>,
    pub clock_mapping: ClockMapping,
}

pub fn time_correlate_chunks(
    chunks: Vec<Vec<log_parse::parse::ParsedMessage>>,
    files: &[log_parse::parse::SourceLogFile],
) -> Vec<CorrelationChunkResult> {
    let mut results = Vec::with_capacity(chunks.len());
    let mut prev_tier3_end_utc_ms: Option<i64> = None;

    for chunk in chunks {
        let mut res = time_correlate_chunk(chunk, files);
        if res.clock_mapping.confidence == ClockConfidence::FileMtimeEstimated
            && let (Some(first_msg), Some(last_msg)) =
                (res.parsed_msgs.first(), res.parsed_msgs.last())
        {
            let start_utc = res.clock_mapping.ticks_to_utc_ms(first_msg.timestamp);
            if let Some(prev_end) = prev_tier3_end_utc_ms
                && start_utc <= prev_end
            {
                let adjusted_start = prev_end + 1_000;
                res.clock_mapping.intercept_ms =
                    (adjusted_start as f64) - f64::from(first_msg.timestamp);
            }
            let new_end = res.clock_mapping.ticks_to_utc_ms(last_msg.timestamp);
            prev_tier3_end_utc_ms = Some(new_end);
        }
        results.push(res);
    }

    results
}

pub fn physical_to_u64(dsv: &superdbc::decode::DecodedSignalValue) -> u64 {
    dsv.physical().round() as u64
}

pub fn time_correlate_chunk(
    chunk: Vec<log_parse::parse::ParsedMessage>,
    files: &[log_parse::parse::SourceLogFile],
) -> CorrelationChunkResult {
    // Tier 1: Look for valid GPS messages (`gps_time`) in the chunk and fit a linear regression.
    // Tier 2: If no valid GPS messages exist, anchor to the STM32 RTC filename timestamp.
    // Tier 3: Otherwise, anchor to the source file's filesystem modification timestamp (mtime).

    let mut gps_points = Vec::new();
    for msg in &chunk {
        if msg.decoded.name() == "gps_time" {
            let millisecond = msg.decoded.signal("millisecond").map(physical_to_u64);
            let second = msg.decoded.signal("second").map(physical_to_u64);
            let minute = msg.decoded.signal("minute").map(physical_to_u64);
            let hour = msg.decoded.signal("hour").map(physical_to_u64);
            let day = msg.decoded.signal("day").map(physical_to_u64);
            let month = msg.decoded.signal("month").map(physical_to_u64);
            let year = msg.decoded.signal("year").map(physical_to_u64);

            if let (Some(ms), Some(s), Some(min), Some(h), Some(d), Some(mon), Some(y)) =
                (millisecond, second, minute, hour, day, month, year)
            {
                let full_year = if y < 100 { 2000 + y as i32 } else { y as i32 };
                if !(2020..=2100).contains(&full_year) {
                    log::debug!(
                        "GPS message at {} ms has uninitialized or out-of-range year {}, skipping",
                        msg.timestamp,
                        full_year
                    );
                    continue;
                }

                if let Some(dt) = chrono::NaiveDate::from_ymd_opt(full_year, mon as u32, d as u32)
                    .and_then(|date| {
                        date.and_hms_milli_opt(h as u32, min as u32, s as u32, ms as u32)
                    })
                {
                    let dt_utc = chrono::Utc.from_utc_datetime(&dt);
                    gps_points.push((msg.timestamp, dt_utc));
                } else {
                    log::error!(
                        "GPS message at {} ms has invalid date/time values, skipping",
                        msg.timestamp
                    );
                }
            } else {
                log::error!(
                    "GPS message at {} ms is missing some time signals, skipping",
                    msg.timestamp
                );
            }
        }
    }

    if !gps_points.is_empty() {
        let points: Vec<Point> = gps_points
            .iter()
            .map(|(log_ts, real_ts)| Point {
                x: *log_ts as f64,
                y: real_ts.timestamp_millis() as f64,
            })
            .collect();

        if let Some((mut slope, mut intercept)) = linear_regression(&points) {
            if !slope.is_finite() || (slope - REGRESSION_FALLBACK_SLOPE).abs() > 0.1 {
                log::warn!(
                    "GPS regression slope ({:.9}) is non-physical; falling back to unit slope 1.0",
                    slope
                );
                slope = REGRESSION_FALLBACK_SLOPE;
                intercept =
                    points.iter().map(|p| p.y - slope * p.x).sum::<f64>() / (points.len() as f64);
            } else if (slope - REGRESSION_FALLBACK_SLOPE).abs() > 0.001 {
                log::warn!(
                    "GPS correlation slope ({:.9}) deviates from expected {:.3}.",
                    slope,
                    REGRESSION_FALLBACK_SLOPE,
                );
            }

            let rms_error_ms = {
                let mse = points
                    .iter()
                    .map(|p| {
                        let predicted = slope * p.x + intercept;
                        let error = p.y - predicted;
                        error * error
                    })
                    .sum::<f64>()
                    / points.len() as f64;
                mse.sqrt()
            };

            log::info!(
                "Tier 1 (GpsSynced) correlation successful: slope={:.9}, intercept_ms={:.3}, rms_error_ms={:.2}, points={}",
                slope,
                intercept,
                rms_error_ms,
                points.len()
            );

            return CorrelationChunkResult {
                parsed_msgs: chunk,
                clock_mapping: ClockMapping {
                    slope,
                    intercept_ms: intercept,
                    confidence: ClockConfidence::GpsSynced {
                        rms_error_ms,
                        point_count: points.len(),
                    },
                },
            };
        }
    }

    // Tier 2: Check for STM32 hardware RTC timestamp in source .log filename
    for msg in &chunk {
        if let Some(file_meta) = files.get(msg.source_file_idx)
            && let Some(rtc_unix_ms) = file_meta.rtc_timestamp_ms
        {
            let intercept_ms = (rtc_unix_ms as f64) - f64::from(msg.timestamp);
            log::info!(
                "Tier 2 (RtcEstimated) correlation from {}: intercept_ms={:.3}",
                file_meta.file_name,
                intercept_ms
            );
            return CorrelationChunkResult {
                parsed_msgs: chunk,
                clock_mapping: ClockMapping {
                    slope: REGRESSION_FALLBACK_SLOPE,
                    intercept_ms,
                    confidence: ClockConfidence::RtcEstimated,
                },
            };
        }
    }

    // Tier 3: Fallback to filesystem mtime of the first message's source .log file
    let (first_ticks, mtime_unix_ms) = chunk
        .first()
        .map(|first_msg| {
            let mtime = files
                .get(first_msg.source_file_idx)
                .map(|f| f.mtime_unix_ms)
                .unwrap_or_else(|| time::Time::now().unix_millis());
            (first_msg.timestamp, mtime)
        })
        .unwrap_or_else(|| (0, time::Time::now().unix_millis()));

    let intercept_ms = (mtime_unix_ms as f64) - f64::from(first_ticks);
    log::warn!(
        "No GPS or RTC filename timestamp found in chunk; using Tier 3 (FileMtimeEstimated): intercept_ms={:.3}",
        intercept_ms
    );

    CorrelationChunkResult {
        parsed_msgs: chunk,
        clock_mapping: ClockMapping {
            slope: REGRESSION_FALLBACK_SLOPE,
            intercept_ms,
            confidence: ClockConfidence::FileMtimeEstimated,
        },
    }
}

struct Point {
    x: f64, // log timestamp ms
    y: f64, // unix timestamp ms
}

/// Least squares linear regression.
///
/// Note: if there is only 1 point (or all points share identical x), uses REGRESSION_FALLBACK_SLOPE.
///
/// Fits:
/// y = slope * x + intercept
fn linear_regression(points: &[Point]) -> Option<(f64, f64)> {
    if points.is_empty() {
        return None;
    }

    if points.len() == 1 {
        log::warn!(
            "Only one GPS point found, using fallback slope of {}",
            REGRESSION_FALLBACK_SLOPE
        );
        let p = &points[0];
        let slope = REGRESSION_FALLBACK_SLOPE;
        let intercept = p.y - slope * p.x;
        return Some((slope, intercept));
    }

    let n = points.len() as f64;

    let mut sum_x = 0.0;
    let mut sum_y = 0.0;
    let mut sum_xy = 0.0;
    let mut sum_x2 = 0.0;

    for p in points {
        sum_x += p.x;
        sum_y += p.y;
        sum_xy += p.x * p.y;
        sum_x2 += p.x * p.x;
    }

    let denom = n * sum_x2 - sum_x * sum_x;

    if denom.abs() < 1e-9 {
        let slope = REGRESSION_FALLBACK_SLOPE;
        let intercept = (sum_y - slope * sum_x) / n;
        return Some((slope, intercept));
    }

    let slope = (n * sum_xy - sum_x * sum_y) / denom;
    let intercept = (sum_y - slope * sum_x) / n;

    Some((slope, intercept))
}
