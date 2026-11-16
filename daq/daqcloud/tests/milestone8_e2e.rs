use daqcloud::{AppState, build_router};
use daqcore::{
    router::{Resolution, SignalSeries, VirtualTimelineRouter},
    storage::{
        Annotation, compute_file_sha256_hex, download_file_resumable,
        sync_store_with_cloud_blocking,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn milestone8_two_instance_per26_sync_and_realtime_annotations() {
    let manifest_dir = path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dbc_path = manifest_dir.join("../../superdbc/superdbc_per26.json");
    let log_path =
        manifest_dir.join("../../PER26/2026-09-06/2_regen/raw/log-2026-09-06--20-21-44.log");
    assert!(
        dbc_path.is_file() && log_path.is_file(),
        "PER26 dataset and superdbc_per26.json must exist"
    );

    let (base_url, _cloud_tmp, server_handle) = spawn_local_daqcloud().await;

    // -------------------------------------------------------------------------
    // 1. Instance A: Import PER26 SD card log and connect to daqcloud
    // -------------------------------------------------------------------------
    let db = Database::load(&dbc_path).expect("load superdbc_per26.json");
    let raw_dir_a = tempfile::tempdir().expect("raw_dir_a");
    fs::copy(
        &log_path,
        raw_dir_a.path().join("log-2026-09-06--20-21-44.log"),
    )
    .expect("copy PER26 raw log");

    let store_dir_a = tempfile::tempdir().expect("store_dir_a");
    let import_summary =
        daqcore::log_parse::import_logs_to_store(raw_dir_a.path(), store_dir_a.path(), &db)
            .expect("import PER26 log into Instance A store");
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

    router_a.disconnect_cloud();
    router_b.disconnect_cloud();
    server_handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resumable_partial_download_and_cli_sync_roundtrip() {
    let manifest_dir = path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dbc_path = manifest_dir.join("../../superdbc/superdbc_per26.json");
    let log_path =
        manifest_dir.join("../../PER26/2026-09-06/2_regen/raw/log-2026-09-06--20-21-44.log");
    assert!(dbc_path.is_file() && log_path.is_file());

    let (base_url, _cloud_tmp, server_handle) = spawn_local_daqcloud().await;

    let db = Database::load(&dbc_path).unwrap();
    let raw_dir = tempfile::tempdir().unwrap();
    fs::copy(
        &log_path,
        raw_dir.path().join("log-2026-09-06--20-21-44.log"),
    )
    .unwrap();

    let store_a = tempfile::tempdir().unwrap();
    let import_summary =
        daqcore::log_parse::import_logs_to_store(raw_dir.path(), store_a.path(), &db).unwrap();
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

    server_handle.abort();
}
