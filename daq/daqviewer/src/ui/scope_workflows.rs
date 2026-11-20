//! Interaction regressions with real decoded telemetry rather than empty graphs.
use super::*;
use eframe::egui::{self, Event, Modifiers, PointerButton, Pos2};

const BASE: i64 = 1_700_000_000_000;
const SPEED: &str = "gps_speed_heading.gps_speed";
const RPM: &str = "wheel_speeds.front_left";
const THROTTLE: &str = "pedals.throttle";

const TEST_DB_JSON: &str = r#"{
  "content_hash": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
  "versions": { "schema_version": "1.1", "hash": "test_scope_workflows" },
  "buses": {
    "VCAN": {
      "bus_id": 0,
      "baud_rate": 500000,
      "nodes": [],
      "messages": [
        {
          "id": 100,
          "is_extended_id": false,
          "message_name": "gps_speed_heading",
          "transmitter": "MAIN_MODULE",
          "receivers": [],
          "length_bytes": 4,
          "nominal_period_ms": 100,
          "priority": 1,
          "description": "",
          "signals": [
            {
              "signal_name": "gps_speed",
              "description": "",
              "data_type": "float32",
              "raw_type": "float32",
              "start_bit": 0,
              "bit_length": 32,
              "byte_order": "little_endian",
              "scale": 1.0,
              "offset": 0.0,
              "limits": null,
              "unit": "m/s",
              "choices": null,
              "display_format": null
            }
          ]
        },
        {
          "id": 101,
          "is_extended_id": false,
          "message_name": "wheel_speeds",
          "transmitter": "MAIN_MODULE",
          "receivers": [],
          "length_bytes": 4,
          "nominal_period_ms": 100,
          "priority": 1,
          "description": "",
          "signals": [
            {
              "signal_name": "front_left",
              "description": "",
              "data_type": "float32",
              "raw_type": "float32",
              "start_bit": 0,
              "bit_length": 32,
              "byte_order": "little_endian",
              "scale": 1.0,
              "offset": 0.0,
              "limits": null,
              "unit": "rpm",
              "choices": null,
              "display_format": null
            }
          ]
        },
        {
          "id": 102,
          "is_extended_id": false,
          "message_name": "pedals",
          "transmitter": "MAIN_MODULE",
          "receivers": [],
          "length_bytes": 4,
          "nominal_period_ms": 100,
          "priority": 1,
          "description": "",
          "signals": [
            {
              "signal_name": "throttle",
              "description": "",
              "data_type": "float32",
              "raw_type": "float32",
              "start_bit": 0,
              "bit_length": 32,
              "byte_order": "little_endian",
              "scale": 1.0,
              "offset": 0.0,
              "limits": null,
              "unit": "",
              "choices": null,
              "display_format": null
            }
          ]
        }
      ]
    }
  }
}"#;

#[derive(Default)]
struct Requests {
    range: Option<(i64, i64)>,
    cursor: Option<i64>,
    pan: Option<i64>,
    pause: bool,
}
struct Harness {
    ctx: egui::Context,
    scope: Scope,
    router: daqcore::router::VirtualTimelineRouter,
    output: Option<egui::FullOutput>,
    size: egui::Vec2,
}
impl Harness {
    fn new() -> Self {
        let ctx = egui::Context::default();
        ui::theme::store_theme(&ctx, ui::theme::ThemeSelection::default().get_colors());
        let db = daqcore::superdbc::database::Database::from_json(TEST_DB_JSON).unwrap();
        let mut router = daqcore::router::VirtualTimelineRouter::live(
            daqcore::time::Time::from_unix_millis(BASE + 10_000),
            10.0,
        )
        .unwrap();
        for index in 0..=100 {
            let seconds = index as f64 * 0.1;
            for (identity, name, value) in [
                (100u32, "gps_speed", seconds * 2.0),
                (101u32, "front_left", 1000.0 + seconds * 100.0),
                (102u32, "throttle", seconds * 0.1),
            ] {
                let payload = db.encode(0, identity, &[(name, value)]).unwrap();
                let decoded = db.decode(0, identity, &payload).ok();
                router.ingest_frame(daqcore::frame::ParsedFrame {
                    timestamp: daqcore::time::Time::from_unix_millis(BASE + index * 100),
                    bus_id: 0,
                    kind: daqcore::frame::FrameKind::Data,
                    dlc: 4,
                    identity,
                    raw_bytes: payload,
                    decoded,
                });
            }
        }
        router.timeline_mut().snap_to_range_ms(BASE, BASE + 10_000);
        router
            .timeline_mut()
            .set_setpoint(daqcore::time::Time::from_unix_millis(BASE + 5_000));
        let scope = Scope::new_multi(
            1,
            "Workflow",
            vec![
                (SPEED.to_owned(), "Speed".to_owned(), "m/s".to_owned()),
                (RPM.to_owned(), "RPM".to_owned(), "rpm".to_owned()),
                (THROTTLE.to_owned(), "Throttle".to_owned(), String::new()),
            ],
        );
        let mut harness = Self {
            ctx,
            scope,
            router,
            output: None,
            size: egui::vec2(1000.0, 650.0),
        };
        for _ in 0..3 {
            harness.frame(vec![]);
        }
        harness
    }
    fn frame(&mut self, events: Vec<Event>) -> Requests {
        let view = workspace_view::WorkspaceDataView::new(&self.router);
        let output = self.ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, self.size)),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let _ = self.scope.show(ui, &view);
                });
            },
        );
        let requests = Requests {
            range: view.requested_range_ms(),
            cursor: view.requested_setpoint_utc_ms(),
            pan: view.requested_pan_delta_ms(),
            pause: view.requested_pause(),
        };
        self.output = Some(output);
        requests
    }
    fn point(&self, lane: usize, fraction: f32) -> Pos2 {
        let rect = self.scope.plot_frames[lane];
        egui::pos2(rect.left() + rect.width() * fraction, rect.center().y)
    }
    fn press(&mut self, point: Pos2, button: PointerButton) -> Requests {
        self.frame(vec![Event::PointerMoved(point)]);
        self.frame(vec![Event::PointerButton {
            pos: point,
            button,
            pressed: true,
            modifiers: Modifiers::NONE,
        }])
    }
    fn release(&mut self, point: Pos2, button: PointerButton) -> Requests {
        self.frame(vec![Event::PointerButton {
            pos: point,
            button,
            pressed: false,
            modifiers: Modifiers::NONE,
        }])
    }
    fn click(&mut self, point: Pos2) -> Requests {
        self.press(point, PointerButton::Primary);
        let request = self.release(point, PointerButton::Primary);
        self.frame(vec![]);
        request
    }
    fn find(&self, label: &str) -> Pos2 {
        self.output
            .as_ref()
            .unwrap()
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == label => {
                    let center = text.pos + text.galley.size() * 0.5;
                    shape.clip_rect.contains(center).then_some(center)
                }
                _ => None,
            })
            .unwrap_or_else(|| {
                let visible: Vec<_> = self
                    .output
                    .as_ref()
                    .unwrap()
                    .shapes
                    .iter()
                    .filter_map(|shape| match &shape.shape {
                        egui::Shape::Text(text)
                            if shape
                                .clip_rect
                                .contains(text.pos + text.galley.size() * 0.5) =>
                        {
                            Some(text.galley.text().to_owned())
                        }
                        _ => None,
                    })
                    .collect();
                panic!("Missing visible label {label} among {visible:?}")
            })
    }
}

#[test]
fn selecting_a_range_zoom_is_shared_and_escape_cancels() {
    let mut h = Harness::new();
    let a = h.point(0, 0.2);
    let b = h.point(0, 0.8);
    // Right-drag (`PointerButton::Secondary`) selects a range to zoom into.
    assert!(h.press(a, PointerButton::Secondary).pause);
    assert_eq!(h.frame(vec![Event::PointerMoved(b)]).pan, None);
    let requests = h.release(b, PointerButton::Secondary);
    let (start, end) = requests
        .range
        .expect("Right drag selects the shared window");
    assert!((start - BASE - 2_000).abs() < 20);
    assert!((end - BASE - 8_000).abs() < 20);
    assert_eq!(requests.cursor, None);
    let cursor = h.router.timeline().setpoint_utc_ms().clamp(start, end);
    h.router.timeline_mut().snap_to_range_ms(start, end);
    h.router
        .timeline_mut()
        .set_setpoint(daqcore::time::Time::from_unix_millis(cursor));
    h.frame(vec![]);
    assert_eq!(h.scope.cached_data.as_ref().unwrap().key.start_ms, start);
    assert_eq!(h.scope.cached_data.as_ref().unwrap().key.end_ms, end);
    let a = h.point(0, 0.2);
    let b = h.point(0, 0.8);
    h.press(a, PointerButton::Secondary);
    h.frame(vec![
        Event::PointerMoved(b),
        Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        },
    ]);
    assert_eq!(h.release(b, PointerButton::Secondary).range, None);
    assert!(h.scope.gesture.is_none());
}

#[test]
fn left_drag_highlights_section_stats_without_zooming_and_click_scrubs() {
    let mut h = Harness::new();
    let a = h.point(0, 0.2);
    let b = h.point(0, 0.8);
    h.press(a, PointerButton::Primary);
    let request = h.frame(vec![Event::PointerMoved(b)]);
    assert_eq!(request.range, None);
    let release_req = h.release(b, PointerButton::Primary);
    assert_eq!(release_req.range, None, "Left drag must not zoom");
    assert_eq!(release_req.cursor, None);
    let (sel_start, sel_end) = h
        .scope
        .measurements
        .selection
        .expect("Left drag stores highlighted section for stats");
    assert!((sel_start - BASE - 2_000).abs() < 20);
    assert!((sel_end - BASE - 8_000).abs() < 20);

    // Render frame and verify the on-graph stats overlay displays min/max/avg/sum.
    h.frame(vec![]);
    assert!(
        h.output
            .as_ref()
            .unwrap()
            .shapes
            .iter()
            .any(|shape| match &shape.shape {
                egui::Shape::Text(text) =>
                    text.galley.text().contains("min ")
                        && text.galley.text().contains("max ")
                        && text.galley.text().contains("avg ")
                        && text.galley.text().contains("sum "),
                _ => false,
            }),
        "Expected section stats overlay with min/max/avg/sum"
    );

    // Simple left-click clears the selection and scrubs the playhead.
    let point = h.point(0, 0.4);
    let request = h.click(point);
    assert!((request.cursor.unwrap() - BASE - 4_000).abs() < 20);
    assert_eq!(request.range, None);
    assert!(h.scope.measurements.selection.is_none());
}

#[test]
fn axes_have_independent_fixed_ranges_and_split_signals() {
    let mut h = Harness::new();
    h.scope
        .config
        .axes
        .entry("0|m/s".to_owned())
        .or_default()
        .range = Some([0.0, 50.0]);
    h.scope
        .config
        .axes
        .entry("0|rpm".to_owned())
        .or_default()
        .range = Some([0.0, 4_000.0]);
    for _ in 0..3 {
        h.frame(vec![]);
    }
    assert_eq!(h.scope.plot_frames.len(), 1);
    let cache = h.scope.cached_data.as_ref().unwrap();
    for (key, expected) in [("0|m/s", 50.0), ("0|rpm", 4_000.0)] {
        let index = cache
            .unit_groups
            .iter()
            .position(|group| group == key)
            .unwrap();
        assert!(
            (cache.unit_transforms[index].plot_to_phys(cache.primary_max) - expected).abs() < 1e-6
        );
    }
    // Explicit independent axes can split two signals even if their units match.
    h.scope
        .config
        .channels
        .entry(SPEED.to_owned())
        .or_default()
        .axis = "Independent".to_owned();
    h.frame(vec![]);
    assert!(
        h.scope
            .cached_data
            .as_ref()
            .unwrap()
            .unit_groups
            .iter()
            .any(|key| key == "0|Independent")
    );
    for size in [egui::vec2(320.0, 240.0), egui::vec2(120.0, 100.0)] {
        h.size = size;
        h.frame(vec![]);
    }
}

#[test]
fn measurements_pin_drag_and_report_signal_deltas_without_scrubbing() {
    let mut h = Harness::new();
    h.click(egui::pos2(50.0, 17.0));
    assert!(h.scope.measurements.enabled);
    let a = h.point(0, 0.3);
    let b = h.point(0, 0.7);
    assert_eq!(h.click(a).cursor, None);
    assert_eq!(h.click(b).cursor, None);
    let [Some(first), Some(second)] = h.scope.measurements.pins else {
        panic!("Two pinned cursors");
    };
    assert!((first - BASE - 3_000).abs() < 20);
    assert!((second - BASE - 7_000).abs() < 20);
    let start = h.point(0, 0.3);
    let end = h.point(0, 0.4);
    h.press(start, PointerButton::Primary);
    h.frame(vec![Event::PointerMoved(end)]);
    let request = h.release(end, PointerButton::Primary);
    assert_eq!(request.range, None);
    assert_eq!(request.cursor, None);
    assert!((h.scope.measurements.pins[0].unwrap() - BASE - 4_000).abs() < 20);
    h.frame(vec![]);
    assert!(
        h.output
            .as_ref()
            .unwrap()
            .shapes
            .iter()
            .any(|shape| match &shape.shape {
                egui::Shape::Text(text) =>
                    text.galley.text().starts_with("Δ (") && text.galley.text().contains("m/s"),
                _ => false,
            })
    );
}

#[test]
fn browser_drop_assigns_signal_to_plot() {
    let mut h = Harness::new();
    let target = h.scope.plot_frames[0].center();
    h.press(egui::pos2(780.0, 17.0), PointerButton::Primary);
    egui::DragAndDrop::set_payload(
        &h.ctx,
        ui::signal_browser::DraggedSignal::Derived {
            signal_id: "test_signal".to_owned(),
            label: "Test signal".to_owned(),
            unit: "V".to_owned(),
        },
    );
    h.frame(vec![Event::PointerMoved(target)]);
    let request = h.release(target, PointerButton::Primary);
    assert_eq!(request.range, None);
    assert_eq!(request.cursor, None);
    assert_eq!(h.scope.channels.len(), 4);
}

#[test]
fn distance_selection_maps_back_to_shared_timestamps() {
    let mut h = Harness::new();
    h.scope.x_axis_mode = ScopeXAxisMode::TrackDistance;
    h.scope.speed_signal_id = SPEED.to_owned();
    for _ in 0..3 {
        h.frame(vec![]);
    }
    let a = h.point(0, 0.25);
    let b = h.point(0, 0.81);
    h.press(a, PointerButton::Secondary);
    h.frame(vec![Event::PointerMoved(b)]);
    let (start, end) = h
        .release(b, PointerButton::Secondary)
        .range
        .expect("Distance range maps to time");
    // Speed is 2t so traveled distance is t squared.
    assert!((start - BASE - 5_000).abs() < 100);
    assert!((end - BASE - 9_000).abs() < 100);
}

#[test]
fn legend_appearance_and_axis_menus_work() {
    let mut h = Harness::new();
    h.scope.show_legend = true;
    for _ in 0..2 {
        h.frame(vec![]);
    }
    let speed = h.find("Speed");
    h.press(speed, PointerButton::Secondary);
    h.release(speed, PointerButton::Secondary);
    h.frame(vec![]);
    let appearance = h.find("Appearance");
    h.click(appearance);
    let mode = h.find("Solid");
    h.click(mode);
    let steps = h.find("Steps");
    h.click(steps);
    assert_eq!(
        h.scope.config.style(SPEED).mode,
        ui::scope_settings::TraceMode::Steps
    );
    // Close menus without changing the view.
    h.frame(vec![Event::Key {
        key: egui::Key::Escape,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    }]);
    egui::Popup::close_all(&h.ctx);
    h.frame(vec![]);
    let speed = h.find("Speed");
    h.press(speed, PointerButton::Secondary);
    h.release(speed, PointerButton::Secondary);
    h.frame(vec![]);
    let axis = h.find("Axis");
    h.click(axis);
    let fixed = h.find("Fixed");
    h.click(fixed);
    assert!(h.scope.config.axes["0|m/s"].range.is_some());
    egui::Popup::close_all(&h.ctx);
    for _ in 0..3 {
        h.frame(vec![]);
    }
    let colors: Vec<_> = h
        .scope
        .channels
        .iter()
        .map(|channel| channel.color)
        .collect();
    // Removing a different channel must never recolor the survivors.
    let speed_color = h.scope.channels[0].color;
    h.scope.remove_channel(1);
    assert_eq!(h.scope.channels[0].color, speed_color);
    assert_eq!(
        h.scope.channels[1].color, colors[2],
        "Removing RPM must not recolor Throttle"
    );
}

#[test]
fn channels_window_assigns_signals_and_stays_open() {
    use ui::scope_settings::Destination;
    let mut h = Harness::new();
    h.scope = Scope::new_empty(1);
    for _ in 0..3 {
        h.frame(vec![]);
    }
    let button = h.find("ƒ");
    h.click(button);
    for _ in 0..3 {
        h.frame(vec![]);
    }
    assert!(matches!(
        h.scope.picker_target,
        Some(ScopePickerTarget::Channels(0, Destination::Left))
    ));
    let right = h.find("Right");
    h.click(right);
    let candidate = h.find(&format!("{SPEED} [m/s]"));
    h.click(candidate);
    assert_eq!(h.scope.channels.len(), 1);
    assert_eq!(
        h.scope.config.destination(&h.scope.channels[0]),
        Destination::Right
    );
    assert!(h.scope.picker_target.is_some());
}

#[test]
fn blank_scope_accepts_left_right_and_discrete_drops_without_picker() {
    use ui::scope_settings::Destination;
    for destination in [Destination::Left, Destination::Right, Destination::Discrete] {
        let mut h = Harness::new();
        h.scope = Scope::new_empty(1);
        for _ in 0..3 {
            h.frame(vec![]);
        }
        assert!(h.scope.picker_target.is_none());
        assert_eq!(h.scope.plot_frames.len(), 1);
        let frame = h.scope.plot_frames[0];
        let point = match destination {
            Destination::Left => egui::pos2(frame.left() + 20.0, frame.center().y),
            Destination::Right => egui::pos2(frame.right() - 20.0, frame.center().y),
            Destination::Discrete => egui::pos2(frame.center().x, frame.bottom() - 10.0),
        };
        h.press(egui::pos2(500.0, 17.0), PointerButton::Primary);
        egui::DragAndDrop::set_payload(
            &h.ctx,
            ui::signal_browser::DraggedSignal::Derived {
                signal_id: SPEED.to_owned(),
                label: "Speed".to_owned(),
                unit: "m/s".to_owned(),
            },
        );
        h.frame(vec![Event::PointerMoved(point)]);
        let request = h.release(point, PointerButton::Primary);
        assert_eq!(request.cursor, None);
        assert_eq!(h.scope.channels.len(), 1);
        assert_eq!(
            h.scope.config.destination(&h.scope.channels[0]),
            destination
        );
    }
}

#[test]
fn browser_drop_on_right_edge_creates_independent_axis() {
    let mut h = Harness::new();
    let frame = h.scope.plot_frames[0];
    let target = egui::pos2(frame.right() - 15.0, frame.center().y);
    h.press(egui::pos2(780.0, 17.0), PointerButton::Primary);
    egui::DragAndDrop::set_payload(
        &h.ctx,
        ui::signal_browser::DraggedSignal::Derived {
            signal_id: "right_signal".to_owned(),
            label: "Right signal".to_owned(),
            unit: "V".to_owned(),
        },
    );
    h.frame(vec![Event::PointerMoved(target)]);
    h.release(target, PointerButton::Primary);
    let channel = h
        .scope
        .channels
        .iter()
        .find(|c| c.signal_id == "right_signal")
        .unwrap();
    let key = h.scope.config.axis_key(channel);
    assert_eq!(
        h.scope.config.axes[&key].side,
        ui::scope_settings::AxisSide::Right
    );
}

#[test]
fn signals_move_across_scopes_from_every_source_and_destination() {
    use ui::scope_settings::Destination;
    fn frame(
        ctx: &egui::Context,
        router: &daqcore::router::VirtualTimelineRouter,
        source: &mut Scope,
        target: &mut Scope,
        events: Vec<Event>,
        reverse: bool,
    ) -> egui::FullOutput {
        let view = workspace_view::WorkspaceDataView::new(router);
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    Pos2::ZERO,
                    egui::vec2(1200.0, 650.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.columns(2, |columns| {
                        if reverse {
                            let _ = target.show(&mut columns[1], &view);
                            let _ = source.show(&mut columns[0], &view);
                        } else {
                            let _ = source.show(&mut columns[0], &view);
                            let _ = target.show(&mut columns[1], &view);
                        }
                    });
                });
            },
        );
        assert_eq!(view.requested_range_ms(), None);
        assert_eq!(view.requested_setpoint_utc_ms(), None);
        output
    }
    for reverse in [false, true] {
        for source_kind in ["left", "right", "discrete", "floating"] {
            for destination in [Destination::Left, Destination::Right, Destination::Discrete] {
                let mut h = Harness::new();
                h.scope.show_legend = source_kind == "floating";
                let channel = h.scope.channels[0].clone();
                let source_destination = match source_kind {
                    "right" => Destination::Right,
                    "discrete" => Destination::Discrete,
                    _ => Destination::Left,
                };
                h.scope.config.assign(&channel, 0, source_destination);
                h.scope.config.channels.get_mut(SPEED).unwrap().width = 3.2;
                let key = h.scope.config.axis_key(&channel);
                h.scope.config.axes.entry(key).or_default().range = Some([0.0, 50.0]);
                let mut target = Scope::new_empty(2);
                for _ in 0..3 {
                    h.output = Some(frame(
                        &h.ctx,
                        &h.router,
                        &mut h.scope,
                        &mut target,
                        vec![],
                        reverse,
                    ));
                }
                let start = h.find("Speed");
                let rect = target.plot_frames[0];
                let end = match destination {
                    Destination::Left => egui::pos2(rect.left() + 20.0, rect.center().y),
                    Destination::Right => egui::pos2(rect.right() - 20.0, rect.center().y),
                    Destination::Discrete => egui::pos2(rect.center().x, rect.bottom() - 10.0),
                };
                frame(
                    &h.ctx,
                    &h.router,
                    &mut h.scope,
                    &mut target,
                    vec![Event::PointerMoved(start)],
                    reverse,
                );
                frame(
                    &h.ctx,
                    &h.router,
                    &mut h.scope,
                    &mut target,
                    vec![Event::PointerButton {
                        pos: start,
                        button: PointerButton::Primary,
                        pressed: true,
                        modifiers: Modifiers::NONE,
                    }],
                    reverse,
                );
                for _ in 0..2 {
                    frame(
                        &h.ctx,
                        &h.router,
                        &mut h.scope,
                        &mut target,
                        vec![Event::PointerMoved(end)],
                        reverse,
                    );
                }
                assert!(
                    egui::DragAndDrop::payload::<ui::scope_legend::SignalDrag>(&h.ctx).is_some(),
                    "Drag starts from {source_kind} at {start:?} to {end:?} reverse {reverse} destination {destination:?}"
                );
                frame(
                    &h.ctx,
                    &h.router,
                    &mut h.scope,
                    &mut target,
                    vec![Event::PointerButton {
                        pos: end,
                        button: PointerButton::Primary,
                        pressed: false,
                        modifiers: Modifiers::NONE,
                    }],
                    reverse,
                );
                frame(
                    &h.ctx,
                    &h.router,
                    &mut h.scope,
                    &mut target,
                    vec![],
                    reverse,
                );
                assert!(
                    !h.scope.channels.iter().any(|ch| ch.signal_id == SPEED),
                    "Source removed after accepted {source_kind} drag"
                );
                assert_eq!(target.channels.len(), 1);
                assert_eq!(target.channels[0].signal_id, SPEED);
                assert_eq!(target.channels[0].color, channel.color);
                assert_eq!(target.config.style(SPEED).width, 3.2);
                assert_eq!(target.config.destination(&target.channels[0]), destination);
                if destination != Destination::Discrete {
                    assert_eq!(
                        target.config.axes[&target.config.axis_key(&target.channels[0])].range,
                        Some([0.0, 50.0])
                    );
                }
            }
        }
    }
}

#[test]
fn discrete_band_can_be_dragged_back_to_either_axis() {
    use ui::scope_settings::Destination;
    for (destination, gutter) in [Destination::Left, Destination::Right]
        .into_iter()
        .flat_map(|dest| [(dest, false), (dest, true)])
    {
        let mut h = Harness::new();
        let channel = h.scope.channels[0].clone();
        h.scope.config.assign(&channel, 0, Destination::Discrete);
        for _ in 0..3 {
            h.frame(vec![]);
        }
        let label = h.find("Speed");
        let plot = h.scope.plot_frames[0];
        let start = egui::pos2(
            if gutter {
                plot.left() - 20.0
            } else {
                plot.center().x
            },
            label.y + 20.0,
        );
        let end = egui::pos2(
            if destination == Destination::Left {
                plot.left() + 20.0
            } else {
                plot.right() - 20.0
            },
            plot.center().y,
        );
        h.press(start, PointerButton::Primary);
        h.frame(vec![Event::PointerMoved(end)]);
        h.frame(vec![Event::PointerMoved(end)]);
        assert!(egui::DragAndDrop::payload::<ui::scope_legend::SignalDrag>(&h.ctx).is_some());
        h.release(end, PointerButton::Primary);
        h.frame(vec![]);
        assert_eq!(
            h.scope.config.destination(&h.scope.channels[0]),
            destination
        );
    }
}

#[test]
fn cancelled_signal_drag_leaves_source_unchanged() {
    let mut h = Harness::new();
    let original = h.scope.channels.len();
    let start = h.find("Speed");
    h.press(start, PointerButton::Primary);
    h.frame(vec![Event::PointerMoved(egui::pos2(500.0, 17.0))]);
    assert!(egui::DragAndDrop::has_any_payload(&h.ctx));
    h.frame(vec![Event::Key {
        key: egui::Key::Escape,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    }]);
    h.release(egui::pos2(500.0, 17.0), PointerButton::Primary);
    assert_eq!(h.scope.channels.len(), original);
    assert!(
        h.scope
            .channels
            .iter()
            .any(|channel| channel.signal_id == SPEED)
    );
}

#[test]
fn overflowing_legend_remains_searchable_and_solo_is_reversible() {
    let mut h = Harness::new();
    h.scope = Scope::new_multi(
        1,
        "Overflow",
        (0..30)
            .map(|index| {
                (
                    format!("message.signal_{index}"),
                    format!("Signal {index:02}"),
                    "V".to_owned(),
                )
            })
            .collect(),
    );
    for _ in 0..2 {
        h.frame(vec![]);
    }
    let overflow = h
        .output
        .as_ref()
        .unwrap()
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.text().ends_with(" more") => {
                Some(text.pos + text.galley.size() * 0.5)
            }
            _ => None,
        })
        .expect("Overflow button");
    let first = h.find("Signal 00");
    assert!((first.y - overflow.y).abs() < 4.0);
    h.click(first);
    h.click(first);
    assert_eq!(h.scope.solo_channel.as_deref(), Some("message.signal_0"));
    let exit = h.find("Exit solo");
    h.click(exit);
    assert!(h.scope.solo_channel.is_none());

    h.click(overflow);
    let _ = h.find("Filter channels");
    assert!(egui::Popup::is_any_open(&h.ctx));

    let filter = h.find("Filter channels");
    h.click(filter);
    h.frame(vec![Event::Text("29".to_owned())]);
    for _ in 0..3 {
        h.frame(vec![]);
    }
    let source = h.find("Signal 29");
    let target = h.point(0, 0.75);
    h.press(source, PointerButton::Primary);
    assert!(egui::Popup::is_any_open(&h.ctx), "Popup hit at {source:?}");
    h.frame(vec![Event::PointerMoved(target)]);
    h.frame(vec![Event::PointerMoved(target)]);
    let payload = egui::DragAndDrop::payload::<ui::scope_legend::SignalDrag>(&h.ctx)
        .expect("Overflow channel starts a drag");
    assert_eq!(payload.channel.signal_id, "message.signal_29");
    assert!(
        !egui::Popup::is_any_open(&h.ctx),
        "The list clears the drop target"
    );
    h.release(target, PointerButton::Primary);
    h.frame(vec![]);
    let channel = h
        .scope
        .channels
        .iter()
        .find(|c| c.signal_id == "message.signal_29")
        .unwrap();
    assert_eq!(
        h.scope.config.destination(channel),
        ui::scope_settings::Destination::Right
    );
}

#[test]
fn same_unit_quantities_have_distinct_axes_and_can_move_as_a_group() {
    use ui::scope_settings::{AxisConfig, AxisSide};

    let mut h = Harness::new();
    h.scope = Scope::new_multi(
        1,
        "Torque and Throttle",
        [
            ("INVA_CRIT.AMK_ActualTorque", "Torque A", "%"),
            ("INVB_CRIT.AMK_ActualTorque", "Torque B", "%"),
            (THROTTLE, "Throttle", "%"),
        ]
        .into_iter()
        .map(|(signal, label, unit)| (signal.to_owned(), label.to_owned(), unit.to_owned()))
        .collect(),
    );
    let torque = h.scope.config.axis_key(&h.scope.channels[0]);
    let throttle = h.scope.config.axis_key(&h.scope.channels[2]);
    assert_eq!(h.scope.config.axis_key(&h.scope.channels[1]), torque);
    assert_ne!(torque, throttle);

    h.scope.config.axes.insert(
        "0|%".to_owned(),
        AxisConfig {
            range: Some([-100.0, 100.0]),
            ..Default::default()
        },
    );
    assert_eq!(
        h.scope.config.axis_config(&torque).unwrap().range,
        Some([-100.0, 100.0])
    );
    h.scope.config.axes.insert(
        torque,
        AxisConfig {
            side: AxisSide::Right,
            range: Some([-200.0, 200.0]),
            ..Default::default()
        },
    );
    let sides = h.scope.config.resolved_axis_sides(&h.scope.channels);
    assert_eq!(sides[&throttle], AxisSide::Left);

    h.scope.add_channel(
        RPM.to_owned(),
        "RPM".to_owned(),
        "rpm".to_owned(),
        None,
        false,
    );
    for _ in 0..4 {
        h.frame(vec![]);
    }
    let output = h.output.as_ref().unwrap();
    for unit in ["%", "rpm"] {
        let suffix = format!("\n{unit}");
        assert!(
            output.shapes.iter().any(|shape| {
                matches!(&shape.shape, egui::Shape::Text(text) if text.angle == 0.0
                    && text.galley.rows.len() == 2
                    && text.galley.text().strip_suffix(&suffix).is_some_and(|value| value.parse::<f64>().is_ok()))
            }),
            "Missing a unit below the tick value for {unit}"
        );
    }
    assert!(!output.shapes.iter().any(|shape| {
        matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == "Torque (%)")
    }));

    let entry = h.find("Torque A");
    h.press(entry, PointerButton::Secondary);
    h.release(entry, PointerButton::Secondary);
    h.frame(vec![]);
    let _ = h.find("Right · Torque (%)");
}
