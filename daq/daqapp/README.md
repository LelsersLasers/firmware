# PER DaqApp

DaqApp is PER's complete trackside data acquisition and analysis desktop application/tooling.

## What it does

- Connects to supported CAN sources and decodes traffic with a selected SuperDBC artifact and CAN bus selection
- Displays live and historical signals in configurable tables, lists, multi-unit scopes, state lanes, XY scatter plots, and GPS maps
- Built-in Subteam Workspace Presets (`daq/daqviewer/default_presets/*.daqview.json`) for Vehicle Dynamics, Powertrain, Thermals, Aerodynamics, Sensors & Controls, and Trackside Live Debug
- Portable `.daqview.json` workspace layouts and compressed `daqview1:...` Slack/Discord share strings with optional time-range and custom formula restoration
- 3-click Dataset Export Modal (`Parquet`, `CSV`, and MATLAB v5 `.mat`) powered by `daq/config/export_presets.json`
- Logs all received CAN traffic and imports SD card logs into canonical Arrow Parquet partitions with multi-resolution pyramid tiles
- Full support for CAN-message sending
- Hardware-in-the-loop configuration, test running, and results viewing
- Full PER Bootloader workflow

## Screenshots

<table>
  <tr>
    <td><img src="./showcase/table.png" width="500"></td>
    <td><img src="./showcase/battery_charging_workflow.png" width="500"></td>
  </tr>
  <tr>
    <td><img src="./showcase/hil.png" width="500"></td>
    <td><img src="./showcase/scope.png" width="500"></td>
  </tr>
</table>

## Using the app

1. Use sidebar to select a CAN source from the connection controls (SLCAN, CANnector USB, UDP, or dev modes)
2. Use the sidebar to select a `superdbc_*.json` artifact and its CAN bus or multi-bus preset. The file picker opens in `monorepo/superdbc`, which is an output from the firmware build process
3. Use the sidebar (or ctrl-P) to launch widgets
4. Use **Bootloader** to validate and upload a package. See the
   [bootloader update guide](../../docs/daqapp/bootloader_updates.md).

Sidebar settings (source, SuperDBC, etc) are saved to a local settings file to persist between sessions.

## Scope graphs

- New scopes start as blank plots. Click a plot title to rename it; names are saved with the workspace.
- The single **ƒ** channels icon in the scope toolbar opens a searchable selector with **Left**, **Right**, and **Discrete** destinations. It stays open for repeated selection, and lists assigned signals with reassignment/removal controls.
- Drag channels onto the labeled **Left axis**, **Right axis**, or bottom **Discrete** drop zones. Discrete channels render labeled state strips aligned with the graph's time axis. State strips show no-data gaps and support shared cursor hover/click; they are unavailable in comparison mode.
- Click the plot to move the shared playhead. Left drag selects a range and shows its statistics. Escape clears the selection.
- Right drag selects a time range to zoom. Scroll zooms time, Shift scroll zooms Y, and double click resets the local Y zoom.
- Each plot has a single-row legend next to its title. **+N more** opens a searchable channel list with cursor values. Hover to highlight a trace, click its eye to toggle visibility, or double-click its name to solo it. The legend icon switches to a floating overlay.
- Y-axis tick labels show the engineering unit above the value. Hover a legend entry to see its full axis label and side. With three or more scales on a plot, right-click a legend entry and choose **Move axis to linked plot** to give its scale a separate lane on the same timeline.
- Right-click a legend entry, then choose **Appearance**, **Assignment**, or **Axis** to open its settings. Configure color, opacity, width, lines, points, stepped rendering, plot assignment, and independent Auto or Fixed axes.
- The stacked plots icon splits signals into aligned lanes. The **+** button adds another lane without a fixed count limit; tall stacks scroll vertically. Drag a legend entry or a discrete strip into another plot in this or another scope to move that signal. The Left, Right, or Discrete drop zone sets its new assignment. Colour, appearance, and custom axis ranges travel with the signal; cancelled drags leave the source unchanged. The scope menu also contains plot count and layout controls.
- Each plot's **×** turns red on hover. Click it to reveal a green **✓**, then click again to delete the plot and its signals from this widget, not the underlying data. Clicking elsewhere or pressing Escape cancels confirmation. Deleting the last plot leaves a blank plot.
- The measurement icon enables A and B cursors. Click twice to pin them, then drag either cursor line. The plots show the time difference and legends show signal differences. Missing or offscreen measurements are not interpolated across gaps.
- Appearance, axes, and lane assignments are stored in workspace presets. Temporary visibility and measurement cursors are session state.

## Getting started

Install Rust/Cargo and the platform prerequisites described in the repository
[setup guide](../../docs/setup.md). Linux users also likely need `libudev-dev` and
`pkg-config` to discover serial devices.

### To Build

From the repository root:

```bash
cargo build --manifest-path daq/daqapp/Cargo.toml --locked
```

From the `daq/daqapp/` directory, you can also build with Cargo directly:

```bash
cargo build --locked
```

### To Run

Starting from the repository root, run the app with:

```bash
cd daq/daqapp && cargo run
```

Running is supported only with `daq/daqapp/` as the working directory so the
app can find its themes, formatter configuration, and HIL presets. Settings and
logs are also relative to this directory.

## Development

Useful commands from `daq/daqapp/`:

```bash
cargo fmt
cargo build --locked
cargo run
```

The app's source is organized by responsibility across the DAQ workspace:

- `src/ui/` - desktop-specific panels (`Bootloader`, `LogParser`, `SendUi`, `Hil`).
- `../daqviewer/src/` - shared analysis UI (`Scope`, `StateLane`, `XYScatter`, `GpsPlot`, `Timeline`, `SignalBrowser`, `WorkspaceView`, and `themes/`).
- `../daqnative/src/can_thread/` - CAN worker and operational commands/events.
- `../daqnative/src/storage/` - SQLite `Registry`, Arrow/Parquet partition I/O, and background `daqcloud` sync.
- `../daqnative/src/hil/` and `hil_config/` - HIL support and presets.
- `../daqcore/src/` - target-independent SuperDBC decoding/encoding, formula engine, virtual timeline router, log parsing, and dataset export.

### Thread overview

![daqapp_overview](../../docs/daqapp/daqapp_overview.drawio.png)
![daqapp_threads](../../docs/daqapp/daqapp_threads.drawio.png)

### Testing

For easy testing, you can use the loopback or simulated CAN sources. The loopback source is a virtual CAN bus that echoes messages sent to it, while the simulated source generates random messages for testing purposes.
