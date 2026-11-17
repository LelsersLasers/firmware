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
    fault_severity: Option<auto_annotate::FaultSeverity>,
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
                let fault_severity = if is_fault_flag {
                    auto_annotate::FaultSeverity::resolve_for_signal(
                        &signal_id,
                        sig.fault_priority(),
                    )
                } else {
                    None
                };
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
                        fault_severity,
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
        .unwrap_or(match lower.as_str() {
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
            let signal_id = storage::format_signal_id(msg_name, sig_name);
            let fault_severity = if is_fault_flag {
                auto_annotate::FaultSeverity::resolve_for_signal(&signal_id, None)
            } else {
                None
            };
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
                signal_id,
                unit: csv_unit.to_owned(),
                description: csv_desc.to_owned(),
                is_discrete: is_fault_flag,
                is_fault_flag,
                fault_severity,
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
            _ => ms_str.get(..3)?.parse::<u32>().ok()?,
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
    type ParsedCsvRow = (Option<i64>, u32, SignalId, f64, i64, Option<String>);
    let lookup = build_signal_lookup(database);
    let mut raw_rows: Vec<ParsedCsvRow> = Vec::new();
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
                fault_severity: m.fault_severity,
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
        let Some(first_b) = bytes.get(0..4).and_then(|s| s.try_into().ok()) else {
            continue;
        };
        let Some(last_b) = bytes
            .get((n - 1) * 16..(n - 1) * 16 + 4)
            .and_then(|s| s.try_into().ok())
        else {
            continue;
        };
        let first_tick = u32::from_le_bytes(first_b);
        let last_tick = u32::from_le_bytes(last_b);
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
            let Some(prev) = chain.last() else {
                continue;
            };
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
                if let Some(utc_ms) = extract_gps_point_from_cells(&cells, &header.gps_cols, 2025)
                    && gps_pts.last().map(|&(_, u)| u) != Some(utc_ms)
                {
                    gps_pts.push((ticks_ms, utc_ms));
                }
            }
            if gps_pts.len() < 10 {
                continue;
            }
            let (Some(first_pt), Some(last_pt)) = (gps_pts.first(), gps_pts.last()) else {
                continue;
            };
            let dt_daq = (last_pt.0 as f64 - first_pt.0 as f64) / 1000.0;
            let dt_gps = (last_pt.1 as f64 - first_pt.1 as f64) / 1000.0;
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
        if let Some(cur) = sessions.last_mut()
            && let Some(prev) = cur.last()
            && (blk.gps_start_ms - prev.gps_start_ms).abs() <= 180_000
        {
            cur.push(blk);
            continue;
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
                                fault_severity: spec.meta.fault_severity,
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

    let date_0503 = NaiveDate::from_ymd_opt(2026, 5, 3).expect("valid literal date 2026-05-03");
    let p1 = may_root.join("may3_part1");
    let p2 = may_root.join("may3_part2");
    let p3 = may_root.join("may3_part3");

    type CsvBootSpec<'a> = (&'a str, &'a str, Vec<(PathBuf, Option<(f64, f64)>)>);
    let may3_boots: &[CsvBootSpec<'_>] = &[
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
    let date_0510 = NaiveDate::from_ymd_opt(2026, 5, 10).expect("valid literal date 2026-05-10");
    let hunter = may_root.join("may10-hunter-runs");
    let accel = may_root.join("may10-accel-skids");

    let may10_boots: &[CsvBootSpec<'_>] = &[
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
    let date_0614 = NaiveDate::from_ymd_opt(2026, 6, 14).expect("valid literal date 2026-06-14");
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
    let date_0905 = NaiveDate::from_ymd_opt(2026, 9, 5).expect("valid literal date 2026-09-05");

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
            let (frame_chunks, _) = bytes[..usable].as_chunks::<16>();
            for frame_bytes in frame_chunks {
                let (tick_b, _) = frame_bytes
                    .split_first_chunk::<4>()
                    .unwrap_or((&[0; 4], &[]));
                let tick = u32::from_le_bytes(*tick_b);
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

    // Archive all era-specific SuperDBC artifacts by their SHA-256 content_hash into `<store_dir>/superdbcs/`
    if let Ok(entries) = fs::read_dir(superdbc_dir) {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json")
                && let Ok(db) = Database::load(&path)
            {
                let _ = storage::archive_superdbc_to_store(store_dir, &db);
            }
        }
    }

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

    let canonical_dbc_path = superdbc_dir.join("superdbc_per26.json");
    if canonical_dbc_path.is_file() {
        let _ = normalize_per26_store(store_dir, &canonical_dbc_path)?;
    }

    if let Ok(reg) = Registry::open_store(store_dir) {
        let _ = reg.checkpoint_wal();
    }
    let local_db = store_dir.join(storage::LOCAL_REGISTRY_FILENAME);
    let cloud_db = store_dir.join("cloud_registry.sqlite");
    if local_db.is_file() {
        let _ = fs::remove_file(store_dir.join("cloud_registry.sqlite-wal"));
        let _ = fs::remove_file(store_dir.join("cloud_registry.sqlite-shm"));
        let _ = fs::copy(&local_db, &cloud_db);
    }

    Ok(report)
}

/// Compute the median of finite physical values in `samples`.
fn median_physical(samples: &[RawSample]) -> Option<f64> {
    let mut vals: Vec<f64> = samples
        .iter()
        .map(|s| s.physical)
        .filter(|v| v.is_finite())
        .collect();
    if vals.is_empty() {
        return None;
    }
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(vals[vals.len() / 2])
}

/// Sanitize samples in `acc` by replacing any sample outside `[min_ok, max_ok]` with the nearest
/// valid sample (or `fallback` if no valid sample exists).
fn sanitize_column_bounds(
    acc: &mut SignalColumnAccumulator,
    min_ok: f64,
    max_ok: f64,
    fallback: f64,
) {
    let first_valid = acc
        .samples
        .iter()
        .map(|s| s.physical)
        .find(|&v| v.is_finite() && (min_ok..=max_ok).contains(&v))
        .unwrap_or(fallback);
    let mut last_valid = first_valid;
    for sample in &mut acc.samples {
        if sample.physical.is_finite() && (min_ok..=max_ok).contains(&sample.physical) {
            last_valid = sample.physical;
        } else {
            sample.physical = last_valid;
            sample.raw_int = last_valid.round().clamp(i64::MIN as f64, i64::MAX as f64) as i64;
        }
    }
}

/// Build a right-handed orthonormal 3x3 rotation matrix `[R_X; R_Y; R_Z]` that maps the measured
/// stationary gravity vector `(med_ax, med_ay, med_az)` to `(0, 0, -1)` and aligns `y_seed` with `+Y`.
fn build_imu_rotation_matrix(
    med_ax: f64,
    med_ay: f64,
    med_az: f64,
    y_seed: [f64; 3],
) -> ([[f64; 3]; 3], f64) {
    let g_norm = (med_ax * med_ax + med_ay * med_ay + med_az * med_az)
        .sqrt()
        .max(1e-6);
    // At rest in SAE J670 Z-up convention, accelerometer reads (0, 0, -1 g), so R_Z · g_med = -g_norm
    let rz = [-med_ax / g_norm, -med_ay / g_norm, -med_az / g_norm];
    let dot_yz = y_seed[0] * rz[0] + y_seed[1] * rz[1] + y_seed[2] * rz[2];
    let mut ry = [
        y_seed[0] - dot_yz * rz[0],
        y_seed[1] - dot_yz * rz[1],
        y_seed[2] - dot_yz * rz[2],
    ];
    let ry_norm = (ry[0] * ry[0] + ry[1] * ry[1] + ry[2] * ry[2])
        .sqrt()
        .max(1e-6);
    ry[0] /= ry_norm;
    ry[1] /= ry_norm;
    ry[2] /= ry_norm;

    // Right-handed cross product: R_X = R_Y × R_Z
    let rx = [
        ry[1] * rz[2] - ry[2] * rz[1],
        ry[2] * rz[0] - ry[0] * rz[2],
        ry[0] * rz[1] - ry[1] * rz[0],
    ];
    let scale = if (0.75..=1.25).contains(&g_norm) {
        1.0 / g_norm
    } else {
        1.0
    };
    ([rx, ry, rz], scale)
}

/// Align a 3-axis vector signal `(x_id, y_id, z_id)` on a unified timestamp grid and apply the
/// 3x3 rotation matrix `rot` (and optional scalar `scale`).
fn rotate_3axis_columns(
    mapped: &mut BTreeMap<SignalId, SignalColumnAccumulator>,
    x_id: &str,
    y_id: &str,
    z_id: &str,
    rot: [[f64; 3]; 3],
    scale: f64,
) {
    let (Some(x_acc), Some(y_acc), Some(z_acc)) =
        (mapped.get(x_id), mapped.get(y_id), mapped.get(z_id))
    else {
        return;
    };
    let xs = &x_acc.samples;
    let ys = &y_acc.samples;
    let zs = &z_acc.samples;
    if xs.is_empty() || ys.is_empty() || zs.is_empty() {
        return;
    }

    // Fast path when all 3 axes already share identical timestamps
    let same_grid = xs.len() == ys.len()
        && xs.len() == zs.len()
        && xs
            .iter()
            .zip(ys.iter())
            .zip(zs.iter())
            .all(|((a, b), c)| a.utc_ms == b.utc_ms && a.utc_ms == c.utc_ms);

    let mut out_x = Vec::with_capacity(xs.len());
    let mut out_y = Vec::with_capacity(ys.len());
    let mut out_z = Vec::with_capacity(zs.len());

    let apply = |vx: f64, vy: f64, vz: f64| -> (f64, f64, f64) {
        let rx = (rot[0][0] * vx + rot[0][1] * vy + rot[0][2] * vz) * scale;
        let ry = (rot[1][0] * vx + rot[1][1] * vy + rot[1][2] * vz) * scale;
        let rz = (rot[2][0] * vx + rot[2][1] * vy + rot[2][2] * vz) * scale;
        (rx, ry, rz)
    };

    if same_grid {
        for i in 0..xs.len() {
            let (rx, ry, rz) = apply(xs[i].physical, ys[i].physical, zs[i].physical);
            let utc_ms = xs[i].utc_ms;
            let ticks_ms = xs[i].ticks_ms;
            out_x.push(RawSample {
                utc_ms,
                ticks_ms,
                physical: rx,
                raw_int: rx.round() as i64,
                enum_label: None,
            });
            out_y.push(RawSample {
                utc_ms,
                ticks_ms,
                physical: ry,
                raw_int: ry.round() as i64,
                enum_label: None,
            });
            out_z.push(RawSample {
                utc_ms,
                ticks_ms,
                physical: rz,
                raw_int: rz.round() as i64,
                enum_label: None,
            });
        }
    } else {
        // Merge-align on utc_ms with zero-order hold (needed for 2025-09-20 deduplicated CSV columns)
        let (mut ix, mut iy, mut iz) = (0usize, 0usize, 0usize);
        let mut cur_x = xs[0].physical;
        let mut cur_y = ys[0].physical;
        let mut cur_z = zs[0].physical;

        while ix < xs.len() || iy < ys.len() || iz < zs.len() {
            let tx = xs.get(ix).map(|s| s.utc_ms).unwrap_or(i64::MAX);
            let ty = ys.get(iy).map(|s| s.utc_ms).unwrap_or(i64::MAX);
            let tz = zs.get(iz).map(|s| s.utc_ms).unwrap_or(i64::MAX);
            let utc_ms = tx.min(ty).min(tz);
            let mut ticks_ms = 0;

            while let Some(s) = xs.get(ix) {
                if s.utc_ms == utc_ms {
                    cur_x = s.physical;
                    ticks_ms = s.ticks_ms;
                    ix += 1;
                } else {
                    break;
                }
            }
            while let Some(s) = ys.get(iy) {
                if s.utc_ms == utc_ms {
                    cur_y = s.physical;
                    ticks_ms = s.ticks_ms;
                    iy += 1;
                } else {
                    break;
                }
            }
            while let Some(s) = zs.get(iz) {
                if s.utc_ms == utc_ms {
                    cur_z = s.physical;
                    ticks_ms = s.ticks_ms;
                    iz += 1;
                } else {
                    break;
                }
            }

            let (rx, ry, rz) = apply(cur_x, cur_y, cur_z);
            out_x.push(RawSample {
                utc_ms,
                ticks_ms,
                physical: rx,
                raw_int: rx.round() as i64,
                enum_label: None,
            });
            out_y.push(RawSample {
                utc_ms,
                ticks_ms,
                physical: ry,
                raw_int: ry.round() as i64,
                enum_label: None,
            });
            out_z.push(RawSample {
                utc_ms,
                ticks_ms,
                physical: rz,
                raw_int: rz.round() as i64,
                enum_label: None,
            });
        }
    }

    if let Some(acc) = mapped.get_mut(x_id) {
        acc.samples = out_x;
    }
    if let Some(acc) = mapped.get_mut(y_id) {
        acc.samples = out_y;
    }
    if let Some(acc) = mapped.get_mut(z_id) {
        acc.samples = out_z;
    }
}

/// Normalize all legacy PER26 / PER25 signal IDs, physical units, and scaling factors in `columns`
/// to match the canonical `superdbc_per26.json` schema.
pub fn normalize_per26_columns_to_canonical(
    columns: &mut BTreeMap<SignalId, SignalColumnAccumulator>,
    track_day_id: &str,
    canonical_dbc: &Database,
) {
    let lookup = build_signal_lookup(canonical_dbc);
    let mut canonical_by_id: HashMap<String, ResolvedSignalMeta> = HashMap::new();
    for meta in lookup.into_values() {
        canonical_by_id.insert(meta.signal_id.clone(), meta);
    }

    let old_columns = std::mem::take(columns);
    let had_max_temp = old_columns.contains_key("pack_stats.max_temp")
        || old_columns.contains_key("max_cell_temp.max_temp");

    let mut mapped: BTreeMap<SignalId, SignalColumnAccumulator> = BTreeMap::new();

    for (old_id, mut acc) in old_columns {
        let is_raw_adc_shockpot = matches!(
            old_id.as_str(),
            "front_shockpots.left"
                | "front_shockpots.right"
                | "rear_shockpots.left"
                | "rear_shockpots.right"
        ) && (acc.unit.contains("ADC")
            || acc.samples.iter().any(|s| s.physical > 250.0));

        let is_unscaled_2025_tq = track_day_id == "2025-09-20"
            && matches!(
                old_id.as_str(),
                "vcu_torque_request.rear_left" | "vcu_torque_request.rear_right"
            )
            && acc
                .samples
                .iter()
                .map(|s| s.physical)
                .fold(0.0_f64, f64::max)
                <= 105.0;

        let is_inverted_2025_sdc = track_day_id == "2025-09-20"
            && old_id.starts_with("main_module_fault_sync.SDC")
            && old_id != "main_module_fault_sync.SDC17_AIR_M"
            && median_physical(&acc.samples).unwrap_or(0.0) > 0.5;

        type TransformFn = fn(f64) -> f64;
        let (target_id, transform): (String, TransformFn) = match old_id.as_str() {
            // 1. Shockpots (2025-09-20 zero-centered deflection -> canonical length mm, and 2026-04-05 ADC -> mm)
            "shock_front.left_shock" => ("front_shockpots.left".to_owned(), |v| 188.0 + v),
            "shock_front.right_shock" => ("front_shockpots.right".to_owned(), |v| 188.0 + v),
            "shock_rear.left_shock" => ("rear_shockpots.left".to_owned(), |v| 175.0 + v),
            "shock_rear.right_shock" => ("rear_shockpots.right".to_owned(), |v| 175.0 + v),
            "front_shockpots.left"
            | "front_shockpots.right"
            | "rear_shockpots.left"
            | "rear_shockpots.right"
                if is_raw_adc_shockpot =>
            {
                (old_id, |v| {
                    145.0 + ((4095.0 - v.clamp(0.0, 4095.0)) / 4095.0) * 75.0
                })
            }

            // 2. IMU Acceleration (m/s^2 -> g) & Angular Rate (rad/s -> deg/s)
            "imu_accel.imu_accel_x" => ("IMU_acceleration.X_axis".to_owned(), |v| v / 9.80665),
            "imu_accel.imu_accel_y" => ("IMU_acceleration.Y_axis".to_owned(), |v| v / 9.80665),
            "imu_accel.imu_accel_z" => ("IMU_acceleration.Z_axis".to_owned(), |v| v / 9.80665),
            "imu_gyro.imu_gyro_x" => ("IMU_angular_rate.X_axis".to_owned(), |v| {
                v * (180.0 / std::f64::consts::PI)
            }),
            "imu_gyro.imu_gyro_y" => ("IMU_angular_rate.Y_axis".to_owned(), |v| {
                v * (180.0 / std::f64::consts::PI)
            }),
            "imu_gyro.imu_gyro_z" => ("IMU_angular_rate.Z_axis".to_owned(), |v| {
                v * (180.0 / std::f64::consts::PI)
            }),

            // 3. GPS Coordinates (1e-7 deg integer -> deg), Speed & Heading
            "gps_coordinates.latitude" | "gps_coordinates.longitude" => {
                (old_id, |v| if v.abs() > 1000.0 { v * 1e-7 } else { v })
            }
            "gps_speed.gps_speed" => ("gps_speed_heading.gps_speed".to_owned(), |v| v),
            "gps_speed.gps_heading" => ("gps_speed_heading.gps_heading".to_owned(), |v| v),

            // 4. Bosch LWS Steering Sensor
            "LWS_Standard.Cal" => ("LWS_Standard.CAL".to_owned(), |v| v),
            "LWS_Standard.Ok" => ("LWS_Standard.OK".to_owned(), |v| v),
            "LWS_Standard.Trim" => ("LWS_Standard.TRIM".to_owned(), |v| v),
            "LWS_Standard.Reserved_1" => ("LWS_Standard.reserved1".to_owned(), |v| v),
            "LWS_Standard.Reserved_2" => ("LWS_Standard.reserved2".to_owned(), |v| v),

            // 5. Pedals
            "filt_throttle_brake.throttle" => ("pedals.throttle".to_owned(), |v| v),
            "filt_throttle_brake.brake" => ("pedals.brake".to_owned(), |v| v),

            // 6. Wheel Speeds (rad/s -> motor shaft rpm)
            "rear_wheel_speeds.left_speed_sensor" => ("wheel_speeds.rear_left".to_owned(), |v| {
                v * (60.0 / (2.0 * std::f64::consts::PI)) * 11.86
            }),
            "rear_wheel_speeds.right_speed_sensor" => ("wheel_speeds.rear_right".to_owned(), |v| {
                v * (60.0 / (2.0 * std::f64::consts::PI)) * 11.86
            }),

            // 7. Pack Stats, Accumulator & Charging
            "orion_currents_volts.pack_voltage" => ("pack_stats.pack_voltage".to_owned(), |v| v),
            "orion_currents_volts.pack_current" => ("pack_stats.pack_current".to_owned(), |v| v),
            "max_cell_temp.max_temp" => ("pack_stats.max_temp".to_owned(), |v| v),
            "pack_stats.avg_temp" => {
                if had_max_temp {
                    continue;
                }
                ("pack_stats.max_temp".to_owned(), |v| v)
            }
            "elcon_charger_command.voltage_limit" => {
                ("elcon_command.voltage_limit".to_owned(), |v| v)
            }
            "elcon_charger_command.current_limit" => {
                ("elcon_command.current_limit".to_owned(), |v| v)
            }
            "elcon_charger_command.charge_disable" => {
                ("elcon_command.charger_disable".to_owned(), |v| v)
            }
            "pack_charge_status.voltage" => ("charging_telemetry.pack_voltage".to_owned(), |v| v),
            "pack_charge_status.current" => ("charging_telemetry.pack_current".to_owned(), |v| v),

            // 8. Torque Requests (Nm -> % Mn where Mn = 9.8 Nm) & Gearbox Oil Temps
            "VCU_torques_speeds.TO_VT_left" => ("vcu_torque_request.rear_left".to_owned(), |v| {
                v * (100.0 / 9.8)
            }),
            "VCU_torques_speeds.TO_VT_right" => ("vcu_torque_request.rear_right".to_owned(), |v| {
                v * (100.0 / 9.8)
            }),
            "vcu_torque_request.rear_left" | "vcu_torque_request.rear_right"
                if is_unscaled_2025_tq =>
            {
                (old_id, |v| v * (21.0 / 9.8))
            }
            "gearbox.l_temp" => ("rear_oil_temps.left".to_owned(), |v| v),
            "gearbox.r_temp" => ("rear_oil_temps.right".to_owned(), |v| v),

            // 9. Coolant Outputs, SDC Fault Sync (invert 2025 sdc_status tap HIGH=OK -> 0=NO_FAULT), Start Button & Precharge
            "coolant_out.dt_pump" => ("coolant_out.is_powertrain_pumps_enabled".to_owned(), |v| {
                if v > 0.0 { 1.0 } else { 0.0 }
            }),
            "coolant_out.dt_fan" => ("coolant_out.is_hx_fan_enabled".to_owned(), |v| {
                if v > 0.0 { 1.0 } else { 0.0 }
            }),
            "coolant_out.bat_fan" => ("coolant_out.is_batt_fans_enabled".to_owned(), |v| {
                if v > 0.0 { 1.0 } else { 0.0 }
            }),
            "sdc_status.IMD" => ("main_module_fault_sync.SDC1_IMD".to_owned(), |v| {
                if v > 0.5 { 0.0 } else { 1.0 }
            }),
            "sdc_status.BMS" => ("main_module_fault_sync.SDC2_BMS".to_owned(), |v| {
                if v > 0.5 { 0.0 } else { 1.0 }
            }),
            "sdc_status.BSPD" => ("main_module_fault_sync.SDC3_BSPD".to_owned(), |v| {
                if v > 0.5 { 0.0 } else { 1.0 }
            }),
            "sdc_status.main" => ("main_module_fault_sync.SDC4_MAIN_OK".to_owned(), |v| {
                if v > 0.5 { 0.0 } else { 1.0 }
            }),
            "sdc_status.BOTS" => ("main_module_fault_sync.SDC5_BOTS".to_owned(), |v| {
                if v > 0.5 { 0.0 } else { 1.0 }
            }),
            "sdc_status.inertia" => ("main_module_fault_sync.SDC6_INERTIA".to_owned(), |v| {
                if v > 0.5 { 0.0 } else { 1.0 }
            }),
            "sdc_status.c_estop" => (
                "main_module_fault_sync.SDC7_COCKPIT_ESTOP".to_owned(),
                |v| {
                    if v > 0.5 { 0.0 } else { 1.0 }
                },
            ),
            "sdc_status.r_estop" => ("main_module_fault_sync.SDC10_RIGHT_ESTOP".to_owned(), |v| {
                if v > 0.5 { 0.0 } else { 1.0 }
            }),
            "sdc_status.l_estop" => ("main_module_fault_sync.SDC11_LEFT_ESTOP".to_owned(), |v| {
                if v > 0.5 { 0.0 } else { 1.0 }
            }),
            "sdc_status.HVD" => ("main_module_fault_sync.SDC12_MSD".to_owned(), |v| {
                if v > 0.5 { 0.0 } else { 1.0 }
            }),
            "sdc_status.emeter" => ("main_module_fault_sync.SDC13_E_METER".to_owned(), |v| {
                if v > 0.5 { 0.0 } else { 1.0 }
            }),
            "sdc_status.hub" => (
                "main_module_fault_sync.SDC15_REAR_INTERLOCK".to_owned(),
                |v| if v > 0.5 { 0.0 } else { 1.0 },
            ),
            "sdc_status.TSMS" => ("main_module_fault_sync.SDC16_TSMS".to_owned(), |v| {
                if v > 0.5 { 0.0 } else { 1.0 }
            }),
            "sdc_status.pchg_out" => ("main_module_fault_sync.SDC17_AIR_M".to_owned(), |v| v),
            "main_hb.precharge_state" => ("main_hb_amk.precharge_state".to_owned(), |v| v),
            "start_button.start" => ("start_button.is_pressed".to_owned(), |v| v),
            "drive_modes.VT_mode" => ("vcu_driver_request.vcu_mode".to_owned(), |v| {
                if v > 1.5 { 2.0 } else { 4.0 }
            }),

            other => {
                if is_inverted_2025_sdc {
                    (other.to_owned(), |v| if v > 0.5 { 0.0 } else { 1.0 })
                } else if let Some(rest) = other.strip_prefix("INVA_LOG_SET.") {
                    (format!("INVA_SET.{rest}"), |v| v)
                } else if let Some(rest) = other.strip_prefix("INVB_LOG_SET.") {
                    (format!("INVB_SET.{rest}"), |v| v)
                } else {
                    (old_id, |v| v)
                }
            }
        };

        if !canonical_by_id.contains_key(&target_id) {
            continue;
        }

        for sample in &mut acc.samples {
            sample.physical = transform(sample.physical);
            sample.raw_int = sample
                .physical
                .round()
                .clamp(i64::MIN as f64, i64::MAX as f64) as i64;
        }

        mapped.entry(target_id).or_insert(acc);
    }

    // Sanitize raw IZZE IMU power-on self-test ringdown spikes before mirroring/rotating
    for sig in [
        "IZZE_acceleration.X_axis",
        "IZZE_acceleration.Y_axis",
        "IZZE_acceleration.Z_axis",
    ] {
        if let Some(acc) = mapped.get_mut(sig) {
            sanitize_column_bounds(acc, -3.5, 3.5, 0.0);
        }
    }
    for sig in [
        "IZZE_angular_rate.X_axis",
        "IZZE_angular_rate.Y_axis",
        "IZZE_angular_rate.Z_axis",
    ] {
        if let Some(acc) = mapped.get_mut(sig) {
            sanitize_column_bounds(acc, -250.0, 250.0, 0.0);
        }
    }

    // If IMU_acceleration is missing or was previously an un-rotated copy of IZZE_acceleration (e.g. April 2026),
    // re-seed IMU_acceleration and IMU_angular_rate from IZZE_acceleration and IZZE_angular_rate so we can rotate cleanly.
    let imu_needs_izze_seed = mapped.contains_key("IZZE_acceleration.Z_axis")
        && (!mapped.contains_key("IMU_acceleration.Z_axis")
            || mapped
                .get("IMU_acceleration.Z_axis")
                .and_then(|a| median_physical(&a.samples))
                .is_some_and(|med_z| med_z > -0.75)
            || mapped
                .get("IMU_acceleration.Y_axis")
                .and_then(|a| median_physical(&a.samples))
                .is_some_and(|med_y| med_y.abs() > 0.25));

    if imu_needs_izze_seed {
        for (src_id, dst_id) in [
            ("IZZE_acceleration.X_axis", "IMU_acceleration.X_axis"),
            ("IZZE_acceleration.Y_axis", "IMU_acceleration.Y_axis"),
            ("IZZE_acceleration.Z_axis", "IMU_acceleration.Z_axis"),
            (
                "IZZE_acceleration.temperature",
                "IMU_acceleration.temperature",
            ),
            ("IZZE_angular_rate.X_axis", "IMU_angular_rate.X_axis"),
            ("IZZE_angular_rate.Y_axis", "IMU_angular_rate.Y_axis"),
            ("IZZE_angular_rate.Z_axis", "IMU_angular_rate.Z_axis"),
            ("IZZE_angular_rate.reserved", "IMU_angular_rate.reserved"),
        ] {
            if let Some(src_acc) = mapped.get(src_id).cloned() {
                mapped.insert(dst_id.to_owned(), src_acc);
            }
        }
    }

    // Sanitize IMU_acceleration and IMU_angular_rate power-on self-test spikes
    for sig in [
        "IMU_acceleration.X_axis",
        "IMU_acceleration.Y_axis",
        "IMU_acceleration.Z_axis",
    ] {
        if let Some(acc) = mapped.get_mut(sig) {
            sanitize_column_bounds(acc, -3.5, 3.5, 0.0);
        }
    }
    for sig in [
        "IMU_angular_rate.X_axis",
        "IMU_angular_rate.Y_axis",
        "IMU_angular_rate.Z_axis",
    ] {
        if let Some(acc) = mapped.get_mut(sig) {
            sanitize_column_bounds(acc, -250.0, 250.0, 0.0);
        }
    }

    // Calibrate and rotate IMU_acceleration & IMU_angular_rate into canonical SAE J670 Z-up vehicle frame
    // (X+ longitudinal, Y+ lateral, Z+ vertical with stationary gravity = [0, 0, -1 g]).
    if let (Some(med_ax), Some(med_ay), Some(med_az)) = (
        mapped
            .get("IMU_acceleration.X_axis")
            .and_then(|a| median_physical(&a.samples)),
        mapped
            .get("IMU_acceleration.Y_axis")
            .and_then(|a| median_physical(&a.samples)),
        mapped
            .get("IMU_acceleration.Z_axis")
            .and_then(|a| median_physical(&a.samples)),
    ) {
        let is_unrotated_sensor_frame =
            med_az > -0.75 || med_ay.abs() > 0.25 || med_ax.abs() > 0.25;
        let has_residual_tilt = med_ax.hypot(med_ay) > 0.005 || (med_az + 1.0).abs() > 0.005;

        if is_unrotated_sensor_frame || has_residual_tilt {
            let y_seed = if is_unrotated_sensor_frame {
                if track_day_id == "2025-09-20" {
                    // PER25 BMI088 on torque_vector PCB: sensor +X points laterally (+Y_vehicle)
                    [1.0, 0.0, 0.0]
                } else {
                    // PER26 IZZE IMU on chassis: yaw = -90 deg so sensor -X points laterally (+Y_vehicle)
                    [-1.0, 0.0, 0.0]
                }
            } else {
                // Already in vehicle frame: preserve yaw orientation around Z while leveling residual pitch/roll
                [0.0, 1.0, 0.0]
            };

            let (rot, accel_scale) = build_imu_rotation_matrix(med_ax, med_ay, med_az, y_seed);
            rotate_3axis_columns(
                &mut mapped,
                "IMU_acceleration.X_axis",
                "IMU_acceleration.Y_axis",
                "IMU_acceleration.Z_axis",
                rot,
                accel_scale,
            );
            rotate_3axis_columns(
                &mut mapped,
                "IMU_angular_rate.X_axis",
                "IMU_angular_rate.Y_axis",
                "IMU_angular_rate.Z_axis",
                rot,
                1.0,
            );
        }

        // Remove any residual stationary gyro zero-rate bias
        for gyro_sig in [
            "IMU_angular_rate.X_axis",
            "IMU_angular_rate.Y_axis",
            "IMU_angular_rate.Z_axis",
        ] {
            if let Some(acc) = mapped.get_mut(gyro_sig)
                && let Some(bias) = median_physical(&acc.samples)
                && bias.abs() > 0.01
                && bias.abs() < 5.0
            {
                for s in &mut acc.samples {
                    s.physical -= bias;
                    s.raw_int = s.physical.round() as i64;
                }
            }
        }
    }

    // On 2025-09-20, replace noisy upright hall-sensor wheel_speeds.rear_{left,right} with clean AMK resolver speeds
    if track_day_id == "2025-09-20" {
        if let Some(inva_spd) = mapped.get("INVA_CRIT.AMK_ActualSpeed").cloned() {
            mapped.insert("wheel_speeds.front_left".to_owned(), inva_spd.clone());
            mapped.insert("wheel_speeds.rear_left".to_owned(), inva_spd);
        }
        if let Some(invb_spd) = mapped.get("INVB_CRIT.AMK_ActualSpeed").cloned() {
            mapped.insert("wheel_speeds.front_right".to_owned(), invb_spd.clone());
            mapped.insert("wheel_speeds.rear_right".to_owned(), invb_spd);
        }
    }

    // Cross-mirror equivalent sensor pairs so every track day has complete coverage for all presets & formulas
    let mirror_pairs: &[(&str, &str)] = &[
        (
            "IZZE_acceleration.temperature",
            "IMU_acceleration.temperature",
        ),
        ("IZZE_angular_rate.reserved", "IMU_angular_rate.reserved"),
        ("LWS_Standard.LWS_ANGLE", "steering_angle.angle"),
        ("steering_angle.angle", "LWS_Standard.LWS_ANGLE"),
        ("LWS_Standard.LWS_SPEED", "steering_angle.angular_rate"),
        ("steering_angle.angular_rate", "LWS_Standard.LWS_SPEED"),
        ("LWS_Standard.OK", "steering_angle.data_valid"),
        ("steering_angle.data_valid", "LWS_Standard.OK"),
        ("INVA_CRIT.AMK_ActualSpeed", "wheel_speeds.front_left"),
        ("INVB_CRIT.AMK_ActualSpeed", "wheel_speeds.front_right"),
        ("INVC_CRIT.AMK_ActualSpeed", "wheel_speeds.rear_left"),
        ("INVD_CRIT.AMK_ActualSpeed", "wheel_speeds.rear_right"),
        ("front_shockpots.left", "rear_shockpots.left"),
        ("front_shockpots.right", "rear_shockpots.right"),
        ("pack_stats.pack_voltage", "pack_stats.vbatt_adc"),
        ("INVA_TEMPS.AMK_IGBTTemp", "INVC_TEMPS.AMK_IGBTTemp"),
        ("INVB_TEMPS.AMK_IGBTTemp", "INVD_TEMPS.AMK_IGBTTemp"),
        ("INVA_TEMPS.AMK_InverterTemp", "INVC_TEMPS.AMK_InverterTemp"),
        ("INVB_TEMPS.AMK_InverterTemp", "INVD_TEMPS.AMK_InverterTemp"),
        ("INVA_TEMPS.AMK_MotorTemp", "INVC_TEMPS.AMK_MotorTemp"),
        ("INVB_TEMPS.AMK_MotorTemp", "INVD_TEMPS.AMK_MotorTemp"),
    ];

    for &(src_id, dst_id) in mirror_pairs {
        if !mapped.contains_key(dst_id)
            && canonical_by_id.contains_key(dst_id)
            && let Some(src_acc) = mapped.get(src_id)
        {
            mapped.insert(
                dst_id.to_owned(),
                SignalColumnAccumulator {
                    unit: src_acc.unit.clone(),
                    is_discrete: src_acc.is_discrete,
                    is_fault_flag: src_acc.is_fault_flag,
                    fault_severity: src_acc.fault_severity,
                    bus_name: src_acc.bus_name.clone(),
                    bus_id: src_acc.bus_id,
                    can_id: src_acc.can_id,
                    transmitter: src_acc.transmitter.clone(),
                    nominal_period_ms: src_acc.nominal_period_ms,
                    description: src_acc.description.clone(),
                    samples: src_acc.samples.clone(),
                },
            );
        }
    }

    // Populate vcu_torque_request.{front_left,front_right,rear_left,rear_right} from MCAN AMK_TorqueSetpoint
    // when vcu_torque_request is absent (April 2026) or all-zero (2026-05-03 direct throttle mode).
    let vcu_tq_all_zero = [
        "vcu_torque_request.front_left",
        "vcu_torque_request.front_right",
        "vcu_torque_request.rear_left",
        "vcu_torque_request.rear_right",
    ]
    .iter()
    .all(|k| {
        mapped
            .get(*k)
            .is_none_or(|a| a.samples.iter().all(|s| s.physical.abs() < 1e-6))
    });
    if vcu_tq_all_zero {
        for (src_id, dst_id) in [
            (
                "INVB_SET.AMK_TorqueSetpoint",
                "vcu_torque_request.front_left",
            ),
            (
                "INVA_SET.AMK_TorqueSetpoint",
                "vcu_torque_request.front_right",
            ),
            (
                "INVC_SET.AMK_TorqueSetpoint",
                "vcu_torque_request.rear_left",
            ),
            (
                "INVD_SET.AMK_TorqueSetpoint",
                "vcu_torque_request.rear_right",
            ),
        ] {
            if let Some(src_acc) = mapped.get(src_id).cloned() {
                mapped.insert(dst_id.to_owned(), src_acc);
            }
        }
    }

    // On 2025-09-20 (2WD rear-only car), synthesize 0.0% front torque requests and INVC/D actual torques
    // so 4-corner powertrain.total_torque_request and powertrain.total_actual_torque formulas evaluate.
    for (ref_id, zero_id) in [
        (
            "vcu_torque_request.rear_left",
            "vcu_torque_request.front_left",
        ),
        (
            "vcu_torque_request.rear_right",
            "vcu_torque_request.front_right",
        ),
        ("INVA_SET.AMK_TorqueSetpoint", "INVC_SET.AMK_TorqueSetpoint"),
        ("INVB_SET.AMK_TorqueSetpoint", "INVD_SET.AMK_TorqueSetpoint"),
        ("INVA_CRIT.AMK_ActualTorque", "INVC_CRIT.AMK_ActualTorque"),
        ("INVB_CRIT.AMK_ActualTorque", "INVD_CRIT.AMK_ActualTorque"),
        ("pedals.throttle", "pedals.regen"),
    ] {
        if !mapped.contains_key(zero_id)
            && let Some(ref_acc) = mapped.get(ref_id)
        {
            let zero_samples = ref_acc
                .samples
                .iter()
                .map(|s| RawSample {
                    utc_ms: s.utc_ms,
                    ticks_ms: s.ticks_ms,
                    physical: 0.0,
                    raw_int: 0,
                    enum_label: None,
                })
                .collect();
            let mut acc = ref_acc.clone();
            acc.samples = zero_samples;
            mapped.insert(zero_id.to_owned(), acc);
        }
    }

    // Synthesize gps_speed_heading.gps_speed from average wheel_speeds (rpm / 557.2 -> m/s) when GPS was absent (April 2026)
    if !mapped.contains_key("gps_speed_heading.gps_speed")
        && let Some(ws_fl) = mapped.get("wheel_speeds.front_left")
    {
        let ws_fr = mapped.get("wheel_speeds.front_right").unwrap_or(ws_fl);
        let ws_rl = mapped.get("wheel_speeds.rear_left").unwrap_or(ws_fl);
        let ws_rr = mapped.get("wheel_speeds.rear_right").unwrap_or(ws_fl);
        let n = ws_fl
            .samples
            .len()
            .min(ws_fr.samples.len())
            .min(ws_rl.samples.len())
            .min(ws_rr.samples.len());
        if n > 0 {
            let mut spd_samples = Vec::with_capacity(n);
            for i in 0..n {
                let avg_rpm = (ws_fl.samples[i].physical.max(0.0)
                    + ws_fr.samples[i].physical.max(0.0)
                    + ws_rl.samples[i].physical.max(0.0)
                    + ws_rr.samples[i].physical.max(0.0))
                    * 0.25;
                let mps = avg_rpm / 557.2;
                spd_samples.push(RawSample {
                    utc_ms: ws_fl.samples[i].utc_ms,
                    ticks_ms: ws_fl.samples[i].ticks_ms,
                    physical: mps,
                    raw_int: mps.round() as i64,
                    enum_label: None,
                });
            }
            let mut spd_acc = ws_fl.clone();
            spd_acc.samples = spd_samples;
            mapped.insert("gps_speed_heading.gps_speed".to_owned(), spd_acc);
        }
    }

    // Synthesize state-lane & fault-lane signals when absent on historical eras
    // 1. main_hb.car_state (on 2025-09-20 from INVA_INFO.AMK_Status_bInverterOn / main_hb_amk.precharge_state)
    if !mapped.contains_key("main_hb.car_state") {
        if let Some(inv_on) = mapped.get("INVA_INFO.AMK_Status_bInverterOn") {
            let mut car_state = inv_on.clone();
            for s in &mut car_state.samples {
                let state = if s.physical > 0.5 { 5.0 } else { 1.0 };
                s.physical = state;
                s.raw_int = state as i64;
            }
            mapped.insert("main_hb.car_state".to_owned(), car_state);
        } else if let Some(pchg) = mapped.get("main_hb_amk.precharge_state") {
            let mut car_state = pchg.clone();
            for s in &mut car_state.samples {
                let state = if s.physical > 0.5 { 5.0 } else { 1.0 };
                s.physical = state;
                s.raw_int = state as i64;
            }
            mapped.insert("main_hb.car_state".to_owned(), car_state);
        }
    }

    // 2. main_hb_amk.precharge_state (on 2026 eras from main_hb.car_state >= 3)
    if !mapped.contains_key("main_hb_amk.precharge_state")
        && let Some(car_state) = mapped.get("main_hb.car_state")
    {
        let mut pchg = car_state.clone();
        for s in &mut pchg.samples {
            let on = if s.physical >= 3.0 { 1.0 } else { 0.0 };
            s.physical = on;
            s.raw_int = on as i64;
        }
        mapped.insert("main_hb_amk.precharge_state".to_owned(), pchg);
    }

    // 3. inva_diagnostics.amk_state (from INVA_INFO.AMK_Status_bInverterOn)
    if !mapped.contains_key("inva_diagnostics.amk_state")
        && let Some(inv_on) = mapped.get("INVA_INFO.AMK_Status_bInverterOn")
    {
        let mut amk_state = inv_on.clone();
        for s in &mut amk_state.samples {
            let st = if s.physical > 0.5 { 2.0 } else { 0.0 };
            s.physical = st;
            s.raw_int = st as i64;
        }
        mapped.insert("inva_diagnostics.amk_state".to_owned(), amk_state);
    }

    // 4. vcu_driver_request.vcu_mode
    if !mapped.contains_key("vcu_driver_request.vcu_mode")
        && let Some(car_state) = mapped.get("main_hb.car_state")
    {
        let mode_val = if track_day_id == "2026-09-05" {
            3.0 // ENDURANCE
        } else {
            4.0 // TUNING
        };
        let mut mode_acc = Vec::new();
        if let Some(first) = car_state.samples.first() {
            mode_acc.push(RawSample {
                utc_ms: first.utc_ms,
                ticks_ms: first.ticks_ms,
                physical: mode_val,
                raw_int: mode_val as i64,
                enum_label: None,
            });
        }
        if let Some(last) = car_state.samples.last()
            && mode_acc.first().map(|s| s.utc_ms) != Some(last.utc_ms)
        {
            mode_acc.push(RawSample {
                utc_ms: last.utc_ms,
                ticks_ms: last.ticks_ms,
                physical: mode_val,
                raw_int: mode_val as i64,
                enum_label: None,
            });
        }
        if !mode_acc.is_empty() {
            let mut acc = car_state.clone();
            acc.samples = mode_acc;
            mapped.insert("vcu_driver_request.vcu_mode".to_owned(), acc);
        }
    }

    // 5. a_box_fault_sync.{IMD, PACK_OVERTEMP, CELL_UNDERVOLTAGE, CELL_OVERVOLTAGE}
    for (src_id, dst_id) in [
        ("main_module_fault_sync.SDC1_IMD", "a_box_fault_sync.IMD"),
        (
            "main_module_fault_sync.SDC2_BMS",
            "a_box_fault_sync.PACK_OVERTEMP",
        ),
        (
            "main_module_fault_sync.SDC2_BMS",
            "a_box_fault_sync.CELL_UNDERVOLTAGE",
        ),
        (
            "main_module_fault_sync.SDC2_BMS",
            "a_box_fault_sync.CELL_OVERVOLTAGE",
        ),
        (
            "a_box_fault_sync.CELL_UNDERVOLTAGE",
            "a_box_fault_sync.CELL_OVERVOLTAGE",
        ),
    ] {
        if !mapped.contains_key(dst_id)
            && let Some(src_acc) = mapped.get(src_id).cloned()
        {
            mapped.insert(dst_id.to_owned(), src_acc);
        }
    }

    // Sanitize pre-fix GPS / BMS / AMK bit-flip & power-off sentinel outliers across all columns
    if let Some(acc) = mapped.get_mut("gps_velocity.gps_vel_e") {
        sanitize_column_bounds(acc, -50.0, 50.0, 0.0);
    }
    if let Some(acc) = mapped.get_mut("gps_velocity.gps_vel_n") {
        sanitize_column_bounds(acc, -50.0, 50.0, 0.0);
    }
    if let Some(acc) = mapped.get_mut("gps_coordinates.latitude") {
        sanitize_column_bounds(acc, 40.40, 40.50, 40.4622);
    }
    if let Some(acc) = mapped.get_mut("gps_coordinates.longitude") {
        sanitize_column_bounds(acc, -87.00, -86.85, -86.9570);
    }
    if let Some(acc) = mapped.get_mut("gps_time.day") {
        sanitize_column_bounds(acc, 1.0, 31.0, 20.0);
    }
    if let Some(acc) = mapped.get_mut("gps_time.month") {
        sanitize_column_bounds(acc, 1.0, 12.0, 9.0);
    }
    if let Some(acc) = mapped.get_mut("gps_time.year") {
        sanitize_column_bounds(acc, 24.0, 27.0, 26.0);
    }
    if let Some(acc) = mapped.get_mut("gps_time.hour") {
        sanitize_column_bounds(acc, 0.0, 23.0, 18.0);
    }
    if let Some(acc) = mapped.get_mut("gps_time.minute") {
        sanitize_column_bounds(acc, 0.0, 59.0, 0.0);
    }
    if let Some(acc) = mapped.get_mut("gps_time.second") {
        sanitize_column_bounds(acc, 0.0, 59.0, 0.0);
    }

    // Compute gps_speed_heading.gps_heading from (gps_vel_e, gps_vel_n) when all-zero in 2026
    let heading_all_zero = mapped
        .get("gps_speed_heading.gps_heading")
        .is_none_or(|a| a.samples.iter().all(|s| s.physical.abs() < 0.01));
    if heading_all_zero
        && let (Some(ve), Some(vn)) = (
            mapped.get("gps_velocity.gps_vel_e"),
            mapped.get("gps_velocity.gps_vel_n"),
        )
    {
        let n = ve.samples.len().min(vn.samples.len());
        if n > 0 {
            let mut hdg_samples = Vec::with_capacity(n);
            let mut last_hdg = 0.0_f64;
            for i in 0..n {
                let e = ve.samples[i].physical;
                let north = vn.samples[i].physical;
                if e.hypot(north) > 0.5 {
                    last_hdg = e.atan2(north).to_degrees().rem_euclid(360.0);
                }
                hdg_samples.push(RawSample {
                    utc_ms: ve.samples[i].utc_ms,
                    ticks_ms: ve.samples[i].ticks_ms,
                    physical: last_hdg,
                    raw_int: last_hdg.round() as i64,
                    enum_label: None,
                });
            }
            let mut hdg_acc = ve.clone();
            hdg_acc.samples = hdg_samples;
            mapped.insert("gps_speed_heading.gps_heading".to_owned(), hdg_acc);
        }
    }

    if let Some(acc) = mapped.get_mut("pack_stats.max_temp") {
        sanitize_column_bounds(acc, 10.0, 85.0, 28.0);
    }
    let vbatt_fallback = mapped
        .get("pack_stats.vbatt_adc")
        .and_then(|a| median_physical(&a.samples))
        .filter(|&v| v > 200.0)
        .unwrap_or(480.0);
    if let Some(acc) = mapped.get_mut("pack_stats.pack_voltage") {
        sanitize_column_bounds(acc, 200.0, 600.0, vbatt_fallback);
    }
    if let Some(acc) = mapped.get_mut("pack_stats.vbatt_adc") {
        sanitize_column_bounds(acc, 200.0, 600.0, vbatt_fallback);
    }
    for t_sig in [
        "INVA_TEMPS.AMK_MotorTemp",
        "INVB_TEMPS.AMK_MotorTemp",
        "INVC_TEMPS.AMK_MotorTemp",
        "INVD_TEMPS.AMK_MotorTemp",
        "INVA_TEMPS.AMK_IGBTTemp",
        "INVB_TEMPS.AMK_IGBTTemp",
        "INVC_TEMPS.AMK_IGBTTemp",
        "INVD_TEMPS.AMK_IGBTTemp",
        "INVA_TEMPS.AMK_InverterTemp",
        "INVB_TEMPS.AMK_InverterTemp",
        "INVC_TEMPS.AMK_InverterTemp",
        "INVD_TEMPS.AMK_InverterTemp",
    ] {
        if let Some(acc) = mapped.get_mut(t_sig) {
            sanitize_column_bounds(acc, 5.0, 150.0, 25.0);
        }
    }
    for tq_sig in [
        "INVA_CRIT.AMK_ActualTorque",
        "INVB_CRIT.AMK_ActualTorque",
        "INVC_CRIT.AMK_ActualTorque",
        "INVD_CRIT.AMK_ActualTorque",
    ] {
        if let Some(acc) = mapped.get_mut(tq_sig) {
            sanitize_column_bounds(acc, -110.0, 250.0, 0.0);
        }
    }

    // Apply canonical SuperDBC metadata (units, descriptions, bus_name, can_id, choices) to every column
    for (sig_id, acc) in &mut mapped {
        if let Some(meta) = canonical_by_id.get(sig_id) {
            acc.unit = meta.unit.clone();
            acc.is_discrete = meta.is_discrete;
            acc.is_fault_flag = meta.is_fault_flag;
            acc.fault_severity = meta.fault_severity;
            acc.bus_name = meta.bus_name.clone();
            acc.bus_id = meta.bus_id;
            acc.can_id = meta.can_id;
            acc.transmitter = meta.transmitter.clone();
            acc.nominal_period_ms = meta.nominal_period_ms;
            acc.description = meta.description.clone();
            if acc.is_discrete {
                for sample in &mut acc.samples {
                    sample.raw_int = sample
                        .physical
                        .round()
                        .clamp(i64::MIN as f64, i64::MAX as f64)
                        as i64;
                    sample.enum_label =
                        meta.int_to_label.get(&sample.raw_int).cloned().or_else(|| {
                            Some(match sample.raw_int {
                                0 => "OFF".to_owned(),
                                1 => "ON".to_owned(),
                                n => format!("RAW_{n}"),
                            })
                        });
                }
            }
        }
    }

    *columns = mapped;
}

/// Normalize all existing partitions in `<store_dir>/partitions/*` in place to match `canonical_dbc_path`,
/// and synchronize `<store_dir>/local_registry.sqlite` and `<store_dir>/cloud_registry.sqlite`.
pub fn normalize_per26_store(
    store_dir: &Path,
    canonical_dbc_path: &Path,
) -> Result<usize, ImportError> {
    let canonical_dbc = Database::load(canonical_dbc_path)
        .map_err(|e| ImportError::Io(io::Error::other(e.to_string())))?;
    let _ = storage::archive_superdbc_to_store(store_dir, &canonical_dbc)?;
    let superdbc_hash = canonical_dbc.content_hash().to_owned();

    let partitions_dir = store_dir.join("partitions");
    if !partitions_dir.is_dir() {
        return Ok(0);
    }

    let mut part_dirs: Vec<PathBuf> = fs::read_dir(&partitions_dir)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir() && p.join("manifest.json").is_file() && p.join("full_res.parquet").is_file()
        })
        .collect();
    part_dirs.sort();

    let registry = Registry::open_store(store_dir)?;
    let _ = registry.clear_auto_fault_annotations()?;

    let mut rewritten = 0usize;
    for part_dir in part_dirs {
        let mf_path = part_dir.join("manifest.json");
        let mf_bytes = fs::read(&mf_path)?;
        let mut manifest: storage::PartitionManifest = serde_json::from_slice(&mf_bytes)
            .map_err(|e| ImportError::Io(io::Error::other(e.to_string())))?;

        let mut columns = storage::read_partition_column_accumulators(&part_dir, &manifest)?;
        normalize_per26_columns_to_canonical(&mut columns, &manifest.track_day_id, &canonical_dbc);
        storage::rewrite_partition_in_place(&part_dir, &mut manifest, &superdbc_hash, &columns)?;
        registry.register_manifest(&manifest, true)?;
        rewritten += 1;
    }

    let _ = registry.checkpoint_wal();
    let local_db = store_dir.join(storage::LOCAL_REGISTRY_FILENAME);
    let cloud_db = store_dir.join("cloud_registry.sqlite");
    if local_db.is_file() {
        let _ = fs::remove_file(store_dir.join("cloud_registry.sqlite-wal"));
        let _ = fs::remove_file(store_dir.join("cloud_registry.sqlite-shm"));
        let _ = fs::copy(&local_db, &cloud_db);
    }

    Ok(rewritten)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_col(unit: &str, vals: &[(i64, f64)]) -> SignalColumnAccumulator {
        SignalColumnAccumulator {
            unit: unit.to_owned(),
            is_discrete: false,
            is_fault_flag: false,
            fault_severity: None,
            bus_name: "VCAN".to_owned(),
            bus_id: 0,
            can_id: 0,
            transmitter: "TEST".to_owned(),
            nominal_period_ms: Some(10),
            description: String::new(),
            samples: vals
                .iter()
                .map(|&(t, v)| RawSample {
                    utc_ms: t,
                    ticks_ms: t as u32,
                    physical: v,
                    raw_int: v.round() as i64,
                    enum_label: None,
                })
                .collect(),
        }
    }

    #[test]
    fn test_per26_imu_rotation_centering_and_idempotency() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let dbc_path = manifest_dir.join("../../superdbc/superdbc_per26.json");
        if !dbc_path.is_file() {
            return;
        }
        let db = Database::load(&dbc_path).expect("load canonical superdbc");

        // Simulate 2025-09-20 tilted BMI088 sensor frame:
        // Stationary gravity: (-0.3156, -7.7494, -5.9902) m/s^2
        // Plus a left turn sample with lateral accel on sensor X (-10.67 m/s^2)
        let mut cols = BTreeMap::new();
        cols.insert(
            "imu_accel.imu_accel_x".to_owned(),
            make_col(
                "m/s^2",
                &[
                    (100, -0.3156),
                    (200, -0.3156),
                    (300, -0.3156),
                    (400, -10.6730),
                ],
            ),
        );
        cols.insert(
            "imu_accel.imu_accel_y".to_owned(),
            make_col(
                "m/s^2",
                &[
                    (100, -7.7494),
                    (200, -7.7494),
                    (300, -7.7494),
                    (400, -7.7494),
                ],
            ),
        );
        cols.insert(
            "imu_accel.imu_accel_z".to_owned(),
            make_col(
                "m/s^2",
                &[
                    (100, -5.9902),
                    (200, -5.9902),
                    (300, -5.9902),
                    (400, -5.9902),
                ],
            ),
        );
        cols.insert(
            "VCU_torques_speeds.TO_VT_left".to_owned(),
            make_col("Nm", &[(100, 0.0), (200, 21.0)]),
        );
        cols.insert(
            "sdc_status.IMD".to_owned(),
            make_col("", &[(100, 1.0), (200, 1.0)]),
        );

        normalize_per26_columns_to_canonical(&mut cols, "2025-09-20", &db);

        let ax = &cols["IMU_acceleration.X_axis"].samples;
        let ay = &cols["IMU_acceleration.Y_axis"].samples;
        let az = &cols["IMU_acceleration.Z_axis"].samples;

        // Stationary samples (t=100..300) must be centered at (0, 0, -1 g)
        assert!(ax[0].physical.abs() < 1e-3, "ax[0]={}", ax[0].physical);
        assert!(ay[0].physical.abs() < 1e-3, "ay[0]={}", ay[0].physical);
        assert!(
            (az[0].physical + 1.0).abs() < 1e-3,
            "az[0]={}",
            az[0].physical
        );
        // Left-turn sample (t=400) must appear on lateral Y_axis (~ -1.05 g), not X_axis!
        assert!(ax[3].physical.abs() < 0.05, "ax[3]={}", ax[3].physical);
        assert!(
            (ay[3].physical - (-1.056)).abs() < 0.05,
            "ay[3]={}",
            ay[3].physical
        );

        // Torque scaling: 21 Nm -> 214.2857 % Mn
        let tq_rl = &cols["vcu_torque_request.rear_left"].samples;
        assert!((tq_rl[1].physical - 214.2857).abs() < 0.01);

        // SDC polarity: sdc_status.IMD == 1 (healthy) -> SDC1_IMD == 0 (no fault)
        let sdc = &cols["main_module_fault_sync.SDC1_IMD"].samples;
        assert_eq!(sdc[0].raw_int, 0);

        // Re-running normalize_per26_columns_to_canonical must be 100% idempotent
        let snapshot_ay: Vec<f64> = ay.iter().map(|s| s.physical).collect();
        let snapshot_tq: Vec<f64> = tq_rl.iter().map(|s| s.physical).collect();
        normalize_per26_columns_to_canonical(&mut cols, "2025-09-20", &db);
        let ay2: Vec<f64> = cols["IMU_acceleration.Y_axis"]
            .samples
            .iter()
            .map(|s| s.physical)
            .collect();
        let tq2: Vec<f64> = cols["vcu_torque_request.rear_left"]
            .samples
            .iter()
            .map(|s| s.physical)
            .collect();
        for (a, b) in snapshot_ay.iter().zip(ay2.iter()) {
            assert!((a - b).abs() < 1e-6, "non-idempotent ay: {a} vs {b}");
        }
        for (a, b) in snapshot_tq.iter().zip(tq2.iter()) {
            assert!((a - b).abs() < 1e-6, "non-idempotent tq: {a} vs {b}");
        }
    }

    #[test]
    fn verify_cloud_store_imu_centered_across_all_days() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let parts_dir = manifest_dir.join("../cloud_store/partitions");
        if !parts_dir.is_dir() {
            return;
        }
        let mut entries: Vec<PathBuf> = fs::read_dir(&parts_dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .collect();
        entries.sort();

        let mut seen_days = std::collections::HashSet::new();
        let mut checked = 0usize;
        for p in entries {
            let mf_path = p.join("manifest.json");
            if !mf_path.is_file() {
                continue;
            }
            let mf: storage::PartitionManifest =
                serde_json::from_slice(&fs::read(&mf_path).unwrap()).unwrap();
            if !mf
                .signals
                .iter()
                .any(|s| s.signal_id == "IMU_acceleration.Z_axis")
            {
                continue;
            }
            if !seen_days.insert(mf.track_day_id.clone()) {
                continue;
            }
            let cols = storage::read_partition_column_accumulators(&p, &mf).unwrap();
            if let (Some(ax), Some(ay), Some(az)) = (
                cols.get("IMU_acceleration.X_axis"),
                cols.get("IMU_acceleration.Y_axis"),
                cols.get("IMU_acceleration.Z_axis"),
            ) {
                let mx = median_physical(&ax.samples).unwrap();
                let my = median_physical(&ay.samples).unwrap();
                let mz = median_physical(&az.samples).unwrap();
                assert!(
                    mx.abs() < 0.05 && my.abs() < 0.05 && (mz + 1.0).abs() < 0.05,
                    "Uncentered IMU in {} ({}): med=({mx:.4}, {my:.4}, {mz:.4})",
                    mf.track_day_id,
                    mf.partition_id
                );
                checked += 1;
            }
        }
        assert!(checked >= 8, "expected >=8 IMU track days, got {checked}");
    }
}
