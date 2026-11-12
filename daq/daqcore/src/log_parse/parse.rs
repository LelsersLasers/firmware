use crate::{frame, log_parse, superdbc, time};
use sha2::Digest as _;
use std::{fs, io, mem, path, time as std_time};

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
    if !(2020..=2100).contains(&full_year) {
        return None;
    }

    let naive_dt = chrono::NaiveDate::from_ymd_opt(full_year, month, day)?
        .and_hms_opt(hour, minute, second)?;
    Some(naive_dt.and_utc().timestamp_millis())
}

fn extract_file_mtime_ms(path: &path::Path) -> i64 {
    fs::metadata(path)
        .ok()
        .and_then(|meta| meta.modified().ok())
        .and_then(|mtime| mtime.duration_since(std_time::UNIX_EPOCH).ok())
        .map(|dur| dur.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_else(|| time::Time::now().unix_millis())
}

fn sha256_hex_bytes(bytes: &[u8]) -> String {
    let digest = sha2::Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

pub fn parse_log_files(
    in_folder: &path::Path,
    database: &superdbc::database::Database,
) -> io::Result<ParsedLogSet> {
    let mut files = Vec::new();
    let mut messages = Vec::new();

    let entries = fs::read_dir(in_folder)?;
    let mut file_paths = entries
        .collect::<io::Result<Vec<_>>>()?
        .into_iter()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("log")
        })
        .collect::<Vec<_>>();
    file_paths.sort();

    for path in file_paths {
        log::info!("Parsing log file: {}", path.display());
        let file_idx = files.len();
        let (file_meta, parsed) = parse_log_file(&path, file_idx, database)?;
        files.push(file_meta);
        messages.extend(parsed);
    }

    Ok(ParsedLogSet { files, messages })
}

fn parse_log_file(
    in_file: &path::Path,
    source_file_idx: usize,
    database: &superdbc::database::Database,
) -> io::Result<(SourceLogFile, Vec<ParsedMessage>)> {
    let mut content = fs::read(in_file)?;
    let sha256_hex = sha256_hex_bytes(&content);
    let file_name = in_file
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown.log")
        .to_owned();
    let rtc_timestamp_ms = parse_rtc_filename_ms(&file_name);
    let mtime_unix_ms = extract_file_mtime_ms(in_file);

    let file_meta = SourceLogFile {
        file_name,
        sha256_hex,
        rtc_timestamp_ms,
        mtime_unix_ms,
    };

    let frame_size = mem::size_of::<RawFrame>();

    // add padding zeroes if content length is not multiple of raw frame size
    let mut added_padding = false;
    if !content.len().is_multiple_of(frame_size) {
        log::warn!(
            "Log file {} has length {} which is not a multiple of frame size {}. Possibly due to outdated log format.",
            in_file.display(),
            content.len(),
            frame_size
        );
        let padding = frame_size - content.len() % frame_size;
        content.resize(content.len() + padding, 0);
        added_padding = true;
    }

    let frames: Vec<RawFrame> = content
        .chunks_exact(frame_size)
        .map(bytemuck::pod_read_unaligned)
        .collect();
    let mut parsed = Vec::with_capacity(frames.len());
    let mut unknown_bus_count = 0usize;
    let mut undecoded_count = 0usize;

    for (i, frame) in frames.iter().enumerate() {
        if added_padding && i == frames.len() - 1 {
            log::info!(
                "Skipping last frame in {} due to padding",
                in_file.display()
            );
            break;
        }

        let identity = frame.can_identity();
        let bus_id = frame.bus_id();

        let Some(bus) = database.bus_by_id(bus_id) else {
            unknown_bus_count += 1;
            log::debug!("Unknown bus {bus_id} in {}", in_file.display());
            continue;
        };

        if let Ok(decoded) = bus.decode(identity, &frame.data) {
            parsed.push(ParsedMessage {
                timestamp: frame.ticks_ms,
                decoded,
                bus_name: bus.name().to_owned(),
                source_file_idx,
            });
        } else {
            undecoded_count += 1;
            log::debug!(
                "Failed to decode message at {} ms with CAN ID {:X} and data {:?} on bus {}",
                frame.ticks_ms,
                identity,
                frame.data,
                bus_id
            );
        }
    }

    if unknown_bus_count > 0 || undecoded_count > 0 {
        log::warn!(
            "File {}: decoded {} frames ({} unknown-bus frames, {} undecoded frames skipped)",
            in_file.display(),
            parsed.len(),
            unknown_bus_count,
            undecoded_count
        );
    }

    Ok((file_meta, parsed))
}

fn split_monotonic_segments(messages: Vec<ParsedMessage>) -> Vec<Vec<ParsedMessage>> {
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
                // The boot ended right as log-0.log rotated to an empty 0-byte RTC file;
                // attribute the final message's file index to that RTC file so Tier 2
                // correlation anchors the end of the segment to its RTC timestamp.
                if let Some(last_msg) = seg.last_mut() {
                    last_msg.source_file_idx = boot.first_file_idx;
                }
                chunks.push(seg);
            } else {
                seg.extend(boot.messages);
                // Split further only if there was an internal tick reset inside the RTC group
                chunks.extend(split_monotonic_segments(seg));
            }
        }
    } else {
        // Fallback when counts differ: match greedily by tick continuity
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
            .unwrap()
            .and_hms_opt(14, 5, 30)
            .unwrap()
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
