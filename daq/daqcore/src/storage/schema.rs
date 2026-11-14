// @generated automatically by Diesel CLI.

diesel::table! {
    annotations (id) {
        id -> Text,
        track_day_id -> Text,
        name -> Text,
        category -> Text,
        color -> Text,
        start_utc_ms -> BigInt,
        end_utc_ms -> Nullable<BigInt>,
        author -> Text,
        source -> Text,
        notes -> Text,
        updated_at_ms -> BigInt,
        deleted -> Bool,
    }
}

diesel::table! {
    partitions (partition_id) {
        partition_id -> Text,
        track_day_id -> Text,
        start_utc_ms -> BigInt,
        end_utc_ms -> BigInt,
        clock_confidence -> Text,
        superdbc_hash -> Text,
        relative_path -> Text,
        is_local -> Bool,
        manifest_json -> Text,
    }
}

diesel::table! {
    raw_assets (sha256_hex) {
        sha256_hex -> Text,
        track_day_id -> Text,
        file_name -> Text,
        rtc_timestamp_ms -> Nullable<BigInt>,
        mtime_unix_ms -> BigInt,
        relative_path -> Text,
        is_local -> Bool,
    }
}

diesel::table! {
    track_days (id) {
        id -> Text,
        title -> Text,
        location -> Text,
        start_utc_ms -> BigInt,
        end_utc_ms -> BigInt,
    }
}

diesel::joinable!(annotations -> track_days (track_day_id));
diesel::joinable!(partitions -> track_days (track_day_id));
diesel::joinable!(raw_assets -> track_days (track_day_id));

diesel::allow_tables_to_appear_in_same_query!(annotations, partitions, raw_assets, track_days,);
