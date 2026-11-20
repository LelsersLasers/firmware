use crate::{frame, log_parse, storage, superdbc};
use std::mem;

/// Maximum valid calendar year for RTC / GPS log timestamps.
pub const MAX_VALID_LOG_YEAR: i32 = 2100;
/// Minimum valid calendar year for RTC / GPS log timestamps.
pub const MIN_VALID_LOG_YEAR: i32 = 2020;
/// Maximum plausible MCU uptime tick counter (`7 days` in milliseconds) used to filter out
/// uninitialized flash words (`0xFFFF_FFFF`) in corrupted raw `.log` sectors.
pub const MAX_PLAUSIBLE_MCU_TICKS_MS: u32 = 7 * 86_400_000;

/// Metadata and provenance captured from a single raw `.log` file on disk.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SourceLogFile {
    pub file_name: String,
    pub sha256_hex: String,
    pub rtc_timestamp_ms: Option<i64>,
    pub mtime_unix_ms: i64,
}

#[derive(Debug, Clone)]
pub struct ParsedMessage {
    pub timestamp: u32,
    pub decoded: superdbc::decode::DecodedMessage,
    pub bus_name: String,
    pub source_file_idx: usize,
}

/// Complete output of scanning and decoding a directory of raw `.log` files.
#[derive(Debug, Clone, Default)]
pub struct ParsedLogSet {
    pub files: Vec<SourceLogFile>,
    pub messages: Vec<ParsedMessage>,
}

#[repr(C)]
#[derive(bytemuck::Pod, bytemuck::Zeroable, Copy, Clone, Debug, PartialEq, Eq)]
// based on definition of timestamped_frame_t in timestamped_frame.h in firmware repo
pub struct RawFrame {
    pub ticks_ms: u32,
    pub identity: u32,
    pub data: [u8; 8],
}

impl RawFrame {
    /// Encode a classic CAN frame using the timestamped wire/log format.
    /// Return an error on invalid bus ID (only buses 0-7 are supported).
    pub fn new(
        ticks_ms: u32,
        identity: frame::CanIdentity,
        bus_id: u8,
        data: [u8; 8],
    ) -> Result<Self, String> {
        if bus_id > 7 {
            return Err(format!(
                "logging is unavailable for bus {bus_id}; only buses 0-7 are supported"
            ));
        }
        Ok(Self {
            ticks_ms,
            identity: identity | ((bus_id as u32) << log_parse::consts::BUS_ID_OFFSET),
            data,
        })
    }

    pub fn bus_id(&self) -> u8 {
        ((self.identity & log_parse::consts::BUS_ID_MASK) >> log_parse::consts::BUS_ID_OFFSET) as u8
    }

    /// Convert log fmt into standard CAN identity (i.e. remove bus ID bits)
    pub fn can_identity(&self) -> frame::CanIdentity {
        self.identity & !log_parse::consts::BUS_ID_MASK
    }
}

/// Parse an STM32 RTC filename (`log-20YY-MM-DD--HH-MM-SS.log`) into Unix UTC milliseconds.
///
/// Returns `None` for non-RTC filenames (such as `log-0.log`) or out-of-range calendar fields.
pub fn parse_rtc_filename_ms(file_name: &str) -> Option<i64> {
    let body = file_name.strip_prefix("log-20")?.strip_suffix(".log")?;
    let (date_part, time_part) = body.split_once("--")?;

    let mut date_iter = date_part.splitn(3, '-');
    let yy_str = date_iter.next()?;
    let mm_str = date_iter.next()?;
    let dd_str = date_iter.next()?;

    let mut time_iter = time_part.splitn(3, '-');
    let hh_str = time_iter.next()?;
    let min_str = time_iter.next()?;
    let ss_str = time_iter.next()?;

    if [yy_str, mm_str, dd_str, hh_str, min_str, ss_str]
        .iter()
        .any(|part| part.len() != 2 || !part.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }

    let yy: i32 = yy_str.parse().ok()?;
    let month: u32 = mm_str.parse().ok()?;
    let day: u32 = dd_str.parse().ok()?;
    let hour: u32 = hh_str.parse().ok()?;
    let minute: u32 = min_str.parse().ok()?;
    let second: u32 = ss_str.parse().ok()?;

    let full_year = 2000 + yy;
    if !(MIN_VALID_LOG_YEAR..=MAX_VALID_LOG_YEAR).contains(&full_year) {
        return None;
    }

    let naive_dt = chrono::NaiveDate::from_ymd_opt(full_year, month, day)?
        .and_hms_opt(hour, minute, second)?;
    Some(naive_dt.and_utc().timestamp_millis())
}

pub fn sha256_hex_bytes(bytes: &[u8]) -> String {
    storage::compute_sha256_hex(bytes)
}

/// Decode a raw 16-byte frame buffer (`content`) on an explicit target bus (`explicit_bus_name`)
/// or using the frame's embedded bus ID bits, filtering out uninitialized flash tick values.
pub fn parse_log_bytes_on_bus(
    content: &[u8],
    file_name: &str,
    mtime_unix_ms: i64,
    source_file_idx: usize,
    explicit_bus_name: &str,
    database: &superdbc::database::Database,
) -> (SourceLogFile, Vec<ParsedMessage>) {
    let sha256_hex = sha256_hex_bytes(content);
    let rtc_timestamp_ms = parse_rtc_filename_ms(file_name);
    let file_meta = SourceLogFile {
        file_name: file_name.to_owned(),
        sha256_hex,
        rtc_timestamp_ms,
        mtime_unix_ms,
    };

    let frame_size = mem::size_of::<RawFrame>();
    let usable_len = content.len() - (content.len() % frame_size);
    let frames: Vec<RawFrame> = content[..usable_len]
        .chunks_exact(frame_size)
        .map(bytemuck::pod_read_unaligned)
        .collect();

    let target_bus = database.bus(explicit_bus_name);
    let mut parsed = Vec::with_capacity(frames.len());

    for frame in &frames {
        if frame.ticks_ms > MAX_PLAUSIBLE_MCU_TICKS_MS {
            continue;
        }
        let identity = frame.can_identity();
        let bus_id = frame.bus_id();
        let legacy_1bit_bus = ((frame.identity >> 31) & 0x1) as u8;
        let legacy_2bit_bus = ((frame.identity >> 30) & 0x3) as u8;

        let decoded_on_bus = target_bus
            .and_then(|bus| {
                bus.decode(identity, &frame.data)
                    .ok()
                    .map(|d| (bus.name().to_owned(), d))
            })
            .or_else(|| {
                [bus_id, legacy_2bit_bus, legacy_1bit_bus]
                    .into_iter()
                    .find_map(|cand_id| {
                        let bus = database.bus_by_id(cand_id)?;
                        let decoded = bus.decode(identity, &frame.data).ok()?;
                        Some((bus.name().to_owned(), decoded))
                    })
            })
            .or_else(|| {
                database.buses().iter().find_map(|bus| {
                    bus.decode(identity, &frame.data)
                        .ok()
                        .map(|d| (bus.name().to_owned(), d))
                })
            });

        if let Some((bus_name, decoded)) = decoded_on_bus {
            parsed.push(ParsedMessage {
                timestamp: frame.ticks_ms,
                decoded,
                bus_name,
                source_file_idx,
            });
        }
    }

    (file_meta, parsed)
}

pub fn split_monotonic_segments(messages: Vec<ParsedMessage>) -> Vec<Vec<ParsedMessage>> {
    let mut chunks = Vec::new();
    let mut current_chunk = Vec::new();
    let mut last_timestamp = None;

    for msg in messages {
        if let Some(last_ts) = last_timestamp
            && (msg.timestamp < last_ts || msg.timestamp - last_ts > log_parse::consts::MAX_JUMP_MS)
            && !current_chunk.is_empty()
        {
            chunks.push(current_chunk);
            current_chunk = Vec::new();
        }
        last_timestamp = Some(msg.timestamp);
        current_chunk.push(msg);
    }

    if !current_chunk.is_empty() {
        chunks.push(current_chunk);
    }

    chunks
}

pub fn chunk_parsed(parsed: Vec<ParsedMessage>) -> Vec<Vec<ParsedMessage>> {
    let mut chunks = split_monotonic_segments(parsed);
    for chunk in &mut chunks {
        chunk.sort_by_key(|m| m.timestamp);
    }
    chunks
}

/// Stitch decoded messages into contiguous MCU boot epochs, pairing initial `log-0.log`
/// segments (`FA_OPEN_APPEND` first-minute boot logs written before RTC sync) with their
/// corresponding RTC-named continuation files (`log-20YY-MM-DD--HH-MM-SS.log`).
pub fn chunk_parsed_with_files(
    files: &[SourceLogFile],
    parsed: Vec<ParsedMessage>,
) -> Vec<Vec<ParsedMessage>> {
    let has_non_rtc_files = files.iter().any(|f| f.rtc_timestamp_ms.is_none());
    let has_rtc_files = files.iter().any(|f| f.rtc_timestamp_ms.is_some());

    if !has_non_rtc_files || !has_rtc_files {
        return chunk_parsed(parsed);
    }

    let mut non_rtc_msgs = Vec::new();
    let mut msgs_by_file: Vec<Vec<ParsedMessage>> = vec![Vec::new(); files.len()];

    for msg in parsed {
        if files
            .get(msg.source_file_idx)
            .and_then(|f| f.rtc_timestamp_ms)
            .is_some()
        {
            if let Some(bucket) = msgs_by_file.get_mut(msg.source_file_idx) {
                bucket.push(msg);
            }
        } else {
            non_rtc_msgs.push(msg);
        }
    }

    let non_rtc_segments = split_monotonic_segments(non_rtc_msgs);

    struct RtcBootGroup {
        first_file_idx: usize,
        messages: Vec<ParsedMessage>,
    }

    let mut rtc_boots: Vec<RtcBootGroup> = Vec::new();
    let mut prev_rtc_ms: Option<i64> = None;

    for (file_idx, file_meta) in files.iter().enumerate() {
        let Some(rtc_ms) = file_meta.rtc_timestamp_ms else {
            continue;
        };
        let file_msgs = mem::take(&mut msgs_by_file[file_idx]);
        let file_first_ts = file_msgs.first().map(|m| m.timestamp);

        let starts_new_boot = match rtc_boots.last() {
            None => true,
            Some(cur_boot) => {
                let rtc_gap_exceeded = prev_rtc_ms.is_some_and(|prev| {
                    rtc_ms - prev > log_parse::consts::RTC_FILE_ROTATION_MAX_GAP_MS
                });
                let tick_jump_exceeded = match (cur_boot.messages.last(), file_first_ts) {
                    (Some(last_m), Some(first_t)) => {
                        first_t < last_m.timestamp
                            || first_t - last_m.timestamp > log_parse::consts::MAX_JUMP_MS
                    }
                    _ => false,
                };
                rtc_gap_exceeded || tick_jump_exceeded
            }
        };

        if starts_new_boot {
            rtc_boots.push(RtcBootGroup {
                first_file_idx: file_idx,
                messages: file_msgs,
            });
        } else if let Some(cur_boot) = rtc_boots.last_mut() {
            cur_boot.messages.extend(file_msgs);
        }

        prev_rtc_ms = Some(rtc_ms);
    }

    let mut chunks = Vec::new();

    if non_rtc_segments.len() == rtc_boots.len() {
        for (mut seg, boot) in non_rtc_segments.into_iter().zip(rtc_boots) {
            if boot.messages.is_empty() {
                if let Some(last_msg) = seg.last_mut() {
                    last_msg.source_file_idx = boot.first_file_idx;
                }
                chunks.push(seg);
            } else {
                seg.extend(boot.messages);
                chunks.extend(split_monotonic_segments(seg));
            }
        }
    } else {
        let mut used_boots = vec![false; rtc_boots.len()];
        for mut seg in non_rtc_segments {
            let seg_last_ts = seg.last().map(|m| m.timestamp).unwrap_or(0);
            let matched_boot_idx = rtc_boots.iter().enumerate().position(|(idx, boot)| {
                !used_boots[idx]
                    && boot.messages.first().is_some_and(|first_m| {
                        first_m.timestamp >= seg_last_ts
                            && first_m.timestamp - seg_last_ts
                                <= log_parse::consts::CONTINUATION_MAX_GAP_MS
                    })
            });

            if let Some(idx) = matched_boot_idx {
                used_boots[idx] = true;
                seg.extend(mem::take(&mut rtc_boots[idx].messages));
                chunks.extend(split_monotonic_segments(seg));
            } else {
                chunks.push(seg);
            }
        }

        for (idx, boot) in rtc_boots.into_iter().enumerate() {
            if !used_boots[idx] && !boot.messages.is_empty() {
                chunks.extend(split_monotonic_segments(boot.messages));
            }
        }
    }

    for chunk in &mut chunks {
        chunk.sort_by_key(|m| m.timestamp);
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_rtc_filename_ms() {
        let ts = parse_rtc_filename_ms("log-2026-11-12--14-05-30.log").expect("valid RTC filename");
        let expected = chrono::NaiveDate::from_ymd_opt(2026, 11, 12)
            .expect("valid date")
            .and_hms_opt(14, 5, 30)
            .expect("valid time")
            .and_utc()
            .timestamp_millis();
        assert_eq!(ts, expected);

        assert_eq!(parse_rtc_filename_ms("log-0.log"), None);
        assert_eq!(parse_rtc_filename_ms("log-2000-01-01--00-00-00.log"), None);
        assert_eq!(parse_rtc_filename_ms("log-2026-13-12--14-05-30.log"), None);
        assert_eq!(
            parse_rtc_filename_ms("other-2026-11-12--14-05-30.txt"),
            None
        );
    }
}
