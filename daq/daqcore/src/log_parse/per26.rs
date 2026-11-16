//! Comprehensive ingestion and corruption-filtering pipeline for all PER26 / PER25
//! track-day datasets (supporting both Format A / Format B `out_*.csv` files and
//! multi-bus `MCAN`/`VCAN`/`TCAN` raw `.log` directories).

use crate::{
    log_parse::{auto_annotate, correlate, parse},
    storage::{
        self, ImportError, ImportSummary, RawSample, Registry, SignalColumnAccumulator, SignalId,
    },
    superdbc::database::Database,
    time::Time,
};
use chrono::{NaiveDate, TimeZone as _, Utc};
use sha2::Digest as _;
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    io::{self, BufRead as _},
    path::{Path, PathBuf},
};

/// Summary of a full `PER26` multi-day dataset import.
#[derive(Debug, Clone, Default)]
pub struct Per26ImportReport {
    pub track_days_imported: usize,
    pub partitions_written: usize,
    pub partitions_skipped_existing: usize,
    pub annotations_created: usize,
}

#[derive(Clone)]
struct ResolvedSignalMeta {
    signal_id: SignalId,
    unit: String,
    description: String,
    is_discrete: bool,
    is_fault_flag: bool,
    bus_name: String,
    bus_id: u8,
    can_id: u32,
    transmitter: String,
    nominal_period_ms: Option<u32>,
    int_to_label: HashMap<i64, String>,
    label_to_int: HashMap<String, i64>,
}

fn build_signal_lookup(database: &Database) -> HashMap<(String, String), ResolvedSignalMeta> {
    let mut map = HashMap::new();
    for bus in database.buses() {
        for msg in bus.messages() {
            let msg_name = msg.name().to_owned();
            for sig in &msg.signals {
                let sig_name = sig.name().to_owned();
                let signal_id = storage::format_signal_id(&msg_name, &sig_name);
                let mut int_to_label = HashMap::new();
                let mut label_to_int = HashMap::new();
                for (raw_val, label) in sig.choices() {
                    int_to_label.insert(raw_val as i64, label.to_owned());
                    label_to_int.insert(label.to_ascii_lowercase(), raw_val as i64);
                }
                let is_fault_flag = auto_annotate::is_fault_flag_signal(&msg_name, &sig_name);
                let is_discrete = is_fault_flag
                    || !int_to_label.is_empty()
                    || (sig.bit_length() == 1
                        && (sig.scale() - 1.0).abs() < 1e-9
                        && sig.offset().abs() < 1e-9);
                map.insert(
                    (msg_name.clone(), sig_name.clone()),
                    ResolvedSignalMeta {
                        signal_id,
                        unit: sig.unit().to_owned(),
                        description: sig.description().to_owned(),
                        is_discrete,
                        is_fault_flag,
                        bus_name: bus.name().to_owned(),
                        bus_id: bus.bus_id(),
                        can_id: msg.identity(),
                        transmitter: msg.transmitter().to_owned(),
                        nominal_period_ms: msg.nominal_period_ms(),
                        int_to_label,
                        label_to_int,
                    },
                );
            }
        }
    }
    map
}

fn parse_cell_value(
    raw_cell: &str,
    meta: &ResolvedSignalMeta,
) -> Option<(f64, i64, Option<String>)> {
    let trimmed = raw_cell.trim();
    if trimmed.is_empty() {
        return None;
    }

    // 1. Check Format B `"LABEL (INT)"` (e.g. `"OFF (0)"`, `"idle (1)"`, `"TUNING (4)"`)
    if let Some(without_close) = trimmed.strip_suffix(')')
        && let Some((label_part, int_part)) = without_close.rsplit_once(" (")
        && let Ok(raw_int) = int_part.trim().parse::<i64>()
    {
        return Some((raw_int as f64, raw_int, Some(label_part.trim().to_owned())));
    }

    // 2. Check numeric value
    if let Ok(val) = trimmed.parse::<f64>() {
        if !val.is_finite() {
            return None;
        }
        let raw_int = val.round().clamp(i64::MIN as f64, i64::MAX as f64) as i64;
        let enum_label = meta.int_to_label.get(&raw_int).cloned();
        return Some((val, raw_int, enum_label));
    }

    // 3. Plain enum label string (e.g. `"OFF"`, `"ON"`, `"idle"`, `"ready2drive"`, `"RUNNING"`)
    let lower = trimmed.to_ascii_lowercase();
    let raw_int = meta
        .label_to_int
        .get(&lower)
        .copied()
        .unwrap_or_else(|| match lower.as_str() {
            "off" | "false" | "fatal" | "unlatched" => 0,
            "on" | "true" | "idle" | "latched" | "charging" => 1,
            "precharging" | "running" => 2,
            "energized" => 3,
            "buzzing" | "tuning" => 4,
            "ready2drive" => 5,
            "error" => 6,
            "reset" => 7,
            "recover" => 8,
            "constant_torque" => 9,
            _ => 0,
        });
    Some((raw_int as f64, raw_int, Some(trimmed.to_owned())))
}

#[derive(Clone)]
struct CsvColumnSpec {
    col_idx: usize,
    meta: ResolvedSignalMeta,
}

#[derive(Clone)]
struct ParsedCsvHeader {
    is_format_b: bool,
    columns: Vec<CsvColumnSpec>,
    gps_cols: HashMap<String, usize>,
}

fn split_csv_line(line: &str) -> Vec<&str> {
    let line = line.trim_end_matches(['\r', '\n']);
    if !line.contains('"') {
        return line.split(',').collect();
    }
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut in_quotes = false;
    for (idx, b) in line.bytes().enumerate() {
        if b == b'"' {
            in_quotes = !in_quotes;
        } else if b == b',' && !in_quotes {
            let cell = line[start..idx].trim_matches('"');
            out.push(cell);
            start = idx + 1;
        }
    }
    out.push(line[start..].trim_matches('"'));
    out
}

fn parse_csv_header_lines(
    header_lines: &[String],
    lookup: &HashMap<(String, String), ResolvedSignalMeta>,
    default_bus: &str,
) -> Option<ParsedCsvHeader> {
    if header_lines.len() < 4 {
        return None;
    }
    let r0 = split_csv_line(&header_lines[0]);
    let r1 = split_csv_line(&header_lines[1]);
    let r2 = split_csv_line(&header_lines[2]);
    let r3 = split_csv_line(&header_lines[3]);

    let is_format_b = r0.get(2).is_some_and(|s| s.trim() == "Bus")
        || r3
            .first()
            .is_some_and(|s| s.trim() == "Message Description");

    let (col_offset, r_sigs, r_descs, r_units) = if is_format_b {
        if header_lines.len() < 7 {
            return None;
        }
        let r4 = split_csv_line(&header_lines[4]);
        let r5 = split_csv_line(&header_lines[5]);
        let r6 = split_csv_line(&header_lines[6]);
        (
            3usize,
            r4.into_iter().map(str::to_owned).collect::<Vec<_>>(),
            Some(r5.into_iter().map(str::to_owned).collect::<Vec<_>>()),
            Some(r6.into_iter().map(str::to_owned).collect::<Vec<_>>()),
        )
    } else {
        (
            1usize,
            r3.into_iter().map(str::to_owned).collect::<Vec<_>>(),
            None,
            None,
        )
    };

    let mut columns = Vec::new();
    let mut gps_cols = HashMap::new();
    let max_cols = r2.len().min(r_sigs.len());

    for col_idx in col_offset..max_cols {
        let msg_name = r2.get(col_idx).map(|s| s.trim()).unwrap_or("");
        let sig_name = r_sigs.get(col_idx).map(|s| s.trim()).unwrap_or("");
        if msg_name.is_empty() || sig_name.is_empty() {
            continue;
        }
        if msg_name == "gps_time" {
            gps_cols.insert(sig_name.to_owned(), col_idx);
        }

        let csv_bus = r0.get(col_idx).map(|s| s.trim()).unwrap_or("");
        let bus_name = if !default_bus.is_empty() && (csv_bus.is_empty() || csv_bus == "Main") {
            default_bus.to_owned()
        } else if !csv_bus.is_empty() {
            csv_bus.to_owned()
        } else {
            "VCAN".to_owned()
        };
        let csv_node = r1.get(col_idx).map(|s| s.trim()).unwrap_or("UNKNOWN");
        let csv_desc = r_descs
            .as_ref()
            .and_then(|v| v.get(col_idx))
            .map(|s| s.trim())
            .unwrap_or("");
        let csv_unit = r_units
            .as_ref()
            .and_then(|v| v.get(col_idx))
            .map(|s| s.trim())
            .unwrap_or("");

        let meta = if let Some(found) = lookup.get(&(msg_name.to_owned(), sig_name.to_owned())) {
            let mut m = found.clone();
            if m.unit.is_empty() && !csv_unit.is_empty() {
                m.unit = csv_unit.to_owned();
            }
            if m.description.is_empty() && !csv_desc.is_empty() {
                m.description = csv_desc.to_owned();
            }
            m
        } else {
            let is_fault_flag = auto_annotate::is_fault_flag_signal(msg_name, sig_name);
            let bus_id = match bus_name.as_str() {
                "VCAN" | "Main" => 0,
                "MCAN" => 1,
                "CCAN" => 2,
                "SCAN" => 3,
                "GCAN" => 4,
                "TCAN" => 5,
                _ => 0,
            };
            ResolvedSignalMeta {
                signal_id: storage::format_signal_id(msg_name, sig_name),
                unit: csv_unit.to_owned(),
                description: csv_desc.to_owned(),
                is_discrete: is_fault_flag,
                is_fault_flag,
                bus_name,
                bus_id,
                can_id: 0,
                transmitter: csv_node.to_owned(),
                nominal_period_ms: None,
                int_to_label: HashMap::new(),
                label_to_int: HashMap::new(),
            }
        };

        columns.push(CsvColumnSpec { col_idx, meta });
    }

    Some(ParsedCsvHeader {
        is_format_b,
        columns,
        gps_cols,
    })
}

fn parse_realtime_str_ms(rt_str: &str, fallback_date: NaiveDate) -> Option<i64> {
    let s = rt_str.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.timestamp_millis());
    }
    // Check `HH:MM:SS.mmm` (used in 2026-06-14)
    if s.len() >= 8 && s.as_bytes().get(2) == Some(&b':') && s.as_bytes().get(5) == Some(&b':') {
        let hh: u32 = s[0..2].parse().ok()?;
        let mm: u32 = s[3..5].parse().ok()?;
        let rest = &s[6..];
        let (sec_str, ms_str) = rest.split_once('.').unwrap_or((rest, "0"));
        let ss: u32 = sec_str.parse().ok()?;
        let ms: u32 = match ms_str.len() {
            0 => 0,
            1 => ms_str.parse::<u32>().ok()? * 100,
            2 => ms_str.parse::<u32>().ok()? * 10,
            _ => ms_str[..3].parse::<u32>().ok()?,
        };
        let naive = fallback_date.and_hms_milli_opt(hh, mm, ss, ms)?;
        // 2026-06-14 `09:54:22` was local EDT (`UTC-4`), add 4 hours for UTC
        return Some(naive.and_utc().timestamp_millis() + 4 * 3_600_000);
    }
    None
}

fn extract_gps_point_from_cells(
    cells: &[&str],
    gps_cols: &HashMap<String, usize>,
    expected_year: i32,
) -> Option<i64> {
    let yr_col = *gps_cols.get("year")?;
    let mo_col = *gps_cols.get("month")?;
    let dy_col = *gps_cols.get("day")?;
    let hr_col = *gps_cols.get("hour")?;
    let mn_col = *gps_cols.get("minute")?;
    let sc_col = *gps_cols.get("second")?;

    let yr_raw = cells.get(yr_col)?.trim();
    if yr_raw.is_empty() {
        return None;
    }
    let yr = yr_raw.parse::<f64>().ok()? as i32;
    let full_year = if yr < 100 { 2000 + yr } else { yr };
    if full_year != expected_year {
        return None;
    }
    let mo = cells.get(mo_col)?.trim().parse::<f64>().ok()? as u32;
    let dy = cells.get(dy_col)?.trim().parse::<f64>().ok()? as u32;
    let hr = cells.get(hr_col)?.trim().parse::<f64>().ok()? as u32;
    let mn = cells.get(mn_col)?.trim().parse::<f64>().ok()? as u32;
    let sc = cells.get(sc_col)?.trim().parse::<f64>().ok()? as u32;
    let ms = gps_cols
        .get("millisecond")
        .and_then(|&c| cells.get(c))
        .and_then(|s| s.trim().parse::<f64>().ok())
        .map(|v| (v as u32).min(999))
        .unwrap_or(0);

    let dt = NaiveDate::from_ymd_opt(full_year, mo, dy)?.and_hms_milli_opt(hr, mn, sc, ms)?;
    Some(Utc.from_utc_datetime(&dt).timestamp_millis())
}

fn fit_gps_clock_mapping(
    gps_points: &[(u32, i64)],
    min_slope: f64,
    max_slope: f64,
    fallback_intercept_ms: i64,
) -> correlate::ClockMapping {
    // Filter to points where GPS timestamp actually changed (avoiding held duplicate rows)
    let mut distinct: Vec<(f64, f64)> = Vec::new();
    let mut last_utc: Option<i64> = None;
    for &(ticks_ms, utc_ms) in gps_points {
        if last_utc != Some(utc_ms) {
            distinct.push((ticks_ms as f64, utc_ms as f64));
            last_utc = Some(utc_ms);
        }
    }

    if distinct.len() >= 2 {
        let n = distinct.len() as f64;
        let sum_x: f64 = distinct.iter().map(|p| p.0).sum();
        let sum_y: f64 = distinct.iter().map(|p| p.1).sum();
        let sum_xy: f64 = distinct.iter().map(|p| p.0 * p.1).sum();
        let sum_x2: f64 = distinct.iter().map(|p| p.0 * p.0).sum();
        let denom = n * sum_x2 - sum_x * sum_x;
        if denom.abs() > 1e-6 {
            let slope = ((n * sum_xy - sum_x * sum_y) / denom).clamp(min_slope, max_slope);
            let intercept_ms = (sum_y - slope * sum_x) / n;
            return correlate::ClockMapping {
                slope,
                intercept_ms,
                confidence: correlate::ClockConfidence::GpsSynced {
                    rms_error_ms: 0.0,
                    point_count: distinct.len(),
                },
            };
        }
    }
    if let Some(&(x0, y0)) = distinct.first() {
        let slope = 1.0_f64.clamp(min_slope, max_slope);
        return correlate::ClockMapping {
            slope,
            intercept_ms: y0 - slope * x0,
            confidence: correlate::ClockConfidence::GpsSynced {
                rms_error_ms: 0.0,
                point_count: 1,
            },
        };
    }
    correlate::ClockMapping {
        slope: 1.0,
        intercept_ms: fallback_intercept_ms as f64,
        confidence: correlate::ClockConfidence::RtcEstimated,
    }
}

fn source_file_record(path: &Path) -> parse::SourceLogFile {
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_owned();
    let mtime_unix_ms = fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let mut hasher = sha2::Sha256::new();
    hasher.update(path.to_string_lossy().as_bytes());
    hasher.update(mtime_unix_ms.to_le_bytes());
    let digest = hasher.finalize();
    let mut sha256_hex = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut sha256_hex, "{b:02x}");
    }
    parse::SourceLogFile {
        rtc_timestamp_ms: parse::parse_rtc_filename_ms(&file_name),
        file_name,
        sha256_hex,
        mtime_unix_ms,
    }
}

fn make_session_annotation(
    id_suffix: &str,
    track_day_id: &str,
    name: &str,
    notes: &str,
    start_utc_ms: i64,
    end_utc_ms: i64,
) -> auto_annotate::Annotation {
    auto_annotate::Annotation {
        id: format!("session_{track_day_id}_{id_suffix}"),
        track_day_id: track_day_id.to_owned(),
        name: name.to_owned(),
        category: "session".to_owned(),
        color: "#3b82f6".to_owned(),
        start_utc_ms,
        end_utc_ms: Some(end_utc_ms.max(start_utc_ms)),
        author: "per26_import".to_owned(),
        source: "manual".to_owned(),
        notes: notes.to_owned(),
        updated_at_ms: Time::now().unix_millis(),
        deleted: false,
    }
}

/// Import a group of Format A or Format B CSV files belonging to a single boot epoch.
#[allow(clippy::too_many_arguments)]
fn import_csv_boot_group(
    csv_files: &[(PathBuf, Option<(f64, f64)>)],
    default_bus: &str,
    track_day_id: &str,
    session_id_suffix: &str,
    session_label: &str,
    expected_year: i32,
    fallback_date: NaiveDate,
    fallback_intercept_ms: i64,
    min_gps_slope: f64,
    max_gps_slope: f64,
    dedup_forward_fill: bool,
    database: &Database,
    store_dir: &Path,
) -> Result<ImportSummary, ImportError> {
    let lookup = build_signal_lookup(database);
    let mut raw_rows: Vec<(Option<i64>, u32, SignalId, f64, i64, Option<String>)> = Vec::new();
    let mut meta_by_id: HashMap<SignalId, ResolvedSignalMeta> = HashMap::new();
    let mut gps_points: Vec<(u32, i64)> = Vec::new();
    let mut source_files = Vec::new();

    for (csv_path, ts_window) in csv_files {
        if !csv_path.is_file() {
            continue;
        }
        source_files.push(source_file_record(csv_path));
        let file = fs::File::open(csv_path)?;
        let mut reader = io::BufReader::with_capacity(256 * 1024, file);
        let mut head = Vec::with_capacity(7);
        for _ in 0..7 {
            let mut ln = String::new();
            if reader.read_line(&mut ln)? == 0 {
                break;
            }
            head.push(ln);
        }
        let Some(header) = parse_csv_header_lines(&head, &lookup, default_bus) else {
            continue;
        };
        for spec in &header.columns {
            meta_by_id
                .entry(spec.meta.signal_id.clone())
                .or_insert_with(|| spec.meta.clone());
        }

        let mut last_val_by_col: Vec<Option<String>> = vec![None; header.columns.len()];

        let mut process_line = |line: &str| {
            let cells = split_csv_line(line);
            let ts_col = usize::from(header.is_format_b);
            let Some(&ts_str) = cells.get(ts_col) else {
                return;
            };
            let Ok(daq_sec) = ts_str.trim().parse::<f64>() else {
                return;
            };
            if !daq_sec.is_finite() || !(0.0..=86_400.0).contains(&daq_sec) {
                return;
            }
            if let Some((min_s, max_s)) = *ts_window
                && (daq_sec < min_s || daq_sec > max_s)
            {
                return;
            }
            let ticks_ms = (daq_sec * 1000.0).round() as u32;
            let rt_ms = if header.is_format_b {
                cells
                    .first()
                    .and_then(|s| parse_realtime_str_ms(s, fallback_date))
            } else {
                None
            };
            if !header.gps_cols.is_empty()
                && let Some(gps_utc_ms) =
                    extract_gps_point_from_cells(&cells, &header.gps_cols, expected_year)
            {
                gps_points.push((ticks_ms, gps_utc_ms));
            }

            for (spec_idx, spec) in header.columns.iter().enumerate() {
                let Some(&cell_str) = cells.get(spec.col_idx) else {
                    continue;
                };
                let trimmed = cell_str.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if dedup_forward_fill {
                    if last_val_by_col[spec_idx].as_deref() == Some(trimmed) {
                        continue;
                    }
                    last_val_by_col[spec_idx] = Some(trimmed.to_owned());
                }
                if let Some((physical, raw_int, enum_label)) = parse_cell_value(trimmed, &spec.meta)
                {
                    raw_rows.push((
                        rt_ms,
                        ticks_ms,
                        spec.meta.signal_id.clone(),
                        physical,
                        raw_int,
                        enum_label,
                    ));
                }
            }
        };

        if !header.is_format_b {
            for ln in head.iter().skip(4) {
                process_line(ln);
            }
        }
        let mut line_buf = String::new();
        while reader.read_line(&mut line_buf)? > 0 {
            process_line(&line_buf);
            line_buf.clear();
        }
    }

    if raw_rows.is_empty() {
        return Ok(ImportSummary::default());
    }

    let clock_mapping = fit_gps_clock_mapping(
        &gps_points,
        min_gps_slope,
        max_gps_slope,
        fallback_intercept_ms,
    );

    let mut columns: BTreeMap<SignalId, SignalColumnAccumulator> = BTreeMap::new();
    let mut min_utc_ms = i64::MAX;
    let mut max_utc_ms = i64::MIN;

    for (rt_ms_opt, ticks_ms, sig_id, physical, raw_int, enum_label) in raw_rows {
        let utc_ms = rt_ms_opt.unwrap_or_else(|| clock_mapping.ticks_to_utc_ms(ticks_ms));
        min_utc_ms = min_utc_ms.min(utc_ms);
        max_utc_ms = max_utc_ms.max(utc_ms);
        let acc = columns.entry(sig_id.clone()).or_insert_with(|| {
            let m = &meta_by_id[&sig_id];
            SignalColumnAccumulator {
                unit: m.unit.clone(),
                is_discrete: m.is_discrete,
                is_fault_flag: m.is_fault_flag,
                bus_name: m.bus_name.clone(),
                bus_id: m.bus_id,
                can_id: m.can_id,
                transmitter: m.transmitter.clone(),
                nominal_period_ms: m.nominal_period_ms,
                description: m.description.clone(),
                samples: Vec::new(),
            }
        });
        if enum_label.is_some() {
            acc.is_discrete = true;
        }
        acc.samples.push(RawSample {
            utc_ms,
            ticks_ms,
            physical,
            raw_int,
            enum_label,
        });
    }

    let extra_ann = if !session_label.is_empty() && min_utc_ms <= max_utc_ms {
        vec![make_session_annotation(
            session_id_suffix,
            track_day_id,
            session_label,
            &format!("Imported session {session_label} on {track_day_id}"),
            min_utc_ms,
            max_utc_ms,
        )]
    } else {
        Vec::new()
    };

    storage::write_column_accumulators_to_store(
        store_dir,
        track_day_id,
        &clock_mapping,
        &source_files,
        database.content_hash(),
        columns,
        &extra_ann,
    )
}

/// Group raw `.log` files in a directory into monotonic boot chains using the invariant
/// `origin = rtc_timestamp_ms - first_tick_ms` (`±45s` tolerance).
fn chain_raw_log_files_by_boot(dir: &Path) -> io::Result<Vec<Vec<PathBuf>>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut files: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().and_then(|s| s.to_str()) == Some("log"))
        .collect();
    files.sort();

    struct FileSpan {
        path: PathBuf,
        rtc_ms: Option<i64>,
        first_tick: u32,
        last_tick: u32,
    }

    let mut spans = Vec::new();
    for path in files {
        let bytes = fs::read(&path)?;
        let n = bytes.len() / 16;
        if n == 0 {
            continue;
        }
        let first_tick = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
        let last_tick =
            u32::from_le_bytes(bytes[(n - 1) * 16..(n - 1) * 16 + 4].try_into().unwrap());
        if first_tick > 86_400_000 || last_tick > 86_400_000 {
            continue;
        }
        let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let rtc_ms = parse::parse_rtc_filename_ms(fname);
        spans.push(FileSpan {
            path,
            rtc_ms,
            first_tick,
            last_tick,
        });
    }

    let mut chains: Vec<Vec<FileSpan>> = Vec::new();
    for span in spans {
        let mut best_idx = None;
        let mut best_gap = u32::MAX;
        for (idx, chain) in chains.iter().enumerate() {
            let prev = chain.last().unwrap();
            if span.first_tick < prev.last_tick
                || span.first_tick.saturating_sub(prev.last_tick) > 1_800_000
            {
                continue;
            }
            let origin_ok = match (span.rtc_ms, prev.rtc_ms) {
                (Some(r_cur), Some(r_prev)) => {
                    let o_cur = r_cur - (span.first_tick as i64);
                    let o_prev = r_prev - (prev.first_tick as i64);
                    (o_cur - o_prev).abs() <= 45_000
                }
                _ => true,
            };
            if origin_ok {
                let gap = span.first_tick - prev.last_tick;
                if gap < best_gap {
                    best_gap = gap;
                    best_idx = Some(idx);
                }
            }
        }
        if let Some(idx) = best_idx {
            chains[idx].push(span);
        } else {
            chains.push(vec![span]);
        }
    }

    Ok(chains
        .into_iter()
        .map(|ch| ch.into_iter().map(|s| s.path).collect())
        .collect())
}

/// Decode and merge multi-bus raw `.log` files for a single boot epoch and write Parquet partitions.
#[allow(clippy::too_many_arguments)]
fn import_multi_bus_raw_boot(
    bus_files: &[(&str, Vec<PathBuf>)],
    track_day_id: &str,
    base_utc_ms: i64,
    session_id_suffix: &str,
    session_label: &str,
    database: &Database,
    store_dir: &Path,
) -> Result<ImportSummary, ImportError> {
    let mut all_source_files = Vec::new();
    let mut all_messages = Vec::new();

    for (bus_name, paths) in bus_files {
        for path in paths {
            if !path.is_file() {
                continue;
            }
            let content = fs::read(path)?;
            if content.len() < 16 {
                continue;
            }
            let file_idx = all_source_files.len();
            let file_name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown.log");
            let mtime_ms = fs::metadata(path)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(base_utc_ms);
            let (meta, msgs) = parse::parse_log_bytes_on_bus(
                &content, file_name, mtime_ms, file_idx, bus_name, database,
            );
            all_source_files.push(meta);
            all_messages.extend(msgs);
        }
    }

    if all_messages.is_empty() {
        return Ok(ImportSummary::default());
    }

    all_messages.sort_by_key(|m| m.timestamp);
    let min_tick = all_messages.first().map(|m| m.timestamp).unwrap_or(0);
    let max_tick = all_messages.last().map(|m| m.timestamp).unwrap_or(min_tick);

    let clock_mapping = correlate::ClockMapping {
        slope: 1.0,
        intercept_ms: base_utc_ms as f64,
        confidence: correlate::ClockConfidence::RtcEstimated,
    };
    let epoch = correlate::CorrelationChunkResult {
        parsed_msgs: all_messages,
        clock_mapping,
    };

    let summary = storage::write_correlated_epochs_to_store_for_day(
        store_dir,
        &all_source_files,
        vec![epoch],
        database,
        Some(track_day_id),
        |_, _, _| {},
    )?;

    if !session_label.is_empty() {
        let registry = Registry::open_store(store_dir)?;
        let start_ms = base_utc_ms + (min_tick as i64);
        let end_ms = base_utc_ms + (max_tick as i64);
        let ann = make_session_annotation(
            session_id_suffix,
            track_day_id,
            session_label,
            &format!("Imported multi-bus session {session_label} on {track_day_id}"),
            start_ms,
            end_ms,
        );
        registry.upsert_annotation(&ann)?;
    }

    Ok(summary)
}

fn accumulate_summary(report: &mut Per26ImportReport, summary: &ImportSummary) {
    report.partitions_written += summary.partitions_written;
    report.partitions_skipped_existing += summary.partitions_skipped_existing;
    report.annotations_created += summary
        .manifests
        .iter()
        .map(|m| m.annotations.len())
        .sum::<usize>();
}

/// Import Track Day 1: `2025-09-20-KMS` (`parsed/out_0.csv` .. `out_47.csv`).
fn import_day_2025_09_20(
    per26_dir: &Path,
    store_dir: &Path,
    superdbc_dir: &Path,
    report: &mut Per26ImportReport,
) -> Result<(), ImportError> {
    let parsed_dir = per26_dir.join("2025-09-20-KMS/parsed");
    let dbc_path = superdbc_dir.join("superdbc_2025_09_20.json");
    if !parsed_dir.is_dir() || !dbc_path.is_file() {
        return Ok(());
    }
    let db =
        Database::load(&dbc_path).map_err(|e| ImportError::Io(io::Error::other(e.to_string())))?;
    let lookup = build_signal_lookup(&db);

    // Split each out_*.csv into 4000-row (60s DAQ time) log blocks and keep all GPS-verified blocks
    struct VerifiedBlock {
        gps_start_ms: i64,
        file_idx: usize,
        blk_idx: usize,
        slope: f64,
        intercept_ms: f64,
        header: ParsedCsvHeader,
        lines: Vec<String>,
        source_file: parse::SourceLogFile,
    }

    let mut verified_blocks = Vec::new();
    for file_idx in 0..48 {
        let csv_path = parsed_dir.join(format!("out_{file_idx}.csv"));
        if !csv_path.is_file() {
            continue;
        }
        let source_file = source_file_record(&csv_path);
        let content = fs::read_to_string(&csv_path)?;
        let all_lines: Vec<String> = content.lines().map(str::to_owned).collect();
        if all_lines.len() < 5 {
            continue;
        }
        let Some(header) = parse_csv_header_lines(&all_lines[..4], &lookup, "VCAN") else {
            continue;
        };
        let data_lines = &all_lines[4..];
        for (blk_idx, chunk) in data_lines.chunks(4000).enumerate() {
            if chunk.len() < 100 {
                continue;
            }
            let mut gps_pts = Vec::new();
            for ln in chunk {
                let cells = split_csv_line(ln);
                let Some(&ts_str) = cells.first() else {
                    continue;
                };
                let Ok(daq_sec) = ts_str.trim().parse::<f64>() else {
                    continue;
                };
                let ticks_ms = (daq_sec * 1000.0).round() as u32;
                if let Some(utc_ms) = extract_gps_point_from_cells(&cells, &header.gps_cols, 2025) {
                    if gps_pts.last().map(|&(_, u)| u) != Some(utc_ms) {
                        gps_pts.push((ticks_ms, utc_ms));
                    }
                }
            }
            if gps_pts.len() < 10 {
                continue;
            }
            let dt_daq =
                (gps_pts.last().unwrap().0 as f64 - gps_pts.first().unwrap().0 as f64) / 1000.0;
            let dt_gps =
                (gps_pts.last().unwrap().1 as f64 - gps_pts.first().unwrap().1 as f64) / 1000.0;
            if dt_daq < 5.0 {
                continue;
            }
            let raw_slope = dt_gps / dt_daq;
            // In PER25 (2025-09-20), 1 DAQ tick sec = ~1.5625 real UTC seconds; blocks with slope outside [1.2, 1.8]
            // are uncalibrated/held transitions between distinct boots.
            if !(1.20..=1.80).contains(&raw_slope) {
                continue;
            }
            let mapping = fit_gps_clock_mapping(&gps_pts, 1.25, 1.75, 0);
            if matches!(
                mapping.confidence,
                correlate::ClockConfidence::GpsSynced { .. }
            ) {
                let slope = mapping.slope;
                let intercept_ms = mapping.intercept_ms;
                let first_tick = split_csv_line(&chunk[0])
                    .first()
                    .and_then(|s| s.trim().parse::<f64>().ok())
                    .map(|s| (s * 1000.0).round() as u32)
                    .unwrap_or(0);
                let gps_start_ms = ((slope * (first_tick as f64)) + intercept_ms).round() as i64;
                verified_blocks.push(VerifiedBlock {
                    gps_start_ms,
                    file_idx,
                    blk_idx,
                    slope,
                    intercept_ms,
                    header: header.clone(),
                    lines: chunk.to_vec(),
                    source_file: source_file.clone(),
                });
            }
        }
    }

    verified_blocks.sort_by_key(|b| (b.gps_start_ms, b.file_idx, b.blk_idx));

    // Group verified blocks into sessions separated by > 180 seconds
    let mut sessions: Vec<Vec<VerifiedBlock>> = Vec::new();
    for blk in verified_blocks {
        if let Some(cur) = sessions.last_mut() {
            let prev_start = cur.last().unwrap().gps_start_ms;
            if (blk.gps_start_ms - prev_start).abs() <= 180_000 {
                cur.push(blk);
                continue;
            }
        }
        sessions.push(vec![blk]);
    }

    let track_day_id = "2025-09-20";
    for (sess_idx, sess_blocks) in sessions.into_iter().enumerate() {
        let mut columns: BTreeMap<SignalId, SignalColumnAccumulator> = BTreeMap::new();
        let mut source_files = Vec::new();
        let mut min_utc_ms = i64::MAX;
        let mut max_utc_ms = i64::MIN;
        let first_slope = sess_blocks[0].slope;
        let first_intercept = sess_blocks[0].intercept_ms;

        for blk in sess_blocks {
            if !source_files
                .iter()
                .any(|f: &parse::SourceLogFile| f.file_name == blk.source_file.file_name)
            {
                source_files.push(blk.source_file.clone());
            }
            let mut last_val_by_col: Vec<Option<String>> = vec![None; blk.header.columns.len()];
            for ln in &blk.lines {
                let cells = split_csv_line(ln);
                let Some(&ts_str) = cells.first() else {
                    continue;
                };
                let Ok(daq_sec) = ts_str.trim().parse::<f64>() else {
                    continue;
                };
                let ticks_ms = (daq_sec * 1000.0).round() as u32;
                let utc_ms = ((blk.slope * (ticks_ms as f64)) + blk.intercept_ms).round() as i64;
                min_utc_ms = min_utc_ms.min(utc_ms);
                max_utc_ms = max_utc_ms.max(utc_ms);

                for (spec_idx, spec) in blk.header.columns.iter().enumerate() {
                    let Some(&cell_str) = cells.get(spec.col_idx) else {
                        continue;
                    };
                    let trimmed = cell_str.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if last_val_by_col[spec_idx].as_deref() == Some(trimmed) {
                        continue;
                    }
                    last_val_by_col[spec_idx] = Some(trimmed.to_owned());
                    if let Some((physical, raw_int, enum_label)) =
                        parse_cell_value(trimmed, &spec.meta)
                    {
                        let acc = columns
                            .entry(spec.meta.signal_id.clone())
                            .or_insert_with(|| SignalColumnAccumulator {
                                unit: spec.meta.unit.clone(),
                                is_discrete: spec.meta.is_discrete,
                                is_fault_flag: spec.meta.is_fault_flag,
                                bus_name: spec.meta.bus_name.clone(),
                                bus_id: spec.meta.bus_id,
                                can_id: spec.meta.can_id,
                                transmitter: spec.meta.transmitter.clone(),
                                nominal_period_ms: spec.meta.nominal_period_ms,
                                description: spec.meta.description.clone(),
                                samples: Vec::new(),
                            });
                        if enum_label.is_some() {
                            acc.is_discrete = true;
                        }
                        acc.samples.push(RawSample {
                            utc_ms,
                            ticks_ms,
                            physical,
                            raw_int,
                            enum_label,
                        });
                    }
                }
            }
        }

        if columns.is_empty() {
            continue;
        }
        let clock_mapping = correlate::ClockMapping {
            slope: first_slope,
            intercept_ms: first_intercept,
            confidence: correlate::ClockConfidence::GpsSynced {
                rms_error_ms: 0.0,
                point_count: 100,
            },
        };
        let label = format!("KMS Session {}", sess_idx + 1);
        let ann = make_session_annotation(
            &format!("run_{}", sess_idx + 1),
            track_day_id,
            &label,
            "PER25 Fall KMS Track Session",
            min_utc_ms,
            max_utc_ms,
        );
        let summary = storage::write_column_accumulators_to_store(
            store_dir,
            track_day_id,
            &clock_mapping,
            &source_files,
            db.content_hash(),
            columns,
            &[ann],
        )?;
        accumulate_summary(report, &summary);
    }

    let registry = Registry::open_store(store_dir)?;
    if let Some(mut td) = registry.get_track_day(track_day_id)? {
        td.title = "2025-09-20 — KMS Fall Test Day (PER25)".to_owned();
        td.location = "KMS".to_owned();
        registry.upsert_track_day(&td)?;
        report.track_days_imported += 1;
    }
    Ok(())
}

/// Import Track Day 2: `2026-04-05-KMS` (Easter Test Day).
fn import_day_2026_04_05(
    per26_dir: &Path,
    store_dir: &Path,
    superdbc_dir: &Path,
    report: &mut Per26ImportReport,
) -> Result<(), ImportError> {
    let day_dir = per26_dir.join("2026-04-05-KMS");
    let dbc_path = superdbc_dir.join("superdbc_2026_04_05.json");
    if !day_dir.is_dir() || !dbc_path.is_file() {
        return Ok(());
    }
    let db =
        Database::load(&dbc_path).map_err(|e| ImportError::Io(io::Error::other(e.to_string())))?;
    let mcan_dir = day_dir.join("MCAN");
    let vcan_dir = day_dir.join("VCAN");

    let boots: &[(&str, &str, i64, &[&str])] = &[
        (
            "boot1",
            "Easter Run 1",
            1_775_401_200_000, // 2026-04-05T15:00:00Z
            &["log-2026-04-05--03-24-35.log"],
        ),
        (
            "boot2",
            "Easter Run 2",
            1_775_401_800_000, // 2026-04-05T15:10:00Z
            &[
                "log-2026-04-05--03-24-40.log",
                "log-2026-04-05--03-25-40.log",
                "log-2026-04-05--03-26-40.log",
            ],
        ),
        (
            "boot3",
            "Easter Run 3",
            1_775_402_400_000, // 2026-04-05T15:20:00Z
            &[
                "log-2026-04-05--03-24-42.log",
                "log-2026-04-05--03-25-42.log",
                "log-2026-04-05--03-26-42.log",
                "log-2026-04-05--03-27-42.log",
            ],
        ),
    ];

    let track_day_id = "2026-04-05";
    for &(suffix, label, base_utc_ms, fnames) in boots {
        let mcan_files: Vec<PathBuf> = fnames.iter().map(|f| mcan_dir.join(f)).collect();
        let vcan_files: Vec<PathBuf> = fnames.iter().map(|f| vcan_dir.join(f)).collect();
        let summary = import_multi_bus_raw_boot(
            &[("MCAN", mcan_files), ("VCAN", vcan_files)],
            track_day_id,
            base_utc_ms,
            suffix,
            label,
            &db,
            store_dir,
        )?;
        accumulate_summary(report, &summary);
    }

    let registry = Registry::open_store(store_dir)?;
    if let Some(mut td) = registry.get_track_day(track_day_id)? {
        td.title = "2026-04-05 — KMS Easter Test Day".to_owned();
        td.location = "KMS".to_owned();
        registry.upsert_track_day(&td)?;
        report.track_days_imported += 1;
    }
    Ok(())
}

/// Import Track Day 3: `2026-04-10-DYNO` (10 Dyno Pulls).
fn import_day_2026_04_10(
    per26_dir: &Path,
    store_dir: &Path,
    superdbc_dir: &Path,
    report: &mut Per26ImportReport,
) -> Result<(), ImportError> {
    let dyno_root = per26_dir.join("2026-04-10-DYNO/2026-04-10-DYNO_all");
    let dbc_path = superdbc_dir.join("superdbc_2026_04_11.json");
    if !dyno_root.is_dir() || !dbc_path.is_file() {
        return Ok(());
    }
    let db =
        Database::load(&dbc_path).map_err(|e| ImportError::Io(io::Error::other(e.to_string())))?;
    let track_day_id = "2026-04-10";

    // Boot 1 base: 2026-04-10T14:00:00Z (1_775_829_600_000)
    // Boot 2 base (5w30 & redline oil fills): 2026-04-10T17:00:00Z (1_775_840_400_000)
    let pulls: &[(&str, &str, i64, Option<&str>)] = &[
        (
            "1_initial_pull",
            "Dyno: 1_initial_pull",
            1_775_829_600_000,
            Some("-54.log"), // Filter out leftover 2026-04-05 files (-35, -40, -42, -50)
        ),
        (
            "1_speed_sweep1",
            "Dyno: 1_speed_sweep1",
            1_775_829_600_000,
            None,
        ),
        (
            "1_quarter_throttle_pull",
            "Dyno: 1_quarter_throttle_pull",
            1_775_829_600_000,
            None,
        ),
        (
            "1_half_throttle_pull",
            "Dyno: 1_half_throttle_pull",
            1_775_829_600_000,
            None,
        ),
        (
            "1_full_throttle_pull_A",
            "Dyno: 1_full_throttle_pull_A",
            1_775_829_600_000,
            None,
        ),
        (
            "2_20fill_quarter_throttle_pull",
            "Dyno: 2_20fill_quarter_throttle_pull",
            1_775_829_600_000,
            None,
        ),
        (
            "2_25fill_quarter_throttle_pull",
            "Dyno: 2_25fill_quarter_throttle_pull",
            1_775_829_600_000,
            None,
        ),
        (
            "2_30fill_quarter_throttle_pull",
            "Dyno: 2_30fill_quarter_throttle_pull",
            1_775_829_600_000,
            None,
        ),
        (
            "3_5w30_25fill_quarter_throttle_pull",
            "Dyno: 3_5w30_25fill_quarter_throttle_pull",
            1_775_840_400_000,
            Some("-16.log"), // Keep the 06-37-16..06-39-16 boot2 files, skip short 23/15 fragments
        ),
        (
            "3_redline_25fill_quarter_throttle_pull",
            "Dyno: 3_redline_25fill_quarter_throttle_pull",
            1_775_840_400_000,
            None,
        ),
    ];

    for &(folder, label, base_utc_ms, filter_pat) in pulls {
        let pull_dir = dyno_root.join(folder);
        if !pull_dir.is_dir() {
            continue;
        }
        let collect_filtered = |sub: &str| -> Vec<PathBuf> {
            let d = pull_dir.join(sub);
            if !d.is_dir() {
                return Vec::new();
            }
            let mut v: Vec<PathBuf> = fs::read_dir(d)
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| {
                    p.is_file()
                        && p.extension().and_then(|s| s.to_str()) == Some("log")
                        && filter_pat.is_none_or(|pat| {
                            p.file_name()
                                .and_then(|n| n.to_str())
                                .is_some_and(|n| n.contains(pat))
                        })
                })
                .collect();
            v.sort();
            v
        };
        let mcan_files = collect_filtered("MCAN_RAW");
        let vcan_files = collect_filtered("VCAN_RAW");
        let summary = import_multi_bus_raw_boot(
            &[("MCAN", mcan_files), ("VCAN", vcan_files)],
            track_day_id,
            base_utc_ms,
            folder,
            label,
            &db,
            store_dir,
        )?;
        accumulate_summary(report, &summary);
    }

    let registry = Registry::open_store(store_dir)?;
    if let Some(mut td) = registry.get_track_day(track_day_id)? {
        td.title = "2026-04-10 — Dyno Session (Throttle & Oil Fill Sweeps)".to_owned();
        td.location = "Dyno".to_owned();
        registry.upsert_track_day(&td)?;
        report.track_days_imported += 1;
    }
    Ok(())
}

/// Import Track Day 4: `2026-04-11-KMS` (MCAN + VCAN + TCAN LR_CT3 Wheel Force Transducer).
fn import_day_2026_04_11(
    per26_dir: &Path,
    store_dir: &Path,
    superdbc_dir: &Path,
    report: &mut Per26ImportReport,
) -> Result<(), ImportError> {
    let day_root = per26_dir.join("2026-04-11-KMS/2026-04-11-KMS");
    let dbc_path = superdbc_dir.join("superdbc_2026_04_11.json");
    if !day_root.is_dir() || !dbc_path.is_file() {
        return Ok(());
    }
    let db =
        Database::load(&dbc_path).map_err(|e| ImportError::Io(io::Error::other(e.to_string())))?;
    let track_day_id = "2026-04-11";
    let base_utc_ms = 1_775_912_559_000_i64; // 2026-04-11T13:02:39Z

    let list_logs = |d: PathBuf| -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = fs::read_dir(d)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().and_then(|s| s.to_str()) == Some("log"))
            .collect();
        v.sort();
        v
    };

    // Morning session (_1): MCAN_RAW_1 + VCAN_RAW_1 + TCAN_RAW_1
    let mcan_1 = list_logs(day_root.join("MCAN_RAW_1"));
    let vcan_1 = list_logs(day_root.join("VCAN_RAW_1"));
    let tcan_1 = list_logs(day_root.join("TCAN_RAW_1"));
    let s1 = import_multi_bus_raw_boot(
        &[("MCAN", mcan_1), ("VCAN", vcan_1), ("TCAN", tcan_1)],
        track_day_id,
        base_utc_ms,
        "part1",
        "KMS Morning Session (LR CT3)",
        &db,
        store_dir,
    )?;
    accumulate_summary(report, &s1);

    // Afternoon session (_2): MCAN_RAW_2 + VCAN_RAW_2 + first 240 files of 2026-04-12/TCAN_RAW
    let mcan_2 = list_logs(day_root.join("MCAN_RAW_2"));
    let vcan_2 = list_logs(day_root.join("VCAN_RAW_2"));
    let tcan_all = list_logs(per26_dir.join("2026-04-12-KMS/2026-04-12-KMS/TCAN_RAW"));
    let tcan_2: Vec<PathBuf> = tcan_all
        .into_iter()
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n < "log-2026-04-05--13-25-54.log")
        })
        .collect();
    let s2 = import_multi_bus_raw_boot(
        &[("MCAN", mcan_2), ("VCAN", vcan_2), ("TCAN", tcan_2)],
        track_day_id,
        base_utc_ms,
        "part2",
        "KMS Afternoon Session (LR CT3)",
        &db,
        store_dir,
    )?;
    accumulate_summary(report, &s2);

    let registry = Registry::open_store(store_dir)?;
    if let Some(mut td) = registry.get_track_day(track_day_id)? {
        td.title = "2026-04-11 — KMS Test Day (LR CT3 Wheel Force)".to_owned();
        td.location = "KMS".to_owned();
        registry.upsert_track_day(&td)?;
        report.track_days_imported += 1;
    }
    Ok(())
}

/// Import Track Day 5: `2026-04-12-KMS` (`*_2` folders + LF_CT3 Wheel Force Transducer).
fn import_day_2026_04_12(
    per26_dir: &Path,
    store_dir: &Path,
    superdbc_dir: &Path,
    report: &mut Per26ImportReport,
) -> Result<(), ImportError> {
    let day_root = per26_dir.join("2026-04-12-KMS/2026-04-12-KMS");
    let dbc_path = superdbc_dir.join("superdbc_2026_04_12.json");
    if !day_root.is_dir() || !dbc_path.is_file() {
        return Ok(());
    }
    let db =
        Database::load(&dbc_path).map_err(|e| ImportError::Io(io::Error::other(e.to_string())))?;
    let track_day_id = "2026-04-12";

    let mcan_chains = chain_raw_log_files_by_boot(&day_root.join("MCAN_RAW_2"))?;
    let vcan_chains = chain_raw_log_files_by_boot(&day_root.join("VCAN_RAW_2"))?;
    let tcan_2: Vec<PathBuf> = fs::read_dir(day_root.join("TCAN_RAW"))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n >= "log-2026-04-05--13-25-54.log")
        })
        .collect();

    // Boot 1 (13:25:55): mcan_chains[0] + vcan_chains[0] + tcan_2
    if let (Some(m0), Some(v0)) = (mcan_chains.first(), vcan_chains.first()) {
        let s1 = import_multi_bus_raw_boot(
            &[("MCAN", m0.clone()), ("VCAN", v0.clone()), ("TCAN", tcan_2)],
            track_day_id,
            1_776_000_355_000, // 2026-04-12T13:25:55Z
            "session1",
            "KMS Session 1 (MCAN + VCAN + LF CT3)",
            &db,
            store_dir,
        )?;
        accumulate_summary(report, &s1);
    }

    // Boot 2 (13:51:12 VCAN-only session): vcan_chains with > 15 files starting around 13:51
    for (idx, vch) in vcan_chains.iter().enumerate().skip(1) {
        let first_name = vch
            .first()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if first_name.contains("13-51-") {
            let s2 = import_multi_bus_raw_boot(
                &[("VCAN", vch.clone())],
                track_day_id,
                1_776_001_872_000, // 2026-04-12T13:51:12Z
                &format!("session2_{idx}"),
                "KMS Session 2 (VCAN)",
                &db,
                store_dir,
            )?;
            accumulate_summary(report, &s2);
        } else if first_name.contains("14-36-") {
            let m_match = mcan_chains
                .iter()
                .find(|mch| {
                    mch.first()
                        .and_then(|p| p.file_name())
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.contains("14-36-"))
                })
                .cloned()
                .unwrap_or_default();
            let s3 = import_multi_bus_raw_boot(
                &[("MCAN", m_match), ("VCAN", vch.clone())],
                track_day_id,
                1_776_004_597_000, // 2026-04-12T14:36:37Z
                "session3",
                "KMS Session 3 (MCAN + VCAN Endurance Run)",
                &db,
                store_dir,
            )?;
            accumulate_summary(report, &s3);
        }
    }

    let registry = Registry::open_store(store_dir)?;
    if let Some(mut td) = registry.get_track_day(track_day_id)? {
        td.title = "2026-04-12 — KMS Test Day (LF CT3 Wheel Force)".to_owned();
        td.location = "KMS".to_owned();
        registry.upsert_track_day(&td)?;
        report.track_days_imported += 1;
    }
    Ok(())
}

/// Import Track Day 6 (`2026-05-03`) and Track Day 7 (`2026-05-10`) from `PER26/2026-05-KMS`.
fn import_days_2026_05(
    per26_dir: &Path,
    store_dir: &Path,
    superdbc_dir: &Path,
    report: &mut Per26ImportReport,
) -> Result<(), ImportError> {
    let may_root = per26_dir.join("2026-05-KMS");
    let dbc_0503 = superdbc_dir.join("superdbc_2026_05_03.json");
    let dbc_0510 = superdbc_dir.join("superdbc_2026_05_10.json");
    if !may_root.is_dir() || !dbc_0503.is_file() || !dbc_0510.is_file() {
        return Ok(());
    }
    let db_0503 =
        Database::load(&dbc_0503).map_err(|e| ImportError::Io(io::Error::other(e.to_string())))?;
    let db_0510 =
        Database::load(&dbc_0510).map_err(|e| ImportError::Io(io::Error::other(e.to_string())))?;

    let date_0503 = NaiveDate::from_ymd_opt(2026, 5, 3).unwrap();
    let p1 = may_root.join("may3_part1");
    let p2 = may_root.join("may3_part2");
    let p3 = may_root.join("may3_part3");

    let may3_boots: &[(&str, &str, Vec<(PathBuf, Option<(f64, f64)>)>)] = &[
        (
            "may3_p1_b1",
            "May 3 Part 1 — Run 1",
            vec![
                (p1.join("out_000.csv"), None),
                (p1.join("out_002.csv"), None),
            ],
        ),
        (
            "may3_p1_b2",
            "May 3 Part 1 — Run 2",
            vec![
                (p1.join("out_003.csv"), None),
                (p1.join("out_005.csv"), None),
            ],
        ),
        (
            "may3_p1_b3",
            "May 3 Part 1 — Run 3",
            vec![(p1.join("out_006.csv"), None)],
        ),
        (
            "may3_p1_b4",
            "May 3 Part 1 — Run 4",
            vec![
                (p1.join("out_007.csv"), None),
                (p1.join("out_010.csv"), None),
                (p1.join("out_014.csv"), None),
                (p1.join("out_017.csv"), None),
            ],
        ),
        (
            "may3_p2_b1",
            "May 3 Part 2 — Run 1",
            vec![
                (p2.join("out_000.csv"), None),
                (p2.join("out_002.csv"), None),
                (p2.join("out_004.csv"), None),
                (p2.join("out_007.csv"), None),
                (p2.join("out_011.csv"), None),
            ],
        ),
        (
            "may3_p2_b2",
            "May 3 Part 2 — Run 2",
            vec![
                (p2.join("out_012.csv"), None),
                (p2.join("out_016.csv"), None),
            ],
        ),
        (
            "may3_p3_b1",
            "May 3 Part 3 — Run 1",
            vec![
                (p3.join("out_003.csv"), Some((0.0, 375.875))),
                (p3.join("out_004.csv"), None),
            ],
        ),
        (
            "may3_p3_b2",
            "May 3 Part 3 — Run 2",
            vec![
                (p3.join("out_000.csv"), None),
                (p3.join("out_002.csv"), None),
                (p3.join("out_005.csv"), None),
                (p3.join("out_011.csv"), None),
                (p3.join("out_016.csv"), None),
                (p3.join("out_023.csv"), None),
            ],
        ),
    ];

    for (suffix, label, files) in may3_boots {
        let s = import_csv_boot_group(
            files,
            "VCAN",
            "2026-05-03",
            suffix,
            label,
            2026,
            date_0503,
            1_777_820_000_000,
            1.20,
            1.80,
            false,
            &db_0503,
            store_dir,
        )?;
        accumulate_summary(report, &s);
    }

    let registry = Registry::open_store(store_dir)?;
    if let Some(mut td) = registry.get_track_day("2026-05-03")? {
        td.title = "2026-05-03 — KMS Torque Vectoring Test Day (Parts 1–3)".to_owned();
        td.location = "KMS".to_owned();
        registry.upsert_track_day(&td)?;
        report.track_days_imported += 1;
    }

    // Track Day 7: 2026-05-10
    let date_0510 = NaiveDate::from_ymd_opt(2026, 5, 10).unwrap();
    let hunter = may_root.join("may10-hunter-runs");
    let accel = may_root.join("may10-accel-skids");

    let may10_boots: &[(&str, &str, Vec<(PathBuf, Option<(f64, f64)>)>)] = &[
        (
            "may10_hunter",
            "May 10 — Hunter Runs",
            vec![
                (hunter.join("out_000.csv"), None),
                (hunter.join("out_008.csv"), None),
            ],
        ),
        (
            "may10_accel_1",
            "May 10 — Accel & Skids Run 1",
            vec![(accel.join("out_000.csv"), None)],
        ),
        (
            "may10_accel_2",
            "May 10 — Accel & Skids Run 2",
            vec![(accel.join("out_001.csv"), None)],
        ),
        (
            "may10_accel_3",
            "May 10 — Accel & Skids Run 3",
            vec![(accel.join("out_002.csv"), None)],
        ),
        (
            "may10_accel_4",
            "May 10 — Accel & Skids Run 4",
            vec![(accel.join("out_003.csv"), None)],
        ),
    ];

    for (suffix, label, files) in may10_boots {
        let s = import_csv_boot_group(
            files,
            "VCAN",
            "2026-05-10",
            suffix,
            label,
            2026,
            date_0510,
            1_778_430_000_000,
            1.20,
            1.80,
            false,
            &db_0510,
            store_dir,
        )?;
        accumulate_summary(report, &s);
    }

    if let Some(mut td) = registry.get_track_day("2026-05-10")? {
        td.title = "2026-05-10 — KMS Hunter Runs, TV+Regen & Accel/Skids".to_owned();
        td.location = "KMS".to_owned();
        registry.upsert_track_day(&td)?;
        report.track_days_imported += 1;
    }

    Ok(())
}

/// Import Track Day 8: `2026-06-14` (`out_005_2026_06_14__09_51_49_torque_window.csv`).
fn import_day_2026_06_14(
    per26_dir: &Path,
    store_dir: &Path,
    superdbc_dir: &Path,
    report: &mut Per26ImportReport,
) -> Result<(), ImportError> {
    let csv_path = per26_dir.join("2026-06-14/out_005_2026_06_14__09_51_49_torque_window.csv");
    let dbc_path = superdbc_dir.join("superdbc_2026_06_14.json");
    if !csv_path.is_file() || !dbc_path.is_file() {
        return Ok(());
    }
    let db =
        Database::load(&dbc_path).map_err(|e| ImportError::Io(io::Error::other(e.to_string())))?;
    let date_0614 = NaiveDate::from_ymd_opt(2026, 6, 14).unwrap();
    let s = import_csv_boot_group(
        &[(csv_path, None)],
        "VCAN",
        "2026-06-14",
        "torque_window",
        "Comp Prep — Torque Window",
        2026,
        date_0614,
        1_781_445_109_000,
        0.995,
        1.005,
        false,
        &db,
        store_dir,
    )?;
    accumulate_summary(report, &s);

    let registry = Registry::open_store(store_dir)?;
    if let Some(mut td) = registry.get_track_day("2026-06-14")? {
        td.title = "2026-06-14 — Comp Prep (Torque Window Session)".to_owned();
        td.location = "Purdue / Comp Prep".to_owned();
        registry.upsert_track_day(&td)?;
        report.track_days_imported += 1;
    }
    Ok(())
}

/// Import Track Day 9: `2026-09-05` (`endurance_attempt/parsed/couple_laps_*.csv`).
fn import_day_2026_09_05(
    per26_dir: &Path,
    store_dir: &Path,
    superdbc_dir: &Path,
    report: &mut Per26ImportReport,
) -> Result<(), ImportError> {
    let parsed_dir = per26_dir.join("2026-09-05/endurance_attempt/parsed");
    let dbc_path = superdbc_dir.join("superdbc_2026_09_06.json");
    if !parsed_dir.is_dir() || !dbc_path.is_file() {
        return Ok(());
    }
    let db =
        Database::load(&dbc_path).map_err(|e| ImportError::Io(io::Error::other(e.to_string())))?;
    let date_0905 = NaiveDate::from_ymd_opt(2026, 9, 5).unwrap();

    let mut csv_files: Vec<(PathBuf, Option<(f64, f64)>)> = fs::read_dir(&parsed_dir)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().and_then(|s| s.to_str()) == Some("csv"))
        .map(|p| (p, None))
        .collect();
    csv_files.sort_by(|a, b| a.0.cmp(&b.0));

    let s = import_csv_boot_group(
        &csv_files,
        "VCAN",
        "2026-09-05",
        "endurance_attempt",
        "Endurance Attempt (Couple Laps)",
        2026,
        date_0905,
        1_788_646_162_987,
        0.995,
        1.005,
        false,
        &db,
        store_dir,
    )?;
    accumulate_summary(report, &s);

    let registry = Registry::open_store(store_dir)?;
    if let Some(mut td) = registry.get_track_day("2026-09-05")? {
        td.title = "2026-09-05 — KMS Endurance Attempt".to_owned();
        td.location = "KMS".to_owned();
        registry.upsert_track_day(&td)?;
        report.track_days_imported += 1;
    }
    Ok(())
}

fn is_day_already_populated(
    store_dir: &Path,
    track_day_id: &str,
    min_partitions: usize,
) -> Option<(usize, usize)> {
    let registry = Registry::open_store(store_dir).ok()?;
    let parts = registry.list_partitions_for_day(track_day_id).ok()?;
    if parts.len() >= min_partitions {
        let anns = registry
            .list_annotations_for_day(track_day_id, false)
            .map(|v| v.len())
            .unwrap_or(0);
        Some((parts.len(), anns))
    } else {
        None
    }
}

/// Stream a raw `.log` directory (`log-0.log` + RTC-named `.log` files) one MCU boot at a time
/// so peak memory stays bounded to a single boot epoch (~200 MB instead of 15+ GB).
fn import_raw_dir_streaming_boots(
    raw_dir: &Path,
    track_day_id: &str,
    db: &Database,
    store_dir: &Path,
    report: &mut Per26ImportReport,
) -> Result<(), ImportError> {
    if !raw_dir.is_dir() {
        return Ok(());
    }
    let mut entries: Vec<PathBuf> = fs::read_dir(raw_dir)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().and_then(|s| s.to_str()) == Some("log"))
        .collect();
    entries.sort();

    // 1. Split `log-0.log` raw bytes into monotonic 16-byte frame slices without decoding
    let mut log0_segments: Vec<Vec<u8>> = Vec::new();
    let mut log0_mtime_ms: i64 = 0;
    let mut log0_sha256 = String::new();
    let mut rtc_paths: Vec<PathBuf> = Vec::new();

    for path in entries {
        let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if parse::parse_rtc_filename_ms(fname).is_some() {
            rtc_paths.push(path);
        } else {
            let bytes = fs::read(&path)?;
            let rec = source_file_record(&path);
            log0_mtime_ms = rec.mtime_unix_ms;
            log0_sha256 = rec.sha256_hex;
            let usable = bytes.len() - (bytes.len() % 16);
            let mut cur_seg = Vec::new();
            let mut prev_tick: Option<u32> = None;
            for frame_bytes in bytes[..usable].chunks_exact(16) {
                let tick = u32::from_le_bytes(frame_bytes[0..4].try_into().unwrap());
                if let Some(prev) = prev_tick
                    && (tick < prev
                        || tick.saturating_sub(prev) > crate::log_parse::consts::MAX_JUMP_MS)
                    && !cur_seg.is_empty()
                {
                    log0_segments.push(cur_seg);
                    cur_seg = Vec::new();
                }
                prev_tick = Some(tick);
                cur_seg.extend_from_slice(frame_bytes);
            }
            if !cur_seg.is_empty() {
                log0_segments.push(cur_seg);
            }
        }
    }

    // 2. Group RTC files into boot chains by checking only first/last 16-byte frame headers
    let mut rtc_boot_groups: Vec<Vec<PathBuf>> = Vec::new();
    let mut prev_rtc_ms: Option<i64> = None;
    let mut prev_last_tick: Option<u32> = None;

    for path in rtc_paths {
        let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let Some(rtc_ms) = parse::parse_rtc_filename_ms(fname) else {
            continue;
        };
        let meta = fs::metadata(&path)?;
        let len = meta.len() as usize;
        let usable = len - (len % 16);
        let (first_tick, last_tick) = if usable >= 16 {
            use std::io::{Read as _, Seek as _, SeekFrom};
            let mut f = fs::File::open(&path)?;
            let mut b0 = [0u8; 4];
            f.read_exact(&mut b0)?;
            let t0 = u32::from_le_bytes(b0);
            f.seek(SeekFrom::Start((usable - 16) as u64))?;
            let mut b1 = [0u8; 4];
            f.read_exact(&mut b1)?;
            let t1 = u32::from_le_bytes(b1);
            (Some(t0), Some(t1))
        } else {
            (None, None)
        };

        let starts_new_boot = if rtc_boot_groups.is_empty() {
            true
        } else {
            let rtc_gap_exceeded = prev_rtc_ms.is_some_and(|prev| {
                rtc_ms - prev > crate::log_parse::consts::RTC_FILE_ROTATION_MAX_GAP_MS
            });
            let tick_jump_exceeded = match (prev_last_tick, first_tick) {
                (Some(last_t), Some(first_t)) => {
                    first_t < last_t || first_t - last_t > crate::log_parse::consts::MAX_JUMP_MS
                }
                _ => false,
            };
            rtc_gap_exceeded || tick_jump_exceeded
        };

        if starts_new_boot {
            rtc_boot_groups.push(vec![path]);
        } else if let Some(cur) = rtc_boot_groups.last_mut() {
            cur.push(path);
        }
        prev_rtc_ms = Some(rtc_ms);
        if last_tick.is_some() {
            prev_last_tick = last_tick;
        }
    }

    let boot_count = log0_segments.len().max(rtc_boot_groups.len());
    for boot_idx in 0..boot_count {
        let mut boot_files: Vec<parse::SourceLogFile> = Vec::new();
        let mut boot_msgs: Vec<parse::ParsedMessage> = Vec::new();

        if let Some(seg_bytes) = log0_segments.get(boot_idx) {
            let (_, mut msgs) =
                parse::parse_log_bytes_on_bus(seg_bytes, "log-0.log", log0_mtime_ms, 0, "VCAN", db);
            boot_files.push(parse::SourceLogFile {
                file_name: "log-0.log".to_owned(),
                sha256_hex: log0_sha256.clone(),
                rtc_timestamp_ms: None,
                mtime_unix_ms: log0_mtime_ms,
            });
            boot_msgs.append(&mut msgs);
        }

        if let Some(paths) = rtc_boot_groups.get(boot_idx) {
            for path in paths {
                let fname = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown.log")
                    .to_owned();
                let bytes = fs::read(path)?;
                let rec = source_file_record(path);
                let file_idx = boot_files.len();
                let (meta, mut msgs) = parse::parse_log_bytes_on_bus(
                    &bytes,
                    &fname,
                    rec.mtime_unix_ms,
                    file_idx,
                    "VCAN",
                    db,
                );
                boot_files.push(meta);
                boot_msgs.append(&mut msgs);
            }
        }

        if boot_msgs.is_empty() {
            continue;
        }
        boot_msgs.sort_by_key(|m| m.timestamp);
        let mut correlated = correlate::time_correlate_chunks(vec![boot_msgs], &boot_files);
        correlated.retain(|c| {
            matches!(
                c.clock_mapping.confidence,
                correlate::ClockConfidence::GpsSynced { .. }
            )
        });
        if correlated.is_empty() {
            continue;
        }
        let s = storage::write_correlated_epochs_to_store_for_day(
            store_dir,
            &boot_files,
            correlated,
            db,
            Some(track_day_id),
            |_, _, _| {},
        )?;
        accumulate_summary(report, &s);
    }

    Ok(())
}

/// Import Track Day 10: `2026-09-06` (`1_no_cooling/raw` + `3_happy_laps_endurance_regen/raw`).
fn import_day_2026_09_06(
    per26_dir: &Path,
    store_dir: &Path,
    superdbc_dir: &Path,
    report: &mut Per26ImportReport,
) -> Result<(), ImportError> {
    let day_root = per26_dir.join("2026-09-06");
    let dbc_path = superdbc_dir.join("superdbc_2026_09_06.json");
    if !day_root.is_dir() || !dbc_path.is_file() {
        return Ok(());
    }
    let db =
        Database::load(&dbc_path).map_err(|e| ImportError::Io(io::Error::other(e.to_string())))?;
    let track_day_id = "2026-09-06";

    // 1. 1_no_cooling/raw
    if is_day_already_populated(store_dir, track_day_id, 3).is_none() {
        let no_cooling_raw = day_root.join("1_no_cooling/raw");
        import_raw_dir_streaming_boots(&no_cooling_raw, track_day_id, &db, store_dir, report)?;
    } else {
        report.partitions_skipped_existing += 3;
    }

    // 2. 3_happy_laps_endurance_regen/raw (streamed 1 boot at a time)
    if let Some((parts, anns)) = is_day_already_populated(store_dir, track_day_id, 10) {
        report.partitions_skipped_existing += parts.saturating_sub(3);
        report.annotations_created += anns;
    } else {
        let happy_raw = day_root.join("3_happy_laps_endurance_regen/raw");
        import_raw_dir_streaming_boots(&happy_raw, track_day_id, &db, store_dir, report)?;
    }

    let registry = Registry::open_store(store_dir)?;
    let named_sessions: &[(&str, &str, i64, i64)] = &[
        (
            "1_no_cooling",
            "Session: 1_no_cooling",
            1_788_719_691_252,
            1_788_721_507_146,
        ),
        (
            "2_regen",
            "Session: 2_regen",
            1_788_726_043_995,
            1_788_726_647_767,
        ),
        (
            "possible_endurance",
            "Session: PossibleEndurance",
            1_788_746_965_156,
            1_788_748_587_585,
        ),
        (
            "regen_test",
            "Session: RegenTest",
            1_788_749_379_804,
            1_788_751_127_098,
        ),
    ];
    for &(id_suf, label, start_ms, end_ms) in named_sessions {
        let ann = make_session_annotation(
            id_suf,
            track_day_id,
            label,
            &format!("PER26 2026-09-06 {label}"),
            start_ms,
            end_ms,
        );
        registry.upsert_annotation(&ann)?;
    }

    if let Some(mut td) = registry.get_track_day(track_day_id)? {
        td.title = "2026-09-06 — KMS No Cooling, Regen & Happy Laps Endurance".to_owned();
        td.location = "KMS".to_owned();
        registry.upsert_track_day(&td)?;
        report.track_days_imported += 1;
    }
    Ok(())
}

/// Process and load all datasets in `per26_dir` into canonical Parquet partitions in `store_dir`.
pub fn import_all_per26(
    per26_dir: &Path,
    store_dir: &Path,
    superdbc_dir: &Path,
) -> Result<Per26ImportReport, ImportError> {
    let mut report = Per26ImportReport::default();

    let mut run_or_skip =
        |day_ids: &[(&str, usize)],
         label: &str,
         f: fn(&Path, &Path, &Path, &mut Per26ImportReport) -> Result<(), ImportError>|
         -> Result<(), ImportError> {
            let mut all_present = true;
            let mut total_parts = 0usize;
            let mut total_anns = 0usize;
            for &(day_id, min_parts) in day_ids {
                if let Some((p, a)) = is_day_already_populated(store_dir, day_id, min_parts) {
                    total_parts += p;
                    total_anns += a;
                } else {
                    all_present = false;
                    break;
                }
            }
            if all_present {
                log::info!(
                    "Skipping PER26 {}: already populated ({} partitions, {} annotations)",
                    label,
                    total_parts,
                    total_anns
                );
                report.track_days_imported += day_ids.len();
                report.partitions_skipped_existing += total_parts;
                report.annotations_created += total_anns;
                Ok(())
            } else {
                log::info!("Importing PER26 {label}...");
                f(per26_dir, store_dir, superdbc_dir, &mut report)
            }
        };

    run_or_skip(
        &[("2025-09-20", 10)],
        "[1/9]: 2025-09-20-KMS",
        import_day_2025_09_20,
    )?;
    run_or_skip(
        &[("2026-04-05", 3)],
        "[2/9]: 2026-04-05-KMS",
        import_day_2026_04_05,
    )?;
    run_or_skip(
        &[("2026-04-10", 13)],
        "[3/9]: 2026-04-10-DYNO",
        import_day_2026_04_10,
    )?;
    run_or_skip(
        &[("2026-04-11", 15)],
        "[4/9]: 2026-04-11-KMS",
        import_day_2026_04_11,
    )?;
    run_or_skip(
        &[("2026-04-12", 8)],
        "[5/9]: 2026-04-12-KMS",
        import_day_2026_04_12,
    )?;
    run_or_skip(
        &[("2026-05-03", 8), ("2026-05-10", 8)],
        "[6/9]: 2026-05-KMS (May 3 & May 10)",
        import_days_2026_05,
    )?;
    run_or_skip(
        &[("2026-06-14", 1)],
        "[7/9]: 2026-06-14",
        import_day_2026_06_14,
    )?;
    run_or_skip(
        &[("2026-09-05", 2)],
        "[8/9]: 2026-09-05",
        import_day_2026_09_05,
    )?;

    log::info!("Importing PER26 [9/9]: 2026-09-06...");
    import_day_2026_09_06(per26_dir, store_dir, superdbc_dir, &mut report)?;

    let local_db = store_dir.join(storage::LOCAL_REGISTRY_FILENAME);
    let cloud_db = store_dir.join("cloud_registry.sqlite");
    if local_db.is_file() {
        let _ = fs::copy(&local_db, &cloud_db);
    }

    Ok(report)
}
