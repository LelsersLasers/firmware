use crate::storage;

pub const SPEED_THRESHOLD_MPS: f64 = 1.0;
pub const MIN_STINT_DURATION_MS: i64 = 5_000;
pub const STOP_DEBOUNCE_MS: i64 = 10_000;

/// Approximate RPM-to-m/s conversion factor for fallback motor shaft speed detection.
const MOTOR_RPM_PER_MPS: f64 = 550.0;

/// First-class point or interval annotation on the unified `utc_ms` timeline.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Annotation {
    pub id: String,
    pub track_day_id: String,
    pub name: String,
    pub category: String,
    pub color: String,
    pub start_utc_ms: i64,
    pub end_utc_ms: Option<i64>,
    pub author: String,
    pub source: String,
    pub notes: String,
    pub updated_at_ms: i64,
    pub deleted: bool,
}

/// Detect driving stints from a time-sorted slice of `(utc_ms, speed_mps)` samples.
///
/// Emits a `driving_stint` interval annotation whenever vehicle speed exceeds
/// [`SPEED_THRESHOLD_MPS`] (`1.0 m/s`) for at least [`MIN_STINT_DURATION_MS`] (`5.0 s`),
/// using a [`STOP_DEBOUNCE_MS`] (`10.0 s`) hysteresis window so brief stops do not
/// fragment a continuous run.
pub fn detect_motion_stints(
    speed_samples: &[(i64, f64)],
    partition_id: &str,
    track_day_id: &str,
    updated_at_ms: i64,
) -> Vec<Annotation> {
    let mut annotations = Vec::new();
    let mut candidate_start_ms: Option<i64> = None;
    let mut last_moving_ms: i64 = 0;
    let mut max_speed_mps: f64 = 0.0;

    let mut emit_if_valid = |start_ms: i64, end_ms: i64, peak_mps: f64| {
        let duration_ms = end_ms.saturating_sub(start_ms);
        if duration_ms >= MIN_STINT_DURATION_MS {
            let stint_idx = annotations.len() + 1;
            annotations.push(Annotation {
                id: format!("auto_stint_{partition_id}_{start_ms}"),
                track_day_id: track_day_id.to_owned(),
                name: format!("Driving Stint #{stint_idx}"),
                category: "driving_stint".to_owned(),
                color: "#22c55e".to_owned(),
                start_utc_ms: start_ms,
                end_utc_ms: Some(end_ms),
                author: "daqcore".to_owned(),
                source: "auto".to_owned(),
                notes: format!(
                    "Auto-detected motion stint ({:.1}s, peak {:.1} m/s)",
                    duration_ms as f64 / 1000.0,
                    peak_mps
                ),
                updated_at_ms,
                deleted: false,
            });
        }
    };

    for &(utc_ms, speed_mps) in speed_samples {
        if !speed_mps.is_finite() {
            continue;
        }
        let speed_abs = speed_mps.abs();
        match candidate_start_ms {
            None => {
                if speed_abs > SPEED_THRESHOLD_MPS {
                    candidate_start_ms = Some(utc_ms);
                    last_moving_ms = utc_ms;
                    max_speed_mps = speed_abs;
                }
            }
            Some(start_ms) => {
                if speed_abs > SPEED_THRESHOLD_MPS {
                    last_moving_ms = utc_ms;
                    if speed_abs > max_speed_mps {
                        max_speed_mps = speed_abs;
                    }
                } else if utc_ms.saturating_sub(last_moving_ms) > STOP_DEBOUNCE_MS {
                    emit_if_valid(start_ms, last_moving_ms, max_speed_mps);
                    candidate_start_ms = None;
                    max_speed_mps = 0.0;
                }
            }
        }
    }

    if let Some(start_ms) = candidate_start_ms {
        emit_if_valid(start_ms, last_moving_ms, max_speed_mps);
    }

    annotations
}

/// Returns whether a `(msg_name, sig_name)` pair represents a fault or error flag signal.
pub fn is_fault_flag_signal(msg_name: &str, sig_name: &str) -> bool {
    let msg_lower = msg_name.to_ascii_lowercase();
    let sig_lower = sig_name.to_ascii_lowercase();

    // `*_fault_event` is handled separately via its (idx, state) fields
    if msg_lower.ends_with("_fault_event") {
        return false;
    }

    if msg_lower.ends_with("_fault_sync")
        || msg_lower.contains("fault")
        || msg_lower.contains("_err")
    {
        return true;
    }

    sig_lower.contains("fault")
        || sig_lower.contains("error")
        || sig_lower == "berror"
        || sig_lower.ends_with("_pec")
}

/// Detect `0 -> non-zero` fault transitions for a single fault flag signal.
pub fn detect_signal_fault_transitions(
    signal_id: &str,
    samples: &[(i64, i64)],
    track_day_id: &str,
    updated_at_ms: i64,
) -> Vec<Annotation> {
    let mut annotations = Vec::new();
    let mut prev_val: Option<i64> = None;
    let mut active_start_ms: Option<i64> = None;
    let mut last_ts_ms: i64 = 0;

    for &(utc_ms, raw_int) in samples {
        last_ts_ms = utc_ms;
        match (prev_val, active_start_ms) {
            (Some(0), None) if raw_int != 0 => {
                active_start_ms = Some(utc_ms);
            }
            (_, Some(start_ms)) if raw_int == 0 => {
                annotations.push(build_fault_annotation(
                    signal_id,
                    signal_id,
                    start_ms,
                    Some(utc_ms),
                    track_day_id,
                    updated_at_ms,
                ));
                active_start_ms = None;
            }
            _ => {}
        }
        prev_val = Some(raw_int);
    }

    if let Some(start_ms) = active_start_ms {
        let end_ms = if last_ts_ms > start_ms {
            Some(last_ts_ms)
        } else {
            Some(start_ms)
        };
        annotations.push(build_fault_annotation(
            signal_id,
            signal_id,
            start_ms,
            end_ms,
            track_day_id,
            updated_at_ms,
        ));
    }

    annotations
}

pub fn build_fault_annotation(
    id_key: &str,
    display_label: &str,
    start_utc_ms: i64,
    end_utc_ms: Option<i64>,
    track_day_id: &str,
    updated_at_ms: i64,
) -> Annotation {
    Annotation {
        id: format!("auto_fault_{id_key}_{start_utc_ms}"),
        track_day_id: track_day_id.to_owned(),
        name: format!("Fault: {display_label}"),
        category: "fault".to_owned(),
        color: "#ef4444".to_owned(),
        start_utc_ms,
        end_utc_ms,
        author: "daqcore".to_owned(),
        source: "auto".to_owned(),
        notes: format!("Auto-detected fault transition on {display_label}"),
        updated_at_ms,
        deleted: false,
    }
}

/// Extract a unified vehicle ground speed series `(utc_ms, speed_mps)` from decoded messages.
///
/// Prefers direct ground/wheel speed signals (`gps_speed_heading.gps_speed` or any `m/s` speed
/// signal), and falls back to `motor_speeds` (`rpm`) if no `m/s` speed signal was recorded.
pub fn extract_speed_samples(
    messages: &[(i64, &crate::log_parse::parse::ParsedMessage)],
) -> Vec<(i64, f64)> {
    let mut direct_mps = Vec::new();
    let mut fallback_rpm = Vec::new();

    for &(utc_ms, msg) in messages {
        let msg_name = msg.decoded.name();
        if msg_name == "gps_speed_heading" {
            if let Some(sig) = msg.decoded.signal("gps_speed") {
                direct_mps.push((utc_ms, sig.physical()));
            }
        } else if msg_name.contains("speed") && msg_name != "motor_speeds" {
            for (sig_name, sig) in msg.decoded.iter() {
                if sig_name.contains("speed") || sig_name.contains("mps") {
                    direct_mps.push((utc_ms, sig.physical()));
                }
            }
        } else if msg_name == "motor_speeds" {
            let mut sum_rpm = 0.0;
            let mut count = 0usize;
            for (_, sig) in msg.decoded.iter() {
                let v = sig.physical();
                if v.is_finite() {
                    sum_rpm += v.abs();
                    count += 1;
                }
            }
            if count > 0 {
                let avg_rpm = sum_rpm / (count as f64);
                fallback_rpm.push((utc_ms, avg_rpm / MOTOR_RPM_PER_MPS));
            }
        }
    }

    if !direct_mps.is_empty() {
        direct_mps
    } else {
        fallback_rpm
    }
}

/// Scan decoded messages for immediate `*_fault_event` CAN messages (`idx`, `val`, `state`).
pub fn detect_fault_event_messages(
    messages: &[(i64, &crate::log_parse::parse::ParsedMessage)],
    track_day_id: &str,
    updated_at_ms: i64,
) -> Vec<Annotation> {
    let mut annotations = Vec::new();
    let mut prev_state_by_fault: std::collections::HashMap<String, i128> =
        std::collections::HashMap::new();

    for &(utc_ms, msg) in messages {
        let msg_name = msg.decoded.name();
        if !msg_name.to_ascii_lowercase().ends_with("_fault_event") {
            continue;
        }
        let Some(idx_sig) = msg.decoded.signal("idx") else {
            continue;
        };
        let Some(state_sig) = msg.decoded.signal("state") else {
            continue;
        };

        let fault_label = idx_sig
            .enum_label()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("idx_{}", idx_sig.int_rounded()));
        let key = storage::format_signal_id(msg_name, &fault_label);
        let state = state_sig.int_rounded();
        let prev = prev_state_by_fault.insert(key.clone(), state).unwrap_or(0);

        if prev == 0 && state != 0 {
            annotations.push(build_fault_annotation(
                &key,
                &fault_label,
                utc_ms,
                None,
                track_day_id,
                updated_at_ms,
            ));
        }
    }

    annotations
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_motion_stint_detector_debounce_and_min_duration() {
        // 1. Short spike (3s > 1.0 m/s) -> ignored (< 5.0s)
        // 2. Long stop (15s at 0.0 m/s)
        // 3. Real stint: 6s at 8.0 m/s, brief 4s stop (< 10s debounce), 6s at 12.0 m/s -> merged into 1 stint (16s total)
        let samples = vec![
            (0, 0.0),
            (1_000, 2.5),
            (4_000, 2.5),
            (5_000, 0.0),
            (16_000, 0.0),
            // Real stint starts at 20_000 ms
            (20_000, 8.0),
            (26_000, 8.5),
            // Brief stop from 27_000 to 30_000 (4s <= 10s debounce)
            (27_000, 0.2),
            (30_000, 0.0),
            // Resume moving until 36_000 ms
            (31_000, 12.0),
            (36_000, 10.0),
            // Stop for > 10s
            (37_000, 0.0),
            (48_000, 0.0),
        ];

        let stints = detect_motion_stints(&samples, "epoch_1", "2026-11-12", 100);
        assert_eq!(stints.len(), 1);
        assert_eq!(stints[0].category, "driving_stint");
        assert_eq!(stints[0].start_utc_ms, 20_000);
        assert_eq!(stints[0].end_utc_ms, Some(36_000));
        assert_eq!(stints[0].id, "auto_stint_epoch_1_20000");
    }

    #[test]
    fn test_fault_transition_detector() {
        let samples = vec![
            (1_000, 0),
            (2_000, 0),
            (3_000, 1),
            (4_000, 1),
            (5_000, 0),
            (6_000, 0),
            (7_000, 1),
            (8_000, 1),
        ];

        let faults = detect_signal_fault_transitions(
            "main_module_fault_sync.SDC1_IMD",
            &samples,
            "2026-11-12",
            100,
        );
        assert_eq!(faults.len(), 2);
        assert_eq!(faults[0].category, "fault");
        assert_eq!(faults[0].start_utc_ms, 3_000);
        assert_eq!(faults[0].end_utc_ms, Some(5_000));
        assert_eq!(faults[1].start_utc_ms, 7_000);
        assert_eq!(faults[1].end_utc_ms, Some(8_000));
    }
}
