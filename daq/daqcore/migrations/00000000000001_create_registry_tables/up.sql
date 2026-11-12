CREATE TABLE track_days (
    id              TEXT PRIMARY KEY NOT NULL,
    title           TEXT NOT NULL,
    location        TEXT NOT NULL DEFAULT '',
    start_utc_ms    BIGINT NOT NULL,
    end_utc_ms      BIGINT NOT NULL,
    CHECK(end_utc_ms >= start_utc_ms)
);

CREATE TABLE partitions (
    partition_id     TEXT PRIMARY KEY NOT NULL,
    track_day_id     TEXT NOT NULL REFERENCES track_days(id) ON DELETE CASCADE,
    start_utc_ms     BIGINT NOT NULL,
    end_utc_ms       BIGINT NOT NULL,
    clock_confidence TEXT NOT NULL,
    superdbc_hash    TEXT NOT NULL,
    relative_path    TEXT NOT NULL,
    is_local         BOOLEAN NOT NULL DEFAULT 1,
    manifest_json    TEXT NOT NULL,
    CHECK(end_utc_ms >= start_utc_ms)
);

CREATE TABLE raw_assets (
    sha256_hex      TEXT PRIMARY KEY NOT NULL,
    track_day_id    TEXT NOT NULL REFERENCES track_days(id) ON DELETE CASCADE,
    file_name       TEXT NOT NULL,
    size_bytes      BIGINT NOT NULL,
    rtc_unix_ms     BIGINT,
    mtime_unix_ms   BIGINT NOT NULL,
    relative_path   TEXT NOT NULL DEFAULT '',
    is_local        BOOLEAN NOT NULL DEFAULT 1
);

CREATE TABLE annotations (
    id              TEXT PRIMARY KEY NOT NULL,
    track_day_id    TEXT NOT NULL REFERENCES track_days(id) ON DELETE CASCADE,
    name            TEXT NOT NULL,
    category        TEXT NOT NULL,
    color           TEXT NOT NULL,
    start_utc_ms    BIGINT NOT NULL,
    end_utc_ms      BIGINT,
    author          TEXT NOT NULL DEFAULT 'local',
    source          TEXT NOT NULL DEFAULT 'manual',
    notes           TEXT NOT NULL DEFAULT '',
    updated_at_ms   BIGINT NOT NULL,
    deleted         BOOLEAN NOT NULL DEFAULT 0,
    CHECK(end_utc_ms IS NULL OR end_utc_ms >= start_utc_ms)
);

CREATE INDEX idx_partitions_track_day ON partitions(track_day_id, start_utc_ms, end_utc_ms);
CREATE INDEX idx_partitions_time ON partitions(start_utc_ms, end_utc_ms);
CREATE INDEX idx_raw_assets_track_day ON raw_assets(track_day_id);
CREATE INDEX idx_annotations_track_day ON annotations(track_day_id, start_utc_ms, end_utc_ms);
CREATE INDEX idx_annotations_time ON annotations(start_utc_ms, end_utc_ms);
