use crate::{superdbc, time};
use std::{fs, io, mem, path, time as std_time};

pub use daqcore::log_parse::parse::{
    ParsedLogSet, ParsedMessage, RawFrame, SourceLogFile, chunk_parsed, chunk_parsed_with_files,
    parse_log_bytes_on_bus, parse_rtc_filename_ms, sha256_hex_bytes, split_monotonic_segments,
};

fn extract_file_mtime_ms(path: &path::Path) -> i64 {
    fs::metadata(path)
        .ok()
        .and_then(|meta| meta.modified().ok())
        .and_then(|mtime| mtime.duration_since(std_time::UNIX_EPOCH).ok())
        .map(|dur| dur.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_else(|| time::Time::now().unix_millis())
}

pub fn parse_log_files(
    in_folder: &path::Path,
    database: &superdbc::database::Database,
) -> io::Result<ParsedLogSet> {
    parse_log_files_with_progress(in_folder, database, |_, _, _| {})
}

pub fn parse_log_files_with_progress<F>(
    in_folder: &path::Path,
    database: &superdbc::database::Database,
    mut on_file: F,
) -> io::Result<ParsedLogSet>
where
    F: FnMut(usize, usize, &str),
{
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

    let total_files = file_paths.len();
    for path in file_paths {
        log::info!("Parsing log file: {}", path.display());
        let file_idx = files.len();
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown.log");
        on_file(file_idx, total_files, file_name);
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

        let decoded_on_bus = database
            .bus_by_id(bus_id)
            .and_then(|bus| {
                bus.decode(identity, &frame.data)
                    .ok()
                    .map(|d| (bus.name().to_owned(), d))
            })
            .or_else(|| {
                // Fallback for historical PER26/early-PER27 `.log` files recorded when
                // `timestamped_frame_t` packed a 1-bit bus_id at bit 31 (before PR #561)
                // or a 2-bit bus_id at bits 31..30 (before PR #580).
                let legacy_1bit_bus = ((frame.identity >> 31) & 0x1) as u8;
                let legacy_2bit_bus = ((frame.identity >> 30) & 0x3) as u8;
                [legacy_1bit_bus, legacy_2bit_bus]
                    .into_iter()
                    .filter(|&alt_bus| alt_bus != bus_id)
                    .find_map(|alt_bus| {
                        let bus = database.bus_by_id(alt_bus)?;
                        let decoded = bus.decode(identity, &frame.data).ok()?;
                        Some((bus.name().to_owned(), decoded))
                    })
            });

        if let Some((bus_name, decoded)) = decoded_on_bus {
            parsed.push(ParsedMessage {
                timestamp: frame.ticks_ms,
                decoded,
                bus_name,
                source_file_idx,
            });
        } else if database.bus_by_id(bus_id).is_none() {
            unknown_bus_count += 1;
            log::debug!("Unknown bus {bus_id} in {}", in_file.display());
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
