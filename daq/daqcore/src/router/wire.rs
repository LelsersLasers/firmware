//! Compact binary wire codec for streaming `SignalSeries` tiles and `PartitionPreloadPayload`
//! between `daqcloud` and client viewers (`daqweb` and `daqapp`).

use crate::{
    router::types::{EnvelopePoint, SignalSeries, StateInterval},
    storage::{SignalId, SignalManifestEntry},
};
use std::{collections::HashMap, sync::Arc};

pub const TILE_WIRE_MAGIC: &[u8; 5] = b"DAQT\x01";
pub const PRELOAD_WIRE_MAGIC: &[u8; 5] = b"DAQP\x01";

const SERIES_TAG_EXACT: u8 = 0;
const SERIES_TAG_ENVELOPE: u8 = 1;

/// Preloaded resident partition data (`overview_1hz`, `activity_1hz`, `states`, and `signals`)
/// streamed over `GET /api/v1/partitions/{partition_id}/preload`.
#[derive(Debug, Clone)]
pub struct PartitionPreloadPayload {
    pub partition_id: String,
    pub overview_1hz: HashMap<SignalId, Arc<SignalSeries>>,
    pub activity_1hz: Vec<(i64, u32)>,
    pub states: HashMap<SignalId, Arc<Vec<StateInterval>>>,
    pub signals: Vec<SignalManifestEntry>,
}

/// Encode a single [`SignalSeries`] into a standalone `DAQT\x01` binary payload.
pub fn encode_tile_payload(series: &SignalSeries) -> Vec<u8> {
    let mut out = Vec::with_capacity(64 + series.estimated_bytes());
    out.extend_from_slice(TILE_WIRE_MAGIC);
    write_series_body(&mut out, series);
    out
}

/// Decode a standalone `DAQT\x01` binary payload into a [`SignalSeries`].
pub fn decode_tile_payload(bytes: &[u8]) -> Result<SignalSeries, String> {
    if bytes.len() < TILE_WIRE_MAGIC.len() || &bytes[..TILE_WIRE_MAGIC.len()] != TILE_WIRE_MAGIC {
        return Err("invalid DAQT tile payload magic header".to_owned());
    }
    let mut cursor = TILE_WIRE_MAGIC.len();
    read_series_body(bytes, &mut cursor)
}

/// Encode a [`PartitionPreloadPayload`] into a compact `DAQP\x01` binary payload.
pub fn encode_preload_payload(payload: &PartitionPreloadPayload) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(4096);
    out.extend_from_slice(PRELOAD_WIRE_MAGIC);
    write_string(&mut out, &payload.partition_id);

    // 1. overview_1hz
    write_u32(&mut out, payload.overview_1hz.len() as u32);
    for (sig_id, series) in &payload.overview_1hz {
        write_string(&mut out, sig_id);
        write_series_body(&mut out, series);
    }

    // 2. activity_1hz
    write_u32(&mut out, payload.activity_1hz.len() as u32);
    for &(bucket_ms, count) in &payload.activity_1hz {
        write_i64(&mut out, bucket_ms);
        write_u32(&mut out, count);
    }

    // 3. states
    write_u32(&mut out, payload.states.len() as u32);
    for (sig_id, intervals) in &payload.states {
        write_string(&mut out, sig_id);
        write_u32(&mut out, intervals.len() as u32);
        for iv in intervals.iter() {
            write_i64(&mut out, iv.start_utc_ms);
            write_i64(&mut out, iv.end_utc_ms);
            write_i64(&mut out, iv.raw_int);
            write_string(&mut out, &iv.label);
        }
    }

    // 4. signals manifest metadata (JSON array block)
    let signals_json = serde_json::to_vec(&payload.signals).map_err(|e| e.to_string())?;
    write_bytes(&mut out, &signals_json);

    Ok(out)
}

/// Decode a `DAQP\x01` binary payload into a [`PartitionPreloadPayload`].
pub fn decode_preload_payload(bytes: &[u8]) -> Result<PartitionPreloadPayload, String> {
    if bytes.len() < PRELOAD_WIRE_MAGIC.len()
        || &bytes[..PRELOAD_WIRE_MAGIC.len()] != PRELOAD_WIRE_MAGIC
    {
        return Err("invalid DAQP preload payload magic header".to_owned());
    }
    let mut cursor = PRELOAD_WIRE_MAGIC.len();
    let partition_id = read_string(bytes, &mut cursor)?;

    // 1. overview_1hz
    let ov_count = read_u32(bytes, &mut cursor)? as usize;
    let mut overview_1hz = HashMap::with_capacity(ov_count.min(4096));
    for _ in 0..ov_count {
        let sig_id = read_string(bytes, &mut cursor)?;
        let series = read_series_body(bytes, &mut cursor)?;
        overview_1hz.insert(sig_id, Arc::new(series));
    }

    // 2. activity_1hz
    let act_count = read_u32(bytes, &mut cursor)? as usize;
    let mut activity_1hz = Vec::with_capacity(act_count.min(65536));
    for _ in 0..act_count {
        let bucket_ms = read_i64(bytes, &mut cursor)?;
        let count = read_u32(bytes, &mut cursor)?;
        activity_1hz.push((bucket_ms, count));
    }

    // 3. states
    let states_count = read_u32(bytes, &mut cursor)? as usize;
    let mut states = HashMap::with_capacity(states_count.min(4096));
    for _ in 0..states_count {
        let sig_id = read_string(bytes, &mut cursor)?;
        let iv_count = read_u32(bytes, &mut cursor)? as usize;
        let mut intervals = Vec::with_capacity(iv_count.min(65536));
        for _ in 0..iv_count {
            let start_utc_ms = read_i64(bytes, &mut cursor)?;
            let end_utc_ms = read_i64(bytes, &mut cursor)?;
            let raw_int = read_i64(bytes, &mut cursor)?;
            let label = read_string(bytes, &mut cursor)?;
            intervals.push(StateInterval {
                start_utc_ms,
                end_utc_ms,
                raw_int,
                label,
            });
        }
        states.insert(sig_id, Arc::new(intervals));
    }

    // 4. signals manifest metadata
    let signals_json = read_bytes(bytes, &mut cursor)?;
    let signals: Vec<SignalManifestEntry> =
        serde_json::from_slice(signals_json).map_err(|e| e.to_string())?;

    Ok(PartitionPreloadPayload {
        partition_id,
        overview_1hz,
        activity_1hz,
        states,
        signals,
    })
}

fn write_u8(out: &mut Vec<u8>, v: u8) {
    out.push(v);
}

fn write_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn write_i64(out: &mut Vec<u8>, v: i64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn write_bytes(out: &mut Vec<u8>, slice: &[u8]) {
    write_u32(out, slice.len() as u32);
    out.extend_from_slice(slice);
}

fn write_string(out: &mut Vec<u8>, s: &str) {
    write_bytes(out, s.as_bytes());
}

fn write_series_body(out: &mut Vec<u8>, series: &SignalSeries) {
    match series {
        SignalSeries::Exact {
            timestamps_ms,
            values,
        } => {
            let n = timestamps_ms.len().min(values.len());
            write_u8(out, SERIES_TAG_EXACT);
            write_u32(out, n as u32);
            if cfg!(target_endian = "little") {
                out.extend_from_slice(bytemuck::cast_slice(&timestamps_ms[..n]));
                out.extend_from_slice(bytemuck::cast_slice(&values[..n]));
            } else {
                for &t in &timestamps_ms[..n] {
                    out.extend_from_slice(&t.to_le_bytes());
                }
                for &v in &values[..n] {
                    out.extend_from_slice(&v.to_le_bytes());
                }
            }
        }
        SignalSeries::Envelope(points) => {
            write_u8(out, SERIES_TAG_ENVELOPE);
            write_u32(out, points.len() as u32);
            if cfg!(target_endian = "little") {
                out.extend_from_slice(bytemuck::cast_slice(points.as_slice()));
            } else {
                for p in points {
                    out.extend_from_slice(&p.utc_ms.to_le_bytes());
                    out.extend_from_slice(&p.min.to_le_bytes());
                    out.extend_from_slice(&p.max.to_le_bytes());
                    out.extend_from_slice(&p.mean.to_le_bytes());
                }
            }
        }
    }
}

fn read_u8(bytes: &[u8], cursor: &mut usize) -> Result<u8, String> {
    let b = *bytes
        .get(*cursor)
        .ok_or_else(|| "unexpected EOF reading u8".to_owned())?;
    *cursor += 1;
    Ok(b)
}

fn read_u32(bytes: &[u8], cursor: &mut usize) -> Result<u32, String> {
    let slice = bytes
        .get(*cursor..*cursor + 4)
        .ok_or_else(|| "unexpected EOF reading u32".to_owned())?;
    *cursor += 4;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn read_i64(bytes: &[u8], cursor: &mut usize) -> Result<i64, String> {
    let slice = bytes
        .get(*cursor..*cursor + 8)
        .ok_or_else(|| "unexpected EOF reading i64".to_owned())?;
    *cursor += 8;
    let arr: [u8; 8] = slice.try_into().map_err(|_| "slice len".to_owned())?;
    Ok(i64::from_le_bytes(arr))
}

fn read_bytes<'a>(bytes: &'a [u8], cursor: &mut usize) -> Result<&'a [u8], String> {
    let len = read_u32(bytes, cursor)? as usize;
    let slice = bytes
        .get(*cursor..*cursor + len)
        .ok_or_else(|| "unexpected EOF reading byte slice".to_owned())?;
    *cursor += len;
    Ok(slice)
}

fn read_string(bytes: &[u8], cursor: &mut usize) -> Result<String, String> {
    let slice = read_bytes(bytes, cursor)?;
    std::str::from_utf8(slice)
        .map(str::to_owned)
        .map_err(|e| format!("invalid UTF-8 in wire string: {e}"))
}

fn read_series_body(bytes: &[u8], cursor: &mut usize) -> Result<SignalSeries, String> {
    let tag = read_u8(bytes, cursor)?;
    let n = read_u32(bytes, cursor)? as usize;
    match tag {
        SERIES_TAG_EXACT => {
            let ts_bytes_len = n
                .checked_mul(8)
                .ok_or_else(|| "overflow in Exact series length".to_owned())?;
            let ts_slice = bytes
                .get(*cursor..*cursor + ts_bytes_len)
                .ok_or_else(|| "unexpected EOF reading Exact timestamps".to_owned())?;
            *cursor += ts_bytes_len;
            let val_slice = bytes
                .get(*cursor..*cursor + ts_bytes_len)
                .ok_or_else(|| "unexpected EOF reading Exact values".to_owned())?;
            *cursor += ts_bytes_len;

            let timestamps_ms: Vec<i64> = if cfg!(target_endian = "little") {
                ts_slice
                    .chunks_exact(std::mem::size_of::<i64>())
                    .map(bytemuck::pod_read_unaligned)
                    .collect()
            } else {
                ts_slice
                    .chunks_exact(8)
                    .map(|c| i64::from_le_bytes(c.try_into().unwrap()))
                    .collect()
            };
            let values: Vec<f64> = if cfg!(target_endian = "little") {
                val_slice
                    .chunks_exact(std::mem::size_of::<f64>())
                    .map(bytemuck::pod_read_unaligned)
                    .collect()
            } else {
                val_slice
                    .chunks_exact(8)
                    .map(|c| f64::from_le_bytes(c.try_into().unwrap()))
                    .collect()
            };
            Ok(SignalSeries::Exact {
                timestamps_ms,
                values,
            })
        }
        SERIES_TAG_ENVELOPE => {
            let env_bytes_len = n
                .checked_mul(std::mem::size_of::<EnvelopePoint>())
                .ok_or_else(|| "overflow in Envelope series length".to_owned())?;
            let env_slice = bytes
                .get(*cursor..*cursor + env_bytes_len)
                .ok_or_else(|| "unexpected EOF reading Envelope points".to_owned())?;
            *cursor += env_bytes_len;

            let points: Vec<EnvelopePoint> = if cfg!(target_endian = "little") {
                env_slice
                    .chunks_exact(std::mem::size_of::<EnvelopePoint>())
                    .map(bytemuck::pod_read_unaligned)
                    .collect()
            } else {
                env_slice
                    .chunks_exact(32)
                    .map(|c| EnvelopePoint {
                        utc_ms: i64::from_le_bytes(c[0..8].try_into().unwrap()),
                        min: f64::from_le_bytes(c[8..16].try_into().unwrap()),
                        max: f64::from_le_bytes(c[16..24].try_into().unwrap()),
                        mean: f64::from_le_bytes(c[24..32].try_into().unwrap()),
                    })
                    .collect()
            };
            Ok(SignalSeries::Envelope(points))
        }
        other => Err(format!("unknown SignalSeries wire tag {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_and_preload_wire_codec_round_trips_exact_and_envelope_series() {
        let exact = SignalSeries::Exact {
            timestamps_ms: vec![1_000, 1_010, 1_020],
            values: vec![12.5, -3.25, 42.0],
        };
        let encoded_exact = encode_tile_payload(&exact);
        let decoded_exact = decode_tile_payload(&encoded_exact).expect("decode exact tile");
        assert_eq!(decoded_exact, exact);

        let env = SignalSeries::Envelope(vec![
            EnvelopePoint {
                utc_ms: 1_000,
                min: -1.0,
                max: 2.0,
                mean: 0.5,
            },
            EnvelopePoint {
                utc_ms: 2_000,
                min: 0.0,
                max: 4.0,
                mean: 2.0,
            },
        ]);
        let encoded_env = encode_tile_payload(&env);
        let decoded_env = decode_tile_payload(&encoded_env).expect("decode envelope tile");
        assert_eq!(decoded_env, env);

        let mut overview_1hz = HashMap::new();
        overview_1hz.insert("IMU_acceleration.X_axis".to_owned(), Arc::new(env.clone()));
        let mut states = HashMap::new();
        states.insert(
            "main_hb.car_state".to_owned(),
            Arc::new(vec![StateInterval {
                start_utc_ms: 1_000,
                end_utc_ms: 2_000,
                raw_int: 5,
                label: "ready2drive".to_owned(),
            }]),
        );
        let preload = PartitionPreloadPayload {
            partition_id: "epoch_123".to_owned(),
            overview_1hz,
            activity_1hz: vec![(1_000, 100), (2_000, 150)],
            states,
            signals: vec![SignalManifestEntry {
                signal_id: "IMU_acceleration.X_axis".to_owned(),
                unit: "g".to_owned(),
                is_discrete: false,
                bus_name: "VCAN".to_owned(),
                bus_id: 0,
                can_id: 100,
                transmitter: "MAIN".to_owned(),
                nominal_period_ms: Some(10),
                description: "Longitudinal G".to_owned(),
                sample_count: 250,
                min_utc_ms: 1_000,
                max_utc_ms: 2_990,
                min_val: -1.0,
                max_val: 4.0,
            }],
        };
        let encoded_preload = encode_preload_payload(&preload).expect("encode preload");
        let decoded_preload = decode_preload_payload(&encoded_preload).expect("decode preload");
        assert_eq!(decoded_preload.partition_id, "epoch_123");
        assert_eq!(decoded_preload.activity_1hz, preload.activity_1hz);
        assert_eq!(decoded_preload.signals, preload.signals);
        assert_eq!(
            decoded_preload
                .overview_1hz
                .get("IMU_acceleration.X_axis")
                .map(|v| v.as_ref()),
            Some(&env)
        );
        assert_eq!(
            decoded_preload
                .states
                .get("main_hb.car_state")
                .map(|v| v.as_slice()),
            preload
                .states
                .get("main_hb.car_state")
                .map(|v| v.as_slice())
        );
    }
}
