use daqcloud::{AppState, build_router};
use daqnative::{
    log_parse::{
        correlate::{ClockConfidence, ClockMapping, CorrelationChunkResult},
        parse::{ParsedMessage, SourceLogFile},
    },
    router::{Resolution, SignalSeries, VirtualTimelineRouter},
    storage::{
        Annotation, ImportSummary, NativeRouterExt as _, compute_file_sha256_hex,
        download_file_resumable, load_archived_superdbc, sync_store_with_cloud_blocking,
        write_correlated_epochs_to_store,
    },
    superdbc::database::Database,
    time::Time,
};
use std::{
    fs, path, thread,
    time::{Duration, Instant},
};

async fn spawn_local_daqcloud() -> (String, tempfile::TempDir, tokio::task::JoinHandle<()>) {
    let tmp = tempfile::tempdir().expect("create cloud tempdir");
    let db_path = tmp.path().join("cloud_registry.sqlite");
    let store_dir = tmp.path().join("cloud_store");
    let state = AppState::open(&db_path, &store_dir).expect("open AppState");
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    let base_url = format!("http://{addr}");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (base_url, tmp, handle)
}

fn synthetic_superdbc_json() -> &'static str {
    r#"{
      "versions": { "schema_version": "1.1", "hash": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef" },
      "content_hash": "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210",
      "buses": {
        "VCAN": {
          "bus_id": 0,
          "baud_rate": 500000,
          "nodes": [{ "name": "MAIN", "is_external": false }],
          "messages": [
            {
              "id": 100,
              "is_extended_id": false,
              "message_name": "IMU_acceleration",
              "transmitter": "MAIN",
              "receivers": [],
              "length_bytes": 2,
              "nominal_period_ms": 100,
              "priority": 2,
              "description": "Longitudinal acceleration",
              "signals": [
                {
                  "signal_name": "X_axis",
                  "description": "Longitudinal acceleration",
                  "data_type": "float",
                  "raw_type": "signed",
                  "start_bit": 0,
                  "bit_length": 16,
                  "byte_order": "little_endian",
                  "scale": 0.01,
                  "offset": 0.0,
                  "limits": null,
                  "unit": "g",
                  "choices": null,
                  "display_format": null
                }
              ]
            },
            {
              "id": 101,
              "is_extended_id": false,
              "message_name": "main_hb",
              "transmitter": "MAIN",
              "receivers": [],
              "length_bytes": 1,
              "nominal_period_ms": 500,
              "priority": 2,
              "description": "Main heartbeat",
              "signals": [
                {
                  "signal_name": "car_state",
                  "description": "",
                  "data_type": "car_state_t",
                  "raw_type": "unsigned",
                  "start_bit": 0,
                  "bit_length": 8,
                  "byte_order": "little_endian",
                  "scale": 1.0,
                  "offset": 0.0,
                  "limits": null,
                  "unit": "",
                  "choices": { "1": "idle", "5": "ready2drive" },
                  "display_format": null
                }
              ]
            }
          ]
        }
      }
    }"#
}

fn prepare_fixture_store(store_dir: &path::Path) -> (Database, ImportSummary) {
    let manifest_dir = path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dbc_path = manifest_dir.join("../../superdbc/superdbc_per26.json");
    let log_path =
        manifest_dir.join("../../PER26/2026-09-06/2_regen/raw/log-2026-09-06--20-21-44.log");

    // Verify synthetic fallback SuperDBC parses cleanly even when PER26/ fixtures exist locally.
    let _ = Database::from_json(synthetic_superdbc_json()).expect("valid synthetic superdbc");

    if dbc_path.is_file() && log_path.is_file() {
        let db = Database::load(&dbc_path).expect("load superdbc_per26.json");
        let raw_dir = tempfile::tempdir().expect("raw_dir");
        fs::copy(
            &log_path,
            raw_dir.path().join("log-2026-09-06--20-21-44.log"),
        )
        .expect("copy PER26 raw log");
        let summary = daqnative::log_parse::import_logs_to_store(raw_dir.path(), store_dir, &db)
            .expect("import PER26 log into store");
        return (db, summary);
    }

    // CI fallback when gitignored PER26/ fixtures are absent: generate a real Parquet pyramid
    // partition + content-addressed SuperDBC using the exact same storage writer.
    let db = Database::from_json(synthetic_superdbc_json()).expect("valid synthetic superdbc");
    let base_utc_ms: i64 = 1_788_726_104_000; // 2026-09-06T20:21:44Z
    let mut parsed_msgs = Vec::new();
    for step in 0..=80u32 {
        let ticks_ms = step * 100;
        let accel_g = ((step as f64) * 0.1).sin();
        let accel_payload = db.encode(0, 100, &[("X_axis", accel_g)]).unwrap();
        parsed_msgs.push(ParsedMessage {
            timestamp: ticks_ms,
            source_file_idx: 0,
            bus_name: "VCAN".to_owned(),
            decoded: db.decode(0, 100, &accel_payload).unwrap(),
        });
        if step.is_multiple_of(10) {
            let state = if step < 40 { 1.0 } else { 5.0 };
            let hb_payload = db.encode(0, 101, &[("car_state", state)]).unwrap();
            parsed_msgs.push(ParsedMessage {
                timestamp: ticks_ms,
                source_file_idx: 0,
                bus_name: "VCAN".to_owned(),
                decoded: db.decode(0, 101, &hb_payload).unwrap(),
            });
        }
    }

    let source_files = vec![SourceLogFile {
        file_name: "log-2026-09-06--20-21-44.log".to_owned(),
        sha256_hex: "synthetic_ci_log_sha256".to_owned(),
        rtc_timestamp_ms: Some(base_utc_ms),
        mtime_unix_ms: base_utc_ms,
    }];
    let epochs = vec![CorrelationChunkResult {
        clock_mapping: ClockMapping {
            slope: 1.0,
            intercept_ms: base_utc_ms as f64,
            confidence: ClockConfidence::RtcEstimated,
        },
        parsed_msgs,
    }];
    let summary = write_correlated_epochs_to_store(store_dir, &source_files, epochs, &db)
        .expect("write synthetic store");
    (db, summary)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn milestone8_two_instance_per26_sync_and_realtime_annotations() {
    let (base_url, _cloud_tmp, server_handle) = spawn_local_daqcloud().await;

    // -------------------------------------------------------------------------
    // 1. Instance A: Import SD card log (or CI synthetic fixture) and connect to daqcloud
    // -------------------------------------------------------------------------
    let store_dir_a = tempfile::tempdir().expect("store_dir_a");
    let (_db, import_summary) = prepare_fixture_store(store_dir_a.path());
    assert_eq!(import_summary.partitions_written, 1);
    let manifest_a = &import_summary.manifests[0];
    let part_id = manifest_a.partition_id.clone();

    let mut router_a = VirtualTimelineRouter::open_store(
        store_dir_a.path(),
        Time::from_unix_millis(manifest_a.end_utc_ms),
        30.0,
    )
    .expect("open Router A");
    router_a
        .connect_cloud(&base_url)
        .expect("connect Router A to daqcloud");

    // -------------------------------------------------------------------------
    // 2. Instance B: Point at an empty local folder and connect to daqcloud
    // -------------------------------------------------------------------------
    let store_dir_b = tempfile::tempdir().expect("store_dir_b");
    let mut router_b = VirtualTimelineRouter::open_store(
        store_dir_b.path(),
        Time::from_unix_millis(1_700_000_000_000),
        30.0,
    )
    .expect("open Router B on empty folder");
    assert!(router_b.partitions().is_empty());
    router_b
        .connect_cloud(&base_url)
        .expect("connect Router B to daqcloud");

    // Wait for Instance A to finish uploading and Instance B to receive the partition & 1Hz overview
    let sync_deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < sync_deadline {
        router_a.poll_cloud_sync();
        router_b.poll_cloud_sync();
        if router_b.partitions().len() == 1
            && router_b.tile_cache().is_partition_preloaded(&part_id)
        {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }

    // Verify Instance B sees the Track Day on its timeline and has preloaded 1Hz overview + states,
    // while `full_res.parquet` is NOT yet downloaded locally (`is_local == false`).
    let days_b = router_b.list_track_days().expect("list track days on B");
    assert_eq!(days_b.len(), 1);
    assert_eq!(days_b[0].id, "2026-09-06");
    assert_eq!(
        router_b.timeline().active_track_day_id(),
        Some("2026-09-06")
    );
    assert_eq!(
        router_b.timeline().day_bounds_ms(),
        Some((manifest_a.start_utc_ms, manifest_a.end_utc_ms))
    );

    let part_b = &router_b.partitions()[0];
    assert_eq!(part_b.partition_id, part_id);
    assert!(
        !part_b.is_local,
        "Instance B should only have 1Hz overview + states initially (is_local == false)"
    );
    let part_dir_b = store_dir_b.path().join("partitions").join(&part_id);
    assert!(part_dir_b.join("overview_1hz.parquet").is_file());
    assert!(part_dir_b.join("states.parquet").is_file());
    assert!(!part_dir_b.join("full_res.parquet").is_file());

    // Verify Instance B immediately streams the 1 Hz overview and activity envelope across the day
    let full_span = manifest_a.start_utc_ms..=manifest_a.end_utc_ms;
    let ov1_b = router_b.query_signal(
        "IMU_acceleration.X_axis",
        full_span.clone(),
        Resolution::Overview1Hz,
    );
    let ov1_a = router_a.query_signal(
        "IMU_acceleration.X_axis",
        full_span.clone(),
        Resolution::Overview1Hz,
    );
    assert!(!ov1_b.is_empty());
    assert_eq!(ov1_b.len(), ov1_a.len());
    assert!(!router_b.activity_envelope_1hz(full_span).is_empty());

    // -------------------------------------------------------------------------
    // 3. Instance B zooms into a 5-second window -> downloads full_res.parquet on demand
    // -------------------------------------------------------------------------
    let zoom_range = manifest_a.start_utc_ms..=(manifest_a.start_utc_ms + 5_000);
    router_b
        .timeline_mut()
        .snap_to_range_ms(*zoom_range.start(), *zoom_range.end());

    // First non-blocking FullRes query returns the resident 1Hz Envelope fallback and triggers download
    let initial_zoom_series = router_b.query_signal(
        "IMU_acceleration.X_axis",
        zoom_range.clone(),
        Resolution::FullRes,
    );
    assert!(
        matches!(initial_zoom_series, SignalSeries::Envelope(_)),
        "Non-blocking query before download completes should return 1Hz Envelope fallback"
    );

    // Wait for background download of overview_10hz.parquet + full_res.parquet and TileCache load
    let dl_deadline = Instant::now() + Duration::from_secs(10);
    let mut exact_b = None;
    while Instant::now() < dl_deadline {
        router_b.poll_cloud_sync();
        router_b.poll_background_tiles();
        let series = router_b.query_signal(
            "IMU_acceleration.X_axis",
            zoom_range.clone(),
            Resolution::FullRes,
        );
        if matches!(series, SignalSeries::Exact { .. }) && !series.is_empty() {
            exact_b = Some(series);
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }

    let exact_b =
        exact_b.expect("Instance B should download full_res.parquet and serve Exact samples");
    assert!(
        router_b.partitions()[0].is_local,
        "is_local should be flipped to true in local_registry.sqlite after download"
    );
    assert!(part_dir_b.join("overview_10hz.parquet").is_file());
    assert!(part_dir_b.join("full_res.parquet").is_file());

    let exact_a = router_a
        .query_signal_blocking(
            "IMU_acceleration.X_axis",
            zoom_range.clone(),
            Resolution::FullRes,
        )
        .expect("Router A exact query");
    assert_eq!(exact_b, exact_a);

    // -------------------------------------------------------------------------
    // 4. Real-time annotation sync via WebSocket (< 100 ms latency)
    // -------------------------------------------------------------------------
    // Ensure both WebSocket connections are established before measuring live broadcast latency
    let ws_ready_deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < ws_ready_deadline {
        if matches!(
            router_b.cloud_status(),
            daqcore::storage::CloudSyncStatus::Connected {
                ws_connected: true,
                ..
            }
        ) {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }

    let live_ann = Annotation {
        id: "m8_live_teammate_annotation".to_owned(),
        track_day_id: "2026-09-06".to_owned(),
        name: "Regen Brake Balance Check".to_owned(),
        category: "driver_note".to_owned(),
        color: "#f59e0b".to_owned(),
        start_utc_ms: manifest_a.start_utc_ms + 1_000,
        end_utc_ms: Some(manifest_a.start_utc_ms + 4_000),
        author: "instance_a_engineer".to_owned(),
        source: "manual".to_owned(),
        notes: "Peak regen torque test".to_owned(),
        updated_at_ms: 1_794_500_123_456,
        deleted: false,
    };

    let t_send = Instant::now();
    router_a
        .upsert_annotation(&live_ann)
        .expect("Instance A upsert annotation");

    let mut received_latency = None;
    let ann_deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < ann_deadline {
        router_b.poll_cloud_sync();
        let anns = router_b
            .query_annotations_in_range(zoom_range.clone())
            .expect("query annotations on B");
        if anns
            .iter()
            .any(|a| a.id == live_ann.id && a.name == live_ann.name)
        {
            received_latency = Some(t_send.elapsed());
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }

    let latency =
        received_latency.expect("Instance B should receive live annotation via WebSocket");
    assert!(
        latency < Duration::from_millis(100),
        "Expected live annotation propagation in < 100 ms, took {latency:?}"
    );

    // Also verify soft-delete propagates over WebSocket in < 100 ms
    let t_del = Instant::now();
    router_a
        .soft_delete_annotation(&live_ann.id, live_ann.updated_at_ms + 100)
        .expect("Instance A soft delete annotation");

    let mut delete_latency = None;
    let del_deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < del_deadline {
        router_b.poll_cloud_sync();
        let anns = router_b
            .query_annotations_in_range(zoom_range.clone())
            .expect("query annotations on B after delete");
        if !anns.iter().any(|a| a.id == live_ann.id) {
            delete_latency = Some(t_del.elapsed());
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }

    let del_lat = delete_latency.expect("Instance B should receive soft-delete via WebSocket");
    assert!(
        del_lat < Duration::from_millis(100),
        "Expected live annotation deletion in < 100 ms, took {del_lat:?}"
    );

    // -------------------------------------------------------------------------
    // 5. Real-time derived signal (formula) sync + deletion via WebSocket
    // -------------------------------------------------------------------------
    let live_formula = daqcore::analysis::DerivedSignalDef {
        id: "m8.synced_accel_mag".to_owned(),
        label: "Synced Accel X Double".to_owned(),
        expression: "IMU_acceleration.X_axis * 2.0".to_owned(),
        unit: "g".to_owned(),
        updated_at_ms: 1_794_500_200_000,
        deleted: false,
    };
    router_a
        .save_derived_signal(live_formula.clone())
        .expect("Instance A save derived signal");

    let mut sig_synced = false;
    let sig_deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < sig_deadline {
        router_b.poll_cloud_sync();
        if router_b.formula_engine().contains(&live_formula.id) {
            sig_synced = true;
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    assert!(
        sig_synced,
        "Instance B should receive live derived signal via WebSocket"
    );

    router_a
        .delete_derived_signal(&live_formula.id)
        .expect("Instance A delete derived signal");
    let mut sig_deleted = false;
    let sig_del_deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < sig_del_deadline {
        router_b.poll_cloud_sync();
        if !router_b.formula_engine().contains(&live_formula.id) {
            sig_deleted = true;
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    assert!(
        sig_deleted,
        "Instance B should receive derived signal deletion via WebSocket"
    );

    router_a.disconnect_cloud();
    router_b.disconnect_cloud();
    server_handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resumable_partial_download_and_cli_sync_roundtrip() {
    let (base_url, _cloud_tmp, server_handle) = spawn_local_daqcloud().await;

    let store_a = tempfile::tempdir().unwrap();
    let (db, import_summary) = prepare_fixture_store(store_a.path());
    let part_id = &import_summary.manifests[0].partition_id;

    // 1. Upload store_a using synchronous CLI helper (`sync_store_with_cloud_blocking`)
    let up_summary =
        sync_store_with_cloud_blocking(store_a.path(), &base_url, false).expect("CLI sync upload");
    assert_eq!(up_summary.partitions_uploaded, 1);

    // 2. Simulate an interrupted download in `store_b` by writing the first half of `full_res.parquet`
    //    into `full_res.parquet.part`, then calling `download_file_resumable`.
    let orig_full_res_path = store_a
        .path()
        .join("partitions")
        .join(part_id)
        .join("full_res.parquet");
    let orig_bytes = fs::read(&orig_full_res_path).unwrap();
    let (_, orig_sha) = compute_file_sha256_hex(&orig_full_res_path).unwrap();
    assert!(orig_bytes.len() > 1024);

    let store_b = tempfile::tempdir().unwrap();
    let part_dir_b = store_b.path().join("partitions").join(part_id);
    fs::create_dir_all(&part_dir_b).unwrap();
    let half_len = orig_bytes.len() / 2;
    fs::write(
        part_dir_b.join("full_res.parquet.part"),
        &orig_bytes[..half_len],
    )
    .unwrap();

    let client = reqwest::Client::new();
    let resumed =
        download_file_resumable(&client, &base_url, part_id, "full_res.parquet", &part_dir_b)
            .await
            .expect("resume partial download");
    assert!(resumed);
    assert!(!part_dir_b.join("full_res.parquet.part").exists());
    let (_, resumed_sha) = compute_file_sha256_hex(&part_dir_b.join("full_res.parquet")).unwrap();
    assert_eq!(resumed_sha, orig_sha);

    // 3. Complete full store sync on `store_b` via `sync_store_with_cloud_blocking(..., true)`
    let down_summary = sync_store_with_cloud_blocking(store_b.path(), &base_url, true)
        .expect("CLI sync download_full");
    assert_eq!(down_summary.remote_track_days, 1);
    assert_eq!(down_summary.remote_partitions, 1);
    assert_eq!(down_summary.full_partitions_downloaded, 1);

    // 4. Verify content-addressed SuperDBC provenance archive (`superdbcs/<hash>.json`)
    //    was archived on store_a and synced to store_b via `/api/v1/superdbcs/{hash}`
    let dbc_hash = &import_summary.manifests[0].superdbc_hash;
    let archived_a = load_archived_superdbc(store_a.path(), dbc_hash)
        .expect("load archived superdbc on store_a")
        .expect("superdbc must exist on store_a");
    let archived_b = load_archived_superdbc(store_b.path(), dbc_hash)
        .expect("load archived superdbc on store_b")
        .expect("superdbc must be synced to store_b");
    assert_eq!(archived_a.content_hash(), db.content_hash());
    assert_eq!(archived_b.content_hash(), db.content_hash());
    assert_eq!(archived_b.version_hash(), db.version_hash());

    server_handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daqweb_routes_preload_tile_and_export_e2e() {
    let (base_url, _cloud_tmp, server_handle) = spawn_local_daqcloud().await;
    let client = reqwest::Client::new();

    // 1. Verify home route `/` and share route `/daqview/daqview1:...` serve the daqweb SPA HTML shell
    for path in ["/", "/daqview/daqview1:eJyrVspNLS5OTE8FAAnfA2E"] {
        let resp = client
            .get(format!("{base_url}{path}"))
            .send()
            .await
            .expect("GET web route");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let ct = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(ct.starts_with("text/html"));
        let html = resp.text().await.unwrap();
        assert!(html.contains("daqweb_canvas"));
    }

    // 2. Upload a real partition and verify `/api/v1/partitions/{id}/preload` and `/tile`
    let store_a = tempfile::tempdir().unwrap();
    let (_db, import_summary) = prepare_fixture_store(store_a.path());
    let m0 = &import_summary.manifests[0];
    let part_id = &m0.partition_id;
    let up_summary =
        sync_store_with_cloud_blocking(store_a.path(), &base_url, false).expect("CLI sync upload");
    assert_eq!(up_summary.partitions_uploaded, 1);

    let preload_bytes = client
        .get(format!("{base_url}/api/v1/partitions/{part_id}/preload"))
        .send()
        .await
        .expect("GET preload")
        .bytes()
        .await
        .expect("preload bytes");
    let decoded_preload =
        daqcore::router::decode_preload_payload(&preload_bytes).expect("decode DAQP preload");
    assert_eq!(decoded_preload.partition_id, *part_id);
    assert!(
        decoded_preload
            .overview_1hz
            .contains_key("IMU_acceleration.X_axis")
    );
    assert!(!decoded_preload.activity_1hz.is_empty());

    let tile_bytes = client
        .get(format!("{base_url}/api/v1/partitions/{part_id}/tile"))
        .query(&[
            ("signal_id", "IMU_acceleration.X_axis"),
            ("resolution", "full"),
        ])
        .send()
        .await
        .expect("GET tile")
        .bytes()
        .await
        .expect("tile bytes");
    let decoded_tile = daqcore::router::decode_tile_payload(&tile_bytes).expect("decode DAQT tile");
    assert!(matches!(decoded_tile, SignalSeries::Exact { .. }));
    assert!(!decoded_tile.is_empty());

    // 3. Verify `POST /api/v1/export` exports a valid ZSTD Parquet file
    let export_req = daqcore::export::DatasetExportRequest {
        start_utc_ms: m0.start_utc_ms,
        end_utc_ms: (m0.start_utc_ms + 3_000).min(m0.end_utc_ms),
        signal_patterns: vec!["IMU_acceleration.*".to_owned()],
        sampling_mode: daqcore::export::SamplingMode::UniformGridHz(50.0),
        format: daqcore::export::ExportFormat::Parquet,
        output_path: "export.parquet".to_owned(),
        custom_formulas: Vec::new(),
    };
    let export_resp = client
        .post(format!("{base_url}/api/v1/export"))
        .json(&export_req)
        .send()
        .await
        .expect("POST /api/v1/export");
    assert_eq!(export_resp.status(), reqwest::StatusCode::OK);
    let pq_bytes = export_resp.bytes().await.unwrap();
    assert!(pq_bytes.starts_with(b"PAR1"));

    // 4. Boot `daqweb::build_web_viewer_app` against this live `daqcloud` server using a `/daqview/daqview1:...`
    //    share URL, and verify automatic preload, on-demand FullRes tile streaming, and browser downloads!
    let custom_view = daqviewer::workspace_io::WorkspaceView {
        id: Some("e2e_shared_view".to_owned()),
        schema_version: 1,
        name: "E2E Shared Web View".to_owned(),
        description: "Restored via /daqview/ URL against live daqcloud".to_owned(),
        context: Some(daqviewer::workspace_io::WorkspaceTimeContext {
            track_day_id: Some(m0.track_day_id.clone()),
            viewport_start_utc_ms: m0.start_utc_ms + 200,
            viewport_end_utc_ms: (m0.start_utc_ms + 2_500).min(m0.end_utc_ms),
            setpoint_utc_ms: Some(m0.start_utc_ms + 1_000),
            compare_annotation_ids: Vec::new(),
            gap_folding_enabled: true,
        }),
        custom_formulas: vec![daqcore::analysis::DerivedSignalDef {
            id: "custom.double_accel_x".to_owned(),
            label: "2x Accel X".to_owned(),
            expression: "IMU_acceleration.X_axis * 2.0".to_owned(),
            unit: "G".to_owned(),
            ..Default::default()
        }],
        layout: Some(daqviewer::workspace_io::FlexibleLayout::Tree(
            daqviewer::workspace_io::WorkspaceLayoutNode::Pane {
                spec: daqviewer::workspace_io::PaneSpec::Scope {
                    title: "Restored Web Scope".to_owned(),
                    custom_title: true,
                    x_mode: daqviewer::ui::scope::ScopeXAxisMode::default(),
                    speed_signal_id: "IMU_acceleration.X_axis".to_owned(),
                    y_bounds: None,
                    channels: vec![daqviewer::workspace_io::ScopeChannelSpec {
                        signal_id: "custom.double_accel_x".to_owned(),
                        label: "2x Accel X".to_owned(),
                        unit: "G".to_owned(),
                        color_rgb: None,
                    }],
                    axes: Vec::new(),
                },
            },
        )),
        panes: Default::default(),
    };
    let share_str = custom_view.to_share_string().expect("encode share string");
    let browser =
        daqweb::platform::MemoryBrowserEnv::new(&base_url, format!("/daqview/{share_str}"));
    let egui_ctx = eframe::egui::Context::default();
    let mut web_app = daqweb::build_web_viewer_app(browser.clone(), Some(&egui_ctx));

    let preload_deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < preload_deadline {
        web_app.state.router.poll_cloud_sync();
        web_app.state.router.poll_background_tiles();
        daqviewer::ViewerPlatform::poll_platform(
            &mut web_app.platform,
            &mut web_app.state,
            &egui_ctx,
        );
        if web_app.state.router.captured_time_span().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(web_app.state.router.partitions().len(), 1);
    assert!(web_app.state.router.captured_time_span().is_some());
    assert_eq!(
        web_app.state.router.timeline().start_utc_ms(),
        m0.start_utc_ms + 200
    );

    // Trigger async FullRes tile stream via `WebTileLoader`
    let tile_deadline = Instant::now() + Duration::from_secs(5);
    let mut full_res_streamed = false;
    while Instant::now() < tile_deadline {
        web_app.state.router.poll_cloud_sync();
        web_app.state.router.poll_background_tiles();
        if let SignalSeries::Exact { timestamps_ms, .. } = web_app.state.router.query_signal(
            "IMU_acceleration.X_axis",
            m0.start_utc_ms..=m0.end_utc_ms,
            Resolution::FullRes,
        ) && !timestamps_ms.is_empty()
        {
            full_res_streamed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        full_res_streamed,
        "daqweb must stream FullRes binary tile from daqcloud"
    );

    // Trigger CSV and Parquet exports from `WebPlatform` and verify browser downloads
    daqviewer::ViewerPlatform::execute_dataset_export(
        &mut web_app.platform,
        &mut web_app.state.router,
        m0.start_utc_ms..=(m0.start_utc_ms + 2_000).min(m0.end_utc_ms),
        &["IMU_acceleration.X_axis".to_owned()],
        daqcore::export::SamplingMode::UniformGridHz(20.0),
        daqcore::export::ExportFormat::Csv,
        "web_export.csv",
    )
    .expect("web CSV export");
    daqviewer::ViewerPlatform::execute_dataset_export(
        &mut web_app.platform,
        &mut web_app.state.router,
        m0.start_utc_ms..=(m0.start_utc_ms + 2_000).min(m0.end_utc_ms),
        &["IMU_acceleration.X_axis".to_owned()],
        daqcore::export::SamplingMode::UniformGridHz(20.0),
        daqcore::export::ExportFormat::Parquet,
        "web_export.parquet",
    )
    .expect("web Parquet export");

    let dl_deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < dl_deadline {
        daqviewer::ViewerPlatform::poll_platform(
            &mut web_app.platform,
            &mut web_app.state,
            &egui_ctx,
        );
        if browser.downloads().len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let downloads = browser.downloads();
    assert_eq!(downloads.len(), 2);
    assert_eq!(downloads[0].filename, "web_export.csv");
    assert_eq!(downloads[1].filename, "web_export.parquet");
    assert!(downloads[1].bytes.starts_with(b"PAR1"));

    // Verify saving a derived signal in `daqweb` syncs it to `daqcloud`
    web_app
        .state
        .router
        .save_derived_signal(daqcore::analysis::DerivedSignalDef {
            id: "web.synced_triple_accel_x".to_owned(),
            label: "3x Accel X".to_owned(),
            expression: "IMU_acceleration.X_axis * 3.0".to_owned(),
            unit: "G".to_owned(),
            updated_at_ms: 1_794_600_000_000,
            deleted: false,
        })
        .expect("web save derived signal");

    let web_sig_deadline = Instant::now() + Duration::from_secs(5);
    let mut remote_has_web_sig = false;
    while Instant::now() < web_sig_deadline {
        web_app.state.router.poll_cloud_sync();
        if let Ok(resp) = client
            .get(format!("{base_url}/api/v1/derived-signals"))
            .send()
            .await
            && let Ok(sigs) = resp
                .json::<Vec<daqcore::analysis::DerivedSignalDef>>()
                .await
            && sigs.iter().any(|s| s.id == "web.synced_triple_accel_x")
        {
            remote_has_web_sig = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        remote_has_web_sig,
        "daqweb save_derived_signal must sync to daqcloud"
    );

    server_handle.abort();
}
