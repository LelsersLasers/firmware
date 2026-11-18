# PER DaqCLI

`daqcli` is the headless command-line companion to `daqapp` for offline SD card `.log` ingestion and synchronized dataset export (`Parquet`, `CSV`, and MATLAB Level 5 `.mat`).

## Subcommands

### 1. `daqcli import` — Ingest Raw SD `.log` Files into Parquet Partitions

Scans a directory of raw SD card `.log` binaries, performs 3-tier clock correlation (GPS regression -> RTC filename -> file mtime), generates multi-resolution Apache Parquet partition pyramids (`full/`, `10hz/`, `1hz/`, `states.parquet`), and registers auto-detected driving stints and fault intervals in `registry.sqlite`:

```bash
cargo run --manifest-path daq/Cargo.toml --release -p daqcli -- import \
    --logs-dir <dir with .log files> \
    --store-dir <parquet store directory> \
    --superdbc <path to superdbc_*.json>
```

### 2. `daqcli export` — Export Synchronized Datasets by Preset or Event

Exports a time window or named event/stint annotation (`--event "Driving Stint 1"`) from a Parquet partition store using a subteam preset (`vd`, `aero`, `powertrain_thermals`, `controls`, `raw_all`) or custom signal globs (`--signals`), evaluating derived `FormulaEngine` channels and aligning multi-rate signals automatically:

```bash
cargo run --manifest-path daq/Cargo.toml --release -p daqcli -- export \
    --store-dir ./store \
    --event "Driving Stint 1" \
    --preset vd \
    --format parquet \
    -o stint1_vd.parquet
```

#### `daqcli export` Options

- `--store-dir <PATH>` (required) — Directory containing the Parquet partition store (`registry.sqlite` and `partitions/`).
- `--event <NAME_OR_ID>` — Named event/stint annotation (e.g. `"Driving Stint 1"`). If omitted, uses `--start-ms` / `--end-ms` or the full track-day span.
- `--start-ms <UTC_MS>` / `--end-ms <UTC_MS>` — Optional explicit UTC millisecond bounds.
- `--preset <ID>` — Subteam signal preset from `daq/config/export_presets.json` (`vd`, `aero`, `powertrain_thermals`, `controls`, `raw_all`; defaults to `vd` when `--signals` is omitted).
- `--signals <GLOBS>` — Optional comma-separated signal IDs or glob patterns (e.g. `"IMU_acceleration.*,battery.power_kw"`).
- `--format <parquet|csv|mat>` — Output file format (inferred from `-o` extension if omitted; defaults to `parquet`).
- `--rate-hz <HZ>` — Override the uniform resampling rate in Hz (e.g. `100.0`).
- `--sparse` — Export at exact union timestamps without uniform grid resampling.
- `-o, --output <PATH>` (required) — Destination file path.

### 3. `daqcli sync` — Synchronize a Local Store with `daqcloud`

```bash
cargo run --manifest-path daq/Cargo.toml --release -p daqcli -- sync \
    --store-dir ./store \
    --cloud-url http://127.0.0.1:3000
```

Set `RUST_LOG` to control log verbosity (for example `RUST_LOG=debug`). Defaults to `info`.
