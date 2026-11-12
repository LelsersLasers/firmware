use crate::{
    log_parse::{auto_annotate, correlate},
    storage::{
        partition_writer::PartitionManifest,
        schema::{annotations, partitions, raw_assets, track_days},
    },
};
use diesel::connection::SimpleConnection as _;
use diesel::prelude::*;
use diesel::r2d2::{self, ConnectionManager, CustomizeConnection, Pool};
use diesel::sqlite::SqliteConnection;
use diesel_migrations::{EmbeddedMigrations, MigrationHarness as _, embed_migrations};
use std::{fmt, fs, io, path, time};

pub const LOCAL_REGISTRY_FILENAME: &str = "local_registry.sqlite";

pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");

const DEFAULT_POOL_SIZE: u32 = 4;
const CONNECTION_TIMEOUT: time::Duration = time::Duration::from_secs(5);

pub type DbPool = Pool<ConnectionManager<SqliteConnection>>;

#[derive(Debug)]
pub enum RegistryError {
    Diesel(diesel::result::Error),
    Pool(r2d2::PoolError),
    Migration(Box<dyn std::error::Error + Send + Sync>),
    Io(io::Error),
    Json(serde_json::Error),
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Diesel(e) => write!(f, "SQLite query error: {e}"),
            Self::Pool(e) => write!(f, "SQLite connection pool error: {e}"),
            Self::Migration(e) => write!(f, "SQLite migration error: {e}"),
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Json(e) => write!(f, "JSON serialization error: {e}"),
        }
    }
}

impl std::error::Error for RegistryError {}

impl From<diesel::result::Error> for RegistryError {
    fn from(value: diesel::result::Error) -> Self {
        Self::Diesel(value)
    }
}

impl From<r2d2::PoolError> for RegistryError {
    fn from(value: r2d2::PoolError) -> Self {
        Self::Pool(value)
    }
}

impl From<io::Error> for RegistryError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for RegistryError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TrackDay {
    pub id: String,
    pub title: String,
    pub location: String,
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RegisteredPartition {
    pub partition_id: String,
    pub track_day_id: String,
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    pub clock_mapping: correlate::ClockMapping,
    pub superdbc_hash: String,
    pub relative_path: String,
    pub is_local: bool,
    pub manifest: PartitionManifest,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RawAssetRecord {
    pub sha256_hex: String,
    pub track_day_id: String,
    pub file_name: String,
    pub rtc_timestamp_ms: Option<i64>,
    pub mtime_unix_ms: i64,
    pub relative_path: String,
    pub is_local: bool,
}

pub use auto_annotate::Annotation;

#[derive(Debug, Clone, Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = track_days)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct TrackDayRow {
    id: String,
    title: String,
    location: String,
    start_utc_ms: i64,
    end_utc_ms: i64,
}

impl From<TrackDayRow> for TrackDay {
    fn from(row: TrackDayRow) -> Self {
        Self {
            id: row.id,
            title: row.title,
            location: row.location,
            start_utc_ms: row.start_utc_ms,
            end_utc_ms: row.end_utc_ms,
        }
    }
}

#[derive(Debug, Clone, Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = partitions)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct PartitionRow {
    partition_id: String,
    track_day_id: String,
    start_utc_ms: i64,
    end_utc_ms: i64,
    clock_confidence: String,
    superdbc_hash: String,
    relative_path: String,
    is_local: bool,
    manifest_json: String,
}

impl PartitionRow {
    fn into_registered(self) -> Result<RegisteredPartition, RegistryError> {
        let clock_mapping: correlate::ClockMapping = serde_json::from_str(&self.clock_confidence)?;
        let manifest: PartitionManifest = serde_json::from_str(&self.manifest_json)?;
        Ok(RegisteredPartition {
            partition_id: self.partition_id,
            track_day_id: self.track_day_id,
            start_utc_ms: self.start_utc_ms,
            end_utc_ms: self.end_utc_ms,
            clock_mapping,
            superdbc_hash: self.superdbc_hash,
            relative_path: self.relative_path,
            is_local: self.is_local,
            manifest,
        })
    }
}

#[derive(Debug, Clone, Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = raw_assets)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct RawAssetRow {
    sha256_hex: String,
    track_day_id: String,
    file_name: String,
    rtc_timestamp_ms: Option<i64>,
    mtime_unix_ms: i64,
    relative_path: String,
    is_local: bool,
}

impl From<RawAssetRow> for RawAssetRecord {
    fn from(row: RawAssetRow) -> Self {
        Self {
            sha256_hex: row.sha256_hex,
            track_day_id: row.track_day_id,
            file_name: row.file_name,
            rtc_timestamp_ms: row.rtc_timestamp_ms,
            mtime_unix_ms: row.mtime_unix_ms,
            relative_path: row.relative_path,
            is_local: row.is_local,
        }
    }
}

#[derive(Debug, Clone, Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = annotations)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct AnnotationRow {
    id: String,
    track_day_id: String,
    name: String,
    category: String,
    color: String,
    start_utc_ms: i64,
    end_utc_ms: Option<i64>,
    author: String,
    source: String,
    notes: String,
    updated_at_ms: i64,
    deleted: bool,
}

impl From<AnnotationRow> for Annotation {
    fn from(row: AnnotationRow) -> Self {
        Self {
            id: row.id,
            track_day_id: row.track_day_id,
            name: row.name,
            category: row.category,
            color: row.color,
            start_utc_ms: row.start_utc_ms,
            end_utc_ms: row.end_utc_ms,
            author: row.author,
            source: row.source,
            notes: row.notes,
            updated_at_ms: row.updated_at_ms,
            deleted: row.deleted,
        }
    }
}

impl From<&Annotation> for AnnotationRow {
    fn from(ann: &Annotation) -> Self {
        Self {
            id: ann.id.clone(),
            track_day_id: ann.track_day_id.clone(),
            name: ann.name.clone(),
            category: ann.category.clone(),
            color: ann.color.clone(),
            start_utc_ms: ann.start_utc_ms,
            end_utc_ms: ann.end_utc_ms,
            author: ann.author.clone(),
            source: ann.source.clone(),
            notes: ann.notes.clone(),
            updated_at_ms: ann.updated_at_ms,
            deleted: ann.deleted,
        }
    }
}

#[derive(Debug)]
struct SqliteConnectionCustomizer;

impl CustomizeConnection<SqliteConnection, r2d2::Error> for SqliteConnectionCustomizer {
    fn on_acquire(&self, conn: &mut SqliteConnection) -> Result<(), r2d2::Error> {
        conn.batch_execute(
            "PRAGMA busy_timeout = 5000; PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON;",
        )
        .map_err(r2d2::Error::QueryError)
    }
}

fn default_track_day_title(track_day_id: &str) -> String {
    format!("Track Day {track_day_id}")
}

/// Shared Diesel + SQLite metadata, manifest, and annotation registry used identically
/// by local storage (`local_registry.sqlite`), USB drives, and `daqcloud`.
#[derive(Clone)]
pub struct Registry {
    pool: DbPool,
}

impl Registry {
    /// Open (or create) a SQLite registry database at `db_path` and run embedded Diesel migrations.
    pub fn open(db_path: impl AsRef<path::Path>) -> Result<Self, RegistryError> {
        let db_path = db_path.as_ref();
        if let Some(parent) = db_path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }
        Self::open_url(&db_path.to_string_lossy(), DEFAULT_POOL_SIZE, true)
    }

    /// Open (or create) `<store_dir>/local_registry.sqlite`.
    pub fn open_store(store_dir: impl AsRef<path::Path>) -> Result<Self, RegistryError> {
        let store_dir = store_dir.as_ref();
        fs::create_dir_all(store_dir)?;
        Self::open(store_dir.join(LOCAL_REGISTRY_FILENAME))
    }

    /// Open an isolated in-memory SQLite registry (single connection pool so `:memory:` state persists).
    pub fn open_in_memory() -> Result<Self, RegistryError> {
        Self::open_url(":memory:", 1, false)
    }

    fn open_url(
        database_url: &str,
        pool_size: u32,
        enable_wal: bool,
    ) -> Result<Self, RegistryError> {
        let manager = ConnectionManager::<SqliteConnection>::new(database_url);
        let pool = Pool::builder()
            .max_size(pool_size)
            .connection_timeout(CONNECTION_TIMEOUT)
            .connection_customizer(Box::new(SqliteConnectionCustomizer))
            .build(manager)?;

        {
            let mut conn = pool.get()?;
            if enable_wal {
                conn.batch_execute("PRAGMA journal_mode = WAL;")?;
            }
            conn.run_pending_migrations(MIGRATIONS)
                .map_err(RegistryError::Migration)?;
        }

        Ok(Self { pool })
    }

    pub fn pool(&self) -> &DbPool {
        &self.pool
    }

    /// Insert a new track day or widen the `[start_utc_ms, end_utc_ms]` bounds of an existing one.
    pub fn upsert_track_day(&self, day: &TrackDay) -> Result<TrackDay, RegistryError> {
        let mut conn = self.pool.get()?;
        conn.immediate_transaction(|conn| Self::upsert_track_day_in_conn(conn, day))
            .map_err(RegistryError::from)
    }

    fn upsert_track_day_in_conn(
        conn: &mut SqliteConnection,
        day: &TrackDay,
    ) -> Result<TrackDay, diesel::result::Error> {
        let existing = track_days::table
            .filter(track_days::id.eq(&day.id))
            .select(TrackDayRow::as_select())
            .first::<TrackDayRow>(conn)
            .optional()?;

        let default_title = default_track_day_title(&day.id);
        let row = match existing {
            Some(prev) => {
                let title = if day.title.is_empty()
                    || (day.title == default_title && prev.title != default_title)
                {
                    prev.title
                } else {
                    day.title.clone()
                };
                let location = if day.location.is_empty() {
                    prev.location
                } else {
                    day.location.clone()
                };
                let start_utc_ms = prev.start_utc_ms.min(day.start_utc_ms);
                let end_utc_ms = prev.end_utc_ms.max(day.end_utc_ms).max(start_utc_ms);

                let updated = TrackDayRow {
                    id: day.id.clone(),
                    title,
                    location,
                    start_utc_ms,
                    end_utc_ms,
                };
                diesel::update(track_days::table.filter(track_days::id.eq(&day.id)))
                    .set(&updated)
                    .execute(conn)?;
                updated
            }
            None => {
                let title = if day.title.is_empty() {
                    default_title
                } else {
                    day.title.clone()
                };
                let start_utc_ms = day.start_utc_ms;
                let end_utc_ms = day.end_utc_ms.max(start_utc_ms);
                let inserted = TrackDayRow {
                    id: day.id.clone(),
                    title,
                    location: day.location.clone(),
                    start_utc_ms,
                    end_utc_ms,
                };
                diesel::insert_into(track_days::table)
                    .values(&inserted)
                    .execute(conn)?;
                inserted
            }
        };

        Ok(TrackDay::from(row))
    }

    pub fn get_track_day(&self, track_day_id: &str) -> Result<Option<TrackDay>, RegistryError> {
        let mut conn = self.pool.get()?;
        let row = track_days::table
            .filter(track_days::id.eq(track_day_id))
            .select(TrackDayRow::as_select())
            .first::<TrackDayRow>(&mut conn)
            .optional()?;
        Ok(row.map(TrackDay::from))
    }

    pub fn list_track_days(&self) -> Result<Vec<TrackDay>, RegistryError> {
        let mut conn = self.pool.get()?;
        let rows = track_days::table
            .order((track_days::start_utc_ms.asc(), track_days::id.asc()))
            .select(TrackDayRow::as_select())
            .load::<TrackDayRow>(&mut conn)?;
        Ok(rows.into_iter().map(TrackDay::from).collect())
    }

    pub fn has_partition(&self, partition_id: &str) -> Result<bool, RegistryError> {
        let mut conn = self.pool.get()?;
        let found = partitions::table
            .filter(partitions::partition_id.eq(partition_id))
            .select(partitions::partition_id)
            .first::<String>(&mut conn)
            .optional()?;
        Ok(found.is_some())
    }

    pub fn get_partition(
        &self,
        partition_id: &str,
    ) -> Result<Option<RegisteredPartition>, RegistryError> {
        let mut conn = self.pool.get()?;
        let row = partitions::table
            .filter(partitions::partition_id.eq(partition_id))
            .select(PartitionRow::as_select())
            .first::<PartitionRow>(&mut conn)
            .optional()?;
        row.map(PartitionRow::into_registered).transpose()
    }

    /// Idempotently register a [`PartitionManifest`] (plus its parent `track_days` bounds,
    /// `raw_assets`, and `annotations`) into the SQLite registry.
    ///
    /// Returns `Ok(true)` if the partition was newly inserted, or `Ok(false)` if `partition_id`
    /// was already present in the registry.
    pub fn register_manifest(
        &self,
        manifest: &PartitionManifest,
        is_local: bool,
    ) -> Result<bool, RegistryError> {
        let clock_confidence = serde_json::to_string(&manifest.clock_mapping)?;
        let manifest_json = serde_json::to_string(manifest)?;
        let relative_path = format!("partitions/{}/", manifest.partition_id);
        let end_utc_ms = manifest.end_utc_ms.max(manifest.start_utc_ms);

        let mut conn = self.pool.get()?;
        conn.immediate_transaction(|conn| {
            let day = TrackDay {
                id: manifest.track_day_id.clone(),
                title: default_track_day_title(&manifest.track_day_id),
                location: String::new(),
                start_utc_ms: manifest.start_utc_ms,
                end_utc_ms,
            };
            Self::upsert_track_day_in_conn(conn, &day)?;

            let existing_partition = partitions::table
                .filter(partitions::partition_id.eq(&manifest.partition_id))
                .select(PartitionRow::as_select())
                .first::<PartitionRow>(conn)
                .optional()?;

            let was_newly_inserted = match existing_partition {
                Some(prev) => {
                    let updated = PartitionRow {
                        partition_id: manifest.partition_id.clone(),
                        track_day_id: manifest.track_day_id.clone(),
                        start_utc_ms: manifest.start_utc_ms,
                        end_utc_ms,
                        clock_confidence,
                        superdbc_hash: manifest.superdbc_hash.clone(),
                        relative_path,
                        is_local: prev.is_local || is_local,
                        manifest_json,
                    };
                    diesel::update(
                        partitions::table
                            .filter(partitions::partition_id.eq(&manifest.partition_id)),
                    )
                    .set(&updated)
                    .execute(conn)?;
                    false
                }
                None => {
                    let row = PartitionRow {
                        partition_id: manifest.partition_id.clone(),
                        track_day_id: manifest.track_day_id.clone(),
                        start_utc_ms: manifest.start_utc_ms,
                        end_utc_ms,
                        clock_confidence,
                        superdbc_hash: manifest.superdbc_hash.clone(),
                        relative_path,
                        is_local,
                        manifest_json,
                    };
                    diesel::insert_into(partitions::table)
                        .values(&row)
                        .execute(conn)?;
                    true
                }
            };

            for raw_file in &manifest.raw_log_hashes {
                let raw_row = RawAssetRow {
                    sha256_hex: raw_file.sha256_hex.clone(),
                    track_day_id: manifest.track_day_id.clone(),
                    file_name: raw_file.file_name.clone(),
                    rtc_timestamp_ms: raw_file.rtc_timestamp_ms,
                    mtime_unix_ms: raw_file.mtime_unix_ms,
                    relative_path: format!("raw/{}", raw_file.file_name),
                    is_local,
                };
                diesel::insert_into(raw_assets::table)
                    .values(&raw_row)
                    .on_conflict(raw_assets::sha256_hex)
                    .do_update()
                    .set(&raw_row)
                    .execute(conn)?;
            }

            for annotation in &manifest.annotations {
                Self::upsert_annotation_in_conn(conn, annotation)?;
            }

            Ok(was_newly_inserted)
        })
    }

    pub fn list_partitions_for_day(
        &self,
        track_day_id: &str,
    ) -> Result<Vec<RegisteredPartition>, RegistryError> {
        let mut conn = self.pool.get()?;
        let rows = partitions::table
            .filter(partitions::track_day_id.eq(track_day_id))
            .order((
                partitions::start_utc_ms.asc(),
                partitions::partition_id.asc(),
            ))
            .select(PartitionRow::as_select())
            .load::<PartitionRow>(&mut conn)?;
        rows.into_iter()
            .map(PartitionRow::into_registered)
            .collect()
    }

    /// Query all registered partitions whose `[start_utc_ms, end_utc_ms]` interval overlaps
    /// `[query_start_utc_ms, query_end_utc_ms]`, ordered by `start_utc_ms ASC, partition_id ASC`.
    pub fn query_partitions_in_range(
        &self,
        query_start_utc_ms: i64,
        query_end_utc_ms: i64,
    ) -> Result<Vec<RegisteredPartition>, RegistryError> {
        let mut conn = self.pool.get()?;
        let rows = partitions::table
            .filter(
                partitions::start_utc_ms
                    .le(query_end_utc_ms)
                    .and(partitions::end_utc_ms.ge(query_start_utc_ms)),
            )
            .order((
                partitions::start_utc_ms.asc(),
                partitions::partition_id.asc(),
            ))
            .select(PartitionRow::as_select())
            .load::<PartitionRow>(&mut conn)?;
        rows.into_iter()
            .map(PartitionRow::into_registered)
            .collect()
    }

    pub fn set_partition_local(
        &self,
        partition_id: &str,
        is_local: bool,
    ) -> Result<bool, RegistryError> {
        let mut conn = self.pool.get()?;
        let updated =
            diesel::update(partitions::table.filter(partitions::partition_id.eq(partition_id)))
                .set(partitions::is_local.eq(is_local))
                .execute(&mut conn)?;
        Ok(updated > 0)
    }

    pub fn list_raw_assets_for_day(
        &self,
        track_day_id: &str,
    ) -> Result<Vec<RawAssetRecord>, RegistryError> {
        let mut conn = self.pool.get()?;
        let rows = raw_assets::table
            .filter(raw_assets::track_day_id.eq(track_day_id))
            .order((raw_assets::mtime_unix_ms.asc(), raw_assets::file_name.asc()))
            .select(RawAssetRow::as_select())
            .load::<RawAssetRow>(&mut conn)?;
        Ok(rows.into_iter().map(RawAssetRecord::from).collect())
    }

    fn upsert_annotation_in_conn(
        conn: &mut SqliteConnection,
        annotation: &Annotation,
    ) -> Result<bool, diesel::result::Error> {
        let existing = annotations::table
            .filter(annotations::id.eq(&annotation.id))
            .select(AnnotationRow::as_select())
            .first::<AnnotationRow>(conn)
            .optional()?;

        let row = AnnotationRow::from(annotation);
        match existing {
            None => {
                diesel::insert_into(annotations::table)
                    .values(&row)
                    .execute(conn)?;
                Ok(true)
            }
            Some(prev) => {
                // Auto-annotations must never overwrite manual annotations or resurrect soft-deleted annotations
                if annotation.source == "auto"
                    && (prev.source != "auto"
                        || prev.deleted
                        || prev.name != annotation.name
                        || prev.notes != annotation.notes)
                {
                    return Ok(false);
                }
                if annotation.source != "auto" && annotation.updated_at_ms < prev.updated_at_ms {
                    return Ok(false);
                }
                diesel::update(annotations::table.filter(annotations::id.eq(&annotation.id)))
                    .set(&row)
                    .execute(conn)?;
                Ok(true)
            }
        }
    }
}
