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
    fn new(stacked: bool) -> Self {
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
        let mut scope = Scope::new_multi(
            1,
            "Workflow",
            vec![
                (SPEED.to_owned(), "Speed".to_owned(), "m/s".to_owned()),
                (RPM.to_owned(), "RPM".to_owned(), "rpm".to_owned()),
                (THROTTLE.to_owned(), "Throttle".to_owned(), String::new()),
            ],
        );
        if stacked {
            scope.config.toggle_stack(&scope.channels);
        }
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
    let mut h = Harness::new(false);
    let a = h.point(0, 0.2);
    let b = h.point(0, 0.8);
    assert!(h.press(a, PointerButton::Primary).pause);
    assert_eq!(h.frame(vec![Event::PointerMoved(b)]).pan, None);
    let requests = h.release(b, PointerButton::Primary);
    let (start, end) = requests.range.expect("Left drag selects the shared window");
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
    h.press(a, PointerButton::Primary);
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
    assert_eq!(h.release(b, PointerButton::Primary).range, None);
    assert!(h.scope.gesture.is_none());
}

#[test]
fn right_drag_pans_but_click_scrubs() {
    let mut h = Harness::new(false);
    let a = h.point(0, 0.6);
    let b = h.point(0, 0.4);
    h.press(a, PointerButton::Secondary);
    let request = h.frame(vec![Event::PointerMoved(b)]);
    assert!((request.pan.unwrap() - 2_000).abs() < 20);
    assert_eq!(request.range, None);
    h.release(b, PointerButton::Secondary);
    let point = h.point(0, 0.4);
    let request = h.click(point);
    assert!((request.cursor.unwrap() - BASE - 4_000).abs() < 20);
    assert_eq!(request.range, None);
}

#[test]
fn stacked_plots_align_and_axes_have_independent_fixed_ranges() {
    let mut h = Harness::new(true);
    h.scope
        .config
        .axes
        .entry("0|m/s".to_owned())
        .or_default()
        .range = Some([0.0, 50.0]);
    h.scope
        .config
        .axes
        .entry("1|rpm".to_owned())
        .or_default()
        .range = Some([0.0, 4_000.0]);
    for _ in 0..3 {
        h.frame(vec![]);
    }
    assert_eq!(h.scope.plot_frames.len(), 2);
    let first = h.scope.plot_frames[0];
    let second = h.scope.plot_frames[1];
    assert!((first.left() - second.left()).abs() <= 1.0);
    assert!((first.right() - second.right()).abs() <= 1.0);
    assert!(first.bottom() < second.top());
    let cache = h.scope.cached_data.as_ref().unwrap();
    for (key, expected) in [("0|m/s", 50.0), ("1|rpm", 4_000.0)] {
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
    h.scope.config.lanes = 8;
    for size in [egui::vec2(320.0, 240.0), egui::vec2(120.0, 100.0)] {
        h.size = size;
        h.frame(vec![]);
    }
}

#[test]
fn measurements_pin_drag_and_report_signal_deltas_without_scrubbing() {
    let mut h = Harness::new(true);
    h.click(egui::pos2(76.0, 17.0));
    assert!(h.scope.measurements.enabled);
    let a = h.point(0, 0.3);
    let b = h.point(1, 0.7);
    assert_eq!(h.click(a).cursor, None);
    assert_eq!(h.click(b).cursor, None);
    let [Some(first), Some(second)] = h.scope.measurements.pins else {
        panic!("Two pinned cursors");
    };
    assert!((first - BASE - 3_000).abs() < 20);
    assert!((second - BASE - 7_000).abs() < 20);
    let start = h.point(1, 0.3);
    let end = h.point(1, 0.4);
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
fn browser_drop_assigns_the_target_lane() {
    let mut h = Harness::new(true);
    let target = h.scope.plot_frames[1].center();
    h.press(egui::pos2(500.0, 17.0), PointerButton::Primary);
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
    assert_eq!(h.scope.config.lane("test_signal"), 1);
}

#[test]
fn distance_selection_maps_back_to_shared_timestamps() {
    let mut h = Harness::new(false);
    h.scope.x_axis_mode = ScopeXAxisMode::TrackDistance;
    h.scope.speed_signal_id = SPEED.to_owned();
    for _ in 0..3 {
        h.frame(vec![]);
    }
    let a = h.point(0, 0.25);
    let b = h.point(0, 0.81);
    h.press(a, PointerButton::Primary);
    h.frame(vec![Event::PointerMoved(b)]);
    let (start, end) = h
        .release(b, PointerButton::Primary)
        .range
        .expect("Distance range maps to time");
    // Speed is 2t so traveled distance is t squared.
    assert!((start - BASE - 5_000).abs() < 100);
    assert!((end - BASE - 9_000).abs() < 100);
}

#[test]
fn legend_appearance_axis_menus_and_lane_drag_work() {
    let mut h = Harness::new(true);
    h.scope.show_legend = true;
    for _ in 0..2 {
        h.frame(vec![]);
    }
    let speed = h.find(SPEED);
    h.click(speed);
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
    let speed = h.find(SPEED);
    h.click(speed);
    let axis = h.find("Axis");
    h.click(axis);
    let fixed = h.find("Fixed");
    h.click(fixed);
    assert!(h.scope.config.axes["0|m/s"].range.is_some());
    egui::Popup::close_all(&h.ctx);
    for _ in 0..3 {
        h.frame(vec![]);
    }
    let first = h.scope.plot_frames[0];
    // Speed is the first row of the two-row upper legend.
    let source = first.left_bottom() + egui::vec2(20.0, -38.0);
    let target = h.scope.plot_frames[1].center();
    let colors: Vec<_> = h
        .scope
        .channels
        .iter()
        .map(|channel| channel.color)
        .collect();
    h.press(source, PointerButton::Primary);
    h.frame(vec![Event::PointerMoved(target)]);
    h.release(target, PointerButton::Primary);
    h.frame(vec![]);
    assert_eq!(h.scope.config.lane(SPEED), 1);
    assert_eq!(h.scope.channels.len(), 3);
    assert_eq!(
        h.scope
            .channels
            .iter()
            .map(|channel| channel.color)
            .collect::<Vec<_>>(),
        colors
    );
    // Removing a different channel must never recolor the survivors.
    let speed_color = h.scope.channels[0].color;
    h.scope.remove_channel(1);
    assert_eq!(h.scope.channels[0].color, speed_color);
    assert_eq!(
        h.scope.channels[1].color, colors[2],
        "Removing RPM must not recolor Throttle"
    );
}
