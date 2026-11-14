use crate::{
    frame::ParsedFrame,
    router::types::{
        BUCKET_1HZ_MS, BUCKET_10HZ_MS, Resolution, SignalSeries, StateInterval,
        build_envelope_buckets,
    },
    storage::{self, SignalId},
    superdbc::message::MessageKey,
    time::Time,
};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    ops,
};

/// Default maximum samples retained per signal in [`LiveColumnBuffer`] (~1 hour at 100 Hz).
pub const DEFAULT_MAX_SAMPLES_PER_SIGNAL: usize = 360_000;

/// Default maximum raw `ParsedFrame`s retained for packet-level CAN inspector widgets.
pub const DEFAULT_MAX_INSPECTOR_FRAMES: usize = 60_000;

/// Columnar ring buffer for provisional live CAN traffic.
///
/// Stores decoded signal samples in per-signal `VecDeque<(i64, f64)>` columns and
/// discrete enum transitions in `VecDeque<StateInterval>` columns for fast plotting,
/// plus a latest-frame map and bounded frame ring for CAN packet inspector widgets.
pub struct LiveColumnBuffer {
    signal_columns: HashMap<SignalId, VecDeque<(i64, f64)>>,
    state_columns: HashMap<SignalId, VecDeque<StateInterval>>,
    activity_1hz: BTreeMap<i64, u32>,
    latest_frames: HashMap<MessageKey, ParsedFrame>,
    inspector_frames: Vec<ParsedFrame>,
    first_inspector_index: usize,
    min_utc_ms: Option<i64>,
    max_utc_ms: Option<i64>,
    total_samples: usize,
    max_samples_per_signal: usize,
    max_inspector_frames: usize,
}

impl Default for LiveColumnBuffer {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_SAMPLES_PER_SIGNAL, DEFAULT_MAX_INSPECTOR_FRAMES)
    }
}

impl LiveColumnBuffer {
    pub fn new(max_samples_per_signal: usize, max_inspector_frames: usize) -> Self {
        Self {
            signal_columns: HashMap::new(),
            state_columns: HashMap::new(),
            activity_1hz: BTreeMap::new(),
            latest_frames: HashMap::new(),
            inspector_frames: Vec::new(),
            first_inspector_index: 0,
            min_utc_ms: None,
            max_utc_ms: None,
            total_samples: 0,
            max_samples_per_signal: max_samples_per_signal.max(64),
            max_inspector_frames: max_inspector_frames.max(64),
        }
    }

    pub fn clear(&mut self) {
        self.signal_columns.clear();
        self.state_columns.clear();
        self.activity_1hz.clear();
        self.latest_frames.clear();
        self.inspector_frames.clear();
        self.first_inspector_index = 0;
        self.min_utc_ms = None;
        self.max_utc_ms = None;
        self.total_samples = 0;
    }

    pub fn is_empty(&self) -> bool {
        self.min_utc_ms.is_none()
    }

    /// Number of retained inspector frames.
    pub fn len(&self) -> usize {
        self.inspector_frames
            .len()
            .saturating_sub(self.first_inspector_index)
    }

    pub fn total_signal_samples(&self) -> usize {
        self.total_samples
    }

    pub fn signal_count(&self) -> usize {
        self.signal_columns.len()
    }

    pub fn time_bounds_ms(&self) -> Option<(i64, i64)> {
        Some((self.min_utc_ms?, self.max_utc_ms?))
    }

    pub fn time_span(&self) -> Option<(Time, Time)> {
        let (min_ms, max_ms) = self.time_bounds_ms()?;
        Some((
            Time::from_unix_millis(min_ms),
            Time::from_unix_millis(max_ms),
        ))
    }

    pub fn latest(&self, key: MessageKey) -> Option<&ParsedFrame> {
        self.latest_frames.get(&key)
    }

    pub fn latest_map(&self) -> &HashMap<MessageKey, ParsedFrame> {
        &self.latest_frames
    }

    /// Push a single live [`ParsedFrame`] into the columnar ring buffer.
    pub fn push(&mut self, frame: ParsedFrame) {
        let utc_ms = frame.timestamp.unix_millis();
        self.min_utc_ms = Some(self.min_utc_ms.map_or(utc_ms, |prev| prev.min(utc_ms)));
        self.max_utc_ms = Some(self.max_utc_ms.map_or(utc_ms, |prev| prev.max(utc_ms)));

        if let Some(decoded) = &frame.decoded {
            let bucket_ms = utc_ms.div_euclid(BUCKET_1HZ_MS) * BUCKET_1HZ_MS;
            let sig_count = (decoded.iter().count() as u32).max(1);
            let entry = self.activity_1hz.entry(bucket_ms).or_insert(0);
            *entry = entry.saturating_add(sig_count);

            let msg_name = decoded.name();
            for (sig_name, sig_value) in decoded.iter() {
                let signal_id = storage::format_signal_id(msg_name, sig_name);
                let physical = sig_value.physical();

                let col = self.signal_columns.entry(signal_id.clone()).or_default();
                let in_order = col.back().is_none_or(|&(last_ms, _)| utc_ms >= last_ms);
                if in_order {
                    col.push_back((utc_ms, physical));
                } else {
                    let insert_idx = col.partition_point(|&(t, _)| t <= utc_ms);
                    col.insert(insert_idx, (utc_ms, physical));
                }
                self.total_samples = self.total_samples.saturating_add(1);

                if col.len() > self.max_samples_per_signal {
                    col.pop_front();
                    self.total_samples = self.total_samples.saturating_sub(1);
                }

                if let Some(label) = sig_value.enum_label() {
                    let raw_int = sig_value
                        .int_rounded()
                        .clamp(i64::MIN as i128, i64::MAX as i128)
                        as i64;
                    let state_col = self.state_columns.entry(signal_id).or_default();
                    match state_col.back_mut() {
                        Some(last) if utc_ms >= last.start_utc_ms && last.raw_int == raw_int => {
                            last.end_utc_ms = last.end_utc_ms.max(utc_ms);
                        }
                        Some(last) if utc_ms >= last.end_utc_ms => {
                            last.end_utc_ms = utc_ms.max(last.end_utc_ms);
                            state_col.push_back(StateInterval {
                                start_utc_ms: utc_ms,
                                end_utc_ms: utc_ms,
                                raw_int,
                                label: label.to_owned(),
                            });
                        }
                        None => {
                            state_col.push_back(StateInterval {
                                start_utc_ms: utc_ms,
                                end_utc_ms: utc_ms,
                                raw_int,
                                label: label.to_owned(),
                            });
                        }
                        Some(_) => {}
                    }
                }
            }
        }

        let key = frame.key();
        let should_update_latest = self
            .latest_frames
            .get(&key)
            .is_none_or(|existing| frame.timestamp >= existing.timestamp);
        if should_update_latest {
            self.latest_frames.insert(key, frame.clone());
        }

        let active_inspector = &self.inspector_frames[self.first_inspector_index..];
        let frame_in_order = active_inspector
            .last()
            .is_none_or(|last| frame.timestamp >= last.timestamp);
        if frame_in_order {
            self.inspector_frames.push(frame);
        } else {
            let offset = active_inspector.partition_point(|f| f.timestamp <= frame.timestamp);
            self.inspector_frames
                .insert(self.first_inspector_index + offset, frame);
        }

        if self.len() > self.max_inspector_frames {
            let excess = self.len() - self.max_inspector_frames;
            self.first_inspector_index += excess;
            self.compact_inspector_if_needed();
        }
    }

    pub fn push_batch(&mut self, mut batch: Vec<ParsedFrame>) {
        if batch.is_empty() {
            return;
        }
        batch.sort_by_key(|f| f.timestamp);
        for frame in batch {
            self.push(frame);
        }
    }

    /// Evict all samples and inspector frames strictly older than `floor_utc_ms`.
    pub fn evict_before(&mut self, floor_utc_ms: i64) -> usize {
        let mut evicted_samples = 0usize;
        let mut new_min: Option<i64> = None;

        for col in self.signal_columns.values_mut() {
            let drop_count = col.partition_point(|&(t, _)| t < floor_utc_ms);
            if drop_count > 0 {
                col.drain(..drop_count);
                evicted_samples += drop_count;
                self.total_samples = self.total_samples.saturating_sub(drop_count);
            }
            if let Some(&(first_t, _)) = col.front() {
                new_min = Some(new_min.map_or(first_t, |m| m.min(first_t)));
            }
        }

        for state_col in self.state_columns.values_mut() {
            while state_col
                .front()
                .is_some_and(|interval| interval.end_utc_ms < floor_utc_ms)
            {
                state_col.pop_front();
            }
            if let Some(front) = state_col.front_mut()
                && front.start_utc_ms < floor_utc_ms
            {
                front.start_utc_ms = floor_utc_ms;
            }
        }

        let floor_time = Time::from_unix_millis(floor_utc_ms);
        let active = &self.inspector_frames[self.first_inspector_index..];
        let removed_frames = active.partition_point(|f| f.timestamp < floor_time);
        self.first_inspector_index += removed_frames;
        if removed_frames > 0 {
            self.latest_frames
                .retain(|_, frame| frame.timestamp >= floor_time);
        }
        self.compact_inspector_if_needed();

        if let Some(first_frame) = self.active_inspector_frames().first() {
            let ft = first_frame.timestamp.unix_millis();
            new_min = Some(new_min.map_or(ft, |m| m.min(ft)));
        }
        let floor_bucket_ms = floor_utc_ms.div_euclid(BUCKET_1HZ_MS) * BUCKET_1HZ_MS;
        self.activity_1hz = self.activity_1hz.split_off(&floor_bucket_ms);
        self.min_utc_ms = new_min;
        if self.min_utc_ms.is_none() {
            self.max_utc_ms = None;
            self.activity_1hz.clear();
        }

        evicted_samples
    }

    /// Return sorted `(bucket_utc_ms, sample_count)` 1Hz activity buckets overlapping `range`.
    pub fn activity_1hz_in_range(&self, range: ops::RangeInclusive<i64>) -> Vec<(i64, u32)> {
        if range.is_empty() {
            return Vec::new();
        }
        let lo_bucket = range
            .start()
            .div_euclid(BUCKET_1HZ_MS)
            .saturating_mul(BUCKET_1HZ_MS);
        let hi_bucket = range
            .end()
            .div_euclid(BUCKET_1HZ_MS)
            .saturating_mul(BUCKET_1HZ_MS);
        self.activity_1hz
            .range(lo_bucket..=hi_bucket)
            .map(|(&b, &c)| (b, c))
            .collect()
    }

    /// Iterate over all signal IDs currently retained in the live buffer.
    pub fn signal_ids(&self) -> impl Iterator<Item = &SignalId> {
        self.signal_columns.keys()
    }

    fn compact_inspector_if_needed(&mut self) {
        let remaining = self.len();
        if self.first_inspector_index > 0 && self.first_inspector_index >= remaining.max(1024) {
            self.inspector_frames.drain(..self.first_inspector_index);
            self.first_inspector_index = 0;
        }
    }

    fn active_inspector_frames(&self) -> &[ParsedFrame] {
        &self.inspector_frames[self.first_inspector_index..]
    }

    /// Return retained inspector frames in `range` for packet-level CAN inspector tools.
    pub fn frames_in_range(&self, range: ops::RangeInclusive<Time>) -> &[ParsedFrame] {
        let frames = self.active_inspector_frames();
        if range.is_empty() {
            return &frames[..0];
        }
        let start = frames.partition_point(|f| f.timestamp < *range.start());
        let end = frames.partition_point(|f| f.timestamp <= *range.end());
        &frames[start..end]
    }

    /// Query a signal column over `range` (`[start_utc_ms..=end_utc_ms]`) at `resolution`.
    pub fn query_signal(
        &self,
        signal_id: &str,
        range: ops::RangeInclusive<i64>,
        resolution: Resolution,
    ) -> Option<SignalSeries> {
        if range.is_empty() {
            return None;
        }
        let col = self.signal_columns.get(signal_id)?;
        let start_ms = *range.start();
        let end_ms = *range.end();

        let lo = col.partition_point(|&(t, _)| t < start_ms);
        let hi = col.partition_point(|&(t, _)| t <= end_ms);
        if lo >= hi {
            return None;
        }

        let iter = col.range(lo..hi).copied();
        match resolution {
            Resolution::FullRes => {
                let count = hi - lo;
                let mut timestamps_ms = Vec::with_capacity(count);
                let mut values = Vec::with_capacity(count);
                for (t, v) in iter {
                    timestamps_ms.push(t);
                    values.push(v);
                }
                Some(SignalSeries::Exact {
                    timestamps_ms,
                    values,
                })
            }
            Resolution::Overview10Hz => {
                let buckets = build_envelope_buckets(iter, BUCKET_10HZ_MS);
                (!buckets.is_empty()).then_some(SignalSeries::Envelope(buckets))
            }
            Resolution::Overview1Hz => {
                let buckets = build_envelope_buckets(iter, BUCKET_1HZ_MS);
                (!buckets.is_empty()).then_some(SignalSeries::Envelope(buckets))
            }
        }
    }

    /// Query discrete state intervals for `signal_id` overlapping `range`.
    pub fn query_states(
        &self,
        signal_id: &str,
        range: ops::RangeInclusive<i64>,
    ) -> Vec<StateInterval> {
        let Some(col) = self.state_columns.get(signal_id) else {
            return Vec::new();
        };
        col.iter()
            .filter_map(|interval| interval.clamped_to(&range))
            .collect()
    }

    /// Find the latest sample `(utc_ms, value)` for `signal_id` at or before `target_utc_ms`.
    pub fn latest_signal_value_at(
        &self,
        signal_id: &str,
        target_utc_ms: i64,
    ) -> Option<(i64, f64)> {
        let col = self.signal_columns.get(signal_id)?;
        let idx = col.partition_point(|&(t, _)| t <= target_utc_ms);
        let idx = idx.checked_sub(1)?;
        col.get(idx).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{FrameKind, ParsedFrame};
    use crate::superdbc::database::Database;

    fn sample_db() -> Database {
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
                  "message_name": "imu",
                  "transmitter": "MAIN_MODULE",
                  "receivers": [],
                  "length_bytes": 4,
                  "nominal_period_ms": 10,
                  "priority": 1,
                  "description": "IMU",
                  "signals": [
                    {
                      "signal_name": "accel_y_g",
                      "description": "",
                      "data_type": "int16_t",
                      "raw_type": "signed",
                      "start_bit": 0,
                      "bit_length": 16,
                      "byte_order": "little_endian",
                      "scale": 0.01,
                      "offset": 0.0,
                      "limits": null,
                      "unit": "g",
                      "choices": null,
                      "display_format": null
                    },
                    {
                      "signal_name": "mode",
                      "description": "",
                      "data_type": "uint8_t",
                      "raw_type": "unsigned",
                      "start_bit": 16,
                      "bit_length": 8,
                      "byte_order": "little_endian",
                      "scale": 1.0,
                      "offset": 0.0,
                      "limits": null,
                      "unit": "",
                      "choices": { "0": "CAL", "1": "RUN" },
                      "display_format": null
                    }
                  ]
                }
              ]
            }
          }
        }"#;
        Database::from_json(json).unwrap()
    }

    fn make_frame(db: &Database, utc_ms: i64, accel_g: f64, mode: f64) -> ParsedFrame {
        let payload = db
            .encode(0, 100, &[("accel_y_g", accel_g), ("mode", mode)])
            .unwrap();
        let decoded = db.decode(0, 100, &payload).ok();
        ParsedFrame {
            timestamp: Time::from_unix_millis(utc_ms),
            bus_id: 0,
            kind: FrameKind::Data,
            dlc: payload.len() as u8,
            identity: 100,
            raw_bytes: payload,
            decoded,
        }
    }

    #[test]
    fn live_column_buffer_queries_exact_envelopes_and_states() {
        let db = sample_db();
        let mut buf = LiveColumnBuffer::default();

        buf.push(make_frame(&db, 1000, 0.50, 0.0));
        buf.push(make_frame(&db, 1050, 1.50, 0.0));
        // Out-of-order insertion at 1020ms
        buf.push(make_frame(&db, 1020, 1.00, 0.0));
        buf.push(make_frame(&db, 1150, -0.50, 1.0));
        buf.push(make_frame(&db, 1200, -1.00, 1.0));

        let exact = buf
            .query_signal("imu.accel_y_g", 1000..=1200, Resolution::FullRes)
            .expect("series present");
        match exact {
            SignalSeries::Exact {
                timestamps_ms,
                values,
            } => {
                assert_eq!(timestamps_ms, vec![1000, 1020, 1050, 1150, 1200]);
                assert!((values[1] - 1.00).abs() < 1e-6);
            }
            other => panic!("Expected Exact, got {other:?}"),
        }

        let env_10hz = buf
            .query_signal("imu.accel_y_g", 1000..=1200, Resolution::Overview10Hz)
            .expect("10hz envelope");
        match env_10hz {
            SignalSeries::Envelope(pts) => {
                // Buckets at 1000ms, 1100ms, 1200ms
                assert_eq!(pts.len(), 3);
                assert_eq!(pts[0].utc_ms, 1000);
                assert!((pts[0].min - 0.50).abs() < 1e-6);
                assert!((pts[0].max - 1.50).abs() < 1e-6);
                assert!((pts[0].mean - 1.00).abs() < 1e-6);
            }
            other => panic!("Expected Envelope, got {other:?}"),
        }

        let states = buf.query_states("imu.mode", 1000..=1200);
        assert_eq!(states.len(), 2);
        assert_eq!(states[0].label, "CAL");
        assert_eq!(states[0].start_utc_ms, 1000);
        assert_eq!(states[0].end_utc_ms, 1150);
        assert_eq!(states[1].label, "RUN");
        assert_eq!(states[1].start_utc_ms, 1150);
        assert_eq!(states[1].end_utc_ms, 1200);

        assert_eq!(
            buf.latest_signal_value_at("imu.accel_y_g", 1060)
                .map(|(t, v)| (t, (v * 100.0).round() as i64)),
            Some((1050, 150))
        );
    }
}
