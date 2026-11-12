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

    /// Scan `<store_dir>/partitions/*/manifest.json` and ensure all on-disk partitions
    /// are registered in this [`Registry`]. Returns the count of newly inserted partitions.
    pub fn sync_from_store_dir(
        &self,
        store_dir: impl AsRef<path::Path>,
    ) -> Result<usize, RegistryError> {
        let partitions_dir = store_dir.as_ref().join("partitions");
        if !partitions_dir.exists() {
            return Ok(0);
        }

        let mut entries: Vec<path::PathBuf> = fs::read_dir(&partitions_dir)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
        entries.sort();

        let mut newly_inserted = 0usize;
        for dir in entries {
            let manifest_path = dir.join("manifest.json");
            if !manifest_path.is_file() {
                continue;
            }
            let bytes = fs::read(&manifest_path)?;
            let manifest: PartitionManifest = serde_json::from_slice(&bytes)?;
            if self.register_manifest(&manifest, true)? {
                newly_inserted += 1;
            }
        }

        Ok(newly_inserted)
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

    /// Insert or update a point or interval [`Annotation`].
    ///
    /// Automatically ensures the parent `track_days` record exists so live pit-wall
    /// bookmarks created before SD card import succeed with foreign-key enforcement.
    ///
    /// Precedence rules:
    /// - Incoming `"auto"` annotations never overwrite an existing annotation with the same `id`
    ///   (preserving any user edits, promotions to `"manual"`, or soft-deletions).
    /// - Incoming `"manual"` annotations apply when `updated_at_ms >= existing.updated_at_ms`.
    pub fn upsert_annotation(&self, annotation: &Annotation) -> Result<bool, RegistryError> {
        let mut conn = self.pool.get()?;
        conn.immediate_transaction(|conn| {
            let end_utc_ms = annotation
                .end_utc_ms
                .unwrap_or(annotation.start_utc_ms)
                .max(annotation.start_utc_ms);
            let day = TrackDay {
                id: annotation.track_day_id.clone(),
                title: default_track_day_title(&annotation.track_day_id),
                location: String::new(),
                start_utc_ms: annotation.start_utc_ms,
                end_utc_ms,
            };
            Self::upsert_track_day_in_conn(conn, &day)?;
            Self::upsert_annotation_in_conn(conn, annotation)
        })
        .map_err(RegistryError::from)
    }

    pub fn get_annotation(&self, annotation_id: &str) -> Result<Option<Annotation>, RegistryError> {
        let mut conn = self.pool.get()?;
        let row = annotations::table
            .filter(annotations::id.eq(annotation_id))
            .select(AnnotationRow::as_select())
            .first::<AnnotationRow>(&mut conn)
            .optional()?;
        Ok(row.map(Annotation::from))
    }

    /// Soft-delete an annotation (`deleted = true`), preserving a tombstone so re-imports
    /// do not recreate deleted auto-annotations and cloud sync can propagate the deletion.
    pub fn soft_delete_annotation(
        &self,
        annotation_id: &str,
        updated_at_ms: i64,
    ) -> Result<bool, RegistryError> {
        let mut conn = self.pool.get()?;
        let updated = diesel::update(annotations::table.filter(annotations::id.eq(annotation_id)))
            .set((
                annotations::deleted.eq(true),
                annotations::updated_at_ms.eq(updated_at_ms),
            ))
            .execute(&mut conn)?;
        Ok(updated > 0)
    }

    pub fn list_annotations_for_day(
        &self,
        track_day_id: &str,
        include_deleted: bool,
    ) -> Result<Vec<Annotation>, RegistryError> {
        let mut conn = self.pool.get()?;
        let mut query = annotations::table
            .filter(annotations::track_day_id.eq(track_day_id))
            .into_boxed();
        if !include_deleted {
            query = query.filter(annotations::deleted.eq(false));
        }
        let rows = query
            .order((annotations::start_utc_ms.asc(), annotations::id.asc()))
            .select(AnnotationRow::as_select())
            .load::<AnnotationRow>(&mut conn)?;
        Ok(rows.into_iter().map(Annotation::from).collect())
    }

    /// Query all point and interval annotations overlapping `[query_start_utc_ms, query_end_utc_ms]`.
    pub fn query_annotations_in_range(
        &self,
        query_start_utc_ms: i64,
        query_end_utc_ms: i64,
        include_deleted: bool,
    ) -> Result<Vec<Annotation>, RegistryError> {
        let mut conn = self.pool.get()?;
        let point_in_range = annotations::end_utc_ms.is_null().and(
            annotations::start_utc_ms
                .ge(query_start_utc_ms)
                .and(annotations::start_utc_ms.le(query_end_utc_ms)),
        );
        let interval_in_range = annotations::end_utc_ms.is_not_null().and(
            annotations::start_utc_ms
                .le(query_end_utc_ms)
                .and(annotations::end_utc_ms.ge(Some(query_start_utc_ms))),
        );

        let mut query = annotations::table
            .filter(point_in_range.or(interval_in_range))
            .into_boxed();
        if !include_deleted {
            query = query.filter(annotations::deleted.eq(false));
        }
        let rows = query
            .order((annotations::start_utc_ms.asc(), annotations::id.asc()))
            .select(AnnotationRow::as_select())
            .load::<AnnotationRow>(&mut conn)?;
        Ok(rows.into_iter().map(Annotation::from).collect())
    }

    /// List all annotations (including soft-deleted tombstones) modified at or after `since_utc_ms`.
    pub fn list_annotations_updated_since(
        &self,
        since_utc_ms: i64,
    ) -> Result<Vec<Annotation>, RegistryError> {
        let mut conn = self.pool.get()?;
        let rows = annotations::table
            .filter(annotations::updated_at_ms.ge(since_utc_ms))
            .order((annotations::updated_at_ms.asc(), annotations::id.asc()))
            .select(AnnotationRow::as_select())
            .load::<AnnotationRow>(&mut conn)?;
        Ok(rows.into_iter().map(Annotation::from).collect())
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
                // Auto-annotations are deterministic; never overwrite an existing row on re-import
                // so manual edits, promotions to "manual", and soft-deletes are always preserved.
                if annotation.source == "auto" {
                    return Ok(false);
                }
                if annotation.updated_at_ms < prev.updated_at_ms {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{log_parse::parse::SourceLogFile, storage::SignalManifestEntry};

    fn sample_manifest(
        partition_id: &str,
        track_day_id: &str,
        start_utc_ms: i64,
        end_utc_ms: i64,
        raw_hash: &str,
        annotations: Vec<Annotation>,
    ) -> PartitionManifest {
        PartitionManifest {
            partition_id: partition_id.to_owned(),
            track_day_id: track_day_id.to_owned(),
            start_utc_ms,
            end_utc_ms,
            clock_mapping: correlate::ClockMapping {
                slope: 1.0,
                intercept_ms: start_utc_ms as f64,
                confidence: correlate::ClockConfidence::RtcEstimated,
            },
            superdbc_hash: "deadbeef".to_owned(),
            raw_log_hashes: vec![SourceLogFile {
                file_name: format!("log-{partition_id}.log"),
                sha256_hex: raw_hash.to_owned(),
                rtc_timestamp_ms: Some(start_utc_ms),
                mtime_unix_ms: start_utc_ms,
            }],
            signals: vec![SignalManifestEntry {
                signal_id: "imu.accel_y_g".to_owned(),
                unit: "g".to_owned(),
                is_discrete: false,
                sample_count: 100,
                min_utc_ms: start_utc_ms,
                max_utc_ms: end_utc_ms,
                min_val: -1.5,
                max_val: 1.5,
            }],
            annotations,
        }
    }

    #[test]
    fn test_register_multiple_partitions_idempotency_and_range_queries() {
        let registry = Registry::open_in_memory().expect("in-memory registry");

        // Customize track day metadata upfront
        registry
            .upsert_track_day(&TrackDay {
                id: "2026-11-12".to_owned(),
                title: "Putnam Park Test Day".to_owned(),
                location: "Putnam Park Road Course".to_owned(),
                start_utc_ms: 150_000,
                end_utc_ms: 180_000,
            })
            .unwrap();

        let m1 = sample_manifest(
            "epoch_100000_aaa",
            "2026-11-12",
            100_000,
            200_000,
            "hash_a",
            vec![],
        );
        let m2 = sample_manifest(
            "epoch_300000_bbb",
            "2026-11-12",
            300_000,
            450_000,
            "hash_b",
            vec![],
        );
        let m3 = sample_manifest(
            "epoch_900000_ccc",
            "2026-11-13",
            900_000,
            950_000,
            "hash_c",
            vec![],
        );

        assert!(registry.register_manifest(&m1, true).unwrap());
        assert!(registry.register_manifest(&m2, false).unwrap());
        assert!(registry.register_manifest(&m3, true).unwrap());

        // Re-registering m1 must be idempotent (returns false)
        assert!(!registry.register_manifest(&m1, true).unwrap());
        assert!(registry.has_partition("epoch_100000_aaa").unwrap());
        assert!(!registry.has_partition("epoch_nonexistent").unwrap());

        // Verify track day bounds widened to [100_000 .. 450_000] while preserving custom title/location
        let day = registry
            .get_track_day("2026-11-12")
            .unwrap()
            .expect("track day exists");
        assert_eq!(day.title, "Putnam Park Test Day");
        assert_eq!(day.location, "Putnam Park Road Course");
        assert_eq!(day.start_utc_ms, 100_000);
        assert_eq!(day.end_utc_ms, 450_000);

        let all_days = registry.list_track_days().unwrap();
        assert_eq!(all_days.len(), 2);

        // Verify partitions per day & raw assets
        let day_parts = registry.list_partitions_for_day("2026-11-12").unwrap();
        assert_eq!(day_parts.len(), 2);
        assert_eq!(day_parts[0].partition_id, "epoch_100000_aaa");
        assert!(day_parts[0].is_local);
        assert_eq!(day_parts[1].partition_id, "epoch_300000_bbb");
        assert!(!day_parts[1].is_local);

        registry
            .set_partition_local("epoch_300000_bbb", true)
            .unwrap();
        assert!(
            registry
                .get_partition("epoch_300000_bbb")
                .unwrap()
                .unwrap()
                .is_local
        );

        let raw_assets = registry.list_raw_assets_for_day("2026-11-12").unwrap();
        assert_eq!(raw_assets.len(), 2);
        assert_eq!(raw_assets[0].sha256_hex, "hash_a");
        assert_eq!(raw_assets[1].sha256_hex, "hash_b");

        // Verify overlapping time-range queries for [t0, t1]
        let overlap_m1 = registry
            .query_partitions_in_range(150_000, 250_000)
            .unwrap();
        assert_eq!(overlap_m1.len(), 1);
        assert_eq!(overlap_m1[0].partition_id, "epoch_100000_aaa");

        let overlap_both = registry
            .query_partitions_in_range(190_000, 310_000)
            .unwrap();
        assert_eq!(overlap_both.len(), 2);

        let overlap_gap = registry
            .query_partitions_in_range(210_000, 290_000)
            .unwrap();
        assert!(overlap_gap.is_empty());
    }

    #[test]
    fn test_annotations_crud_soft_delete_and_auto_vs_manual_precedence() {
        let registry = Registry::open_in_memory().expect("in-memory registry");

        // 1. Live pit-wall bookmark created BEFORE any SD partition is imported for 2026-11-12
        let pit_bookmark = Annotation {
            id: "manual_bookmark_1".to_owned(),
            track_day_id: "2026-11-12".to_owned(),
            name: "Weird inverter derate here".to_owned(),
            category: "note".to_owned(),
            color: "#eab308".to_owned(),
            start_utc_ms: 125_000,
            end_utc_ms: None,
            author: "pitwall".to_owned(),
            source: "manual".to_owned(),
            notes: "Driver reported brief torque cut".to_owned(),
            updated_at_ms: 100,
            deleted: false,
        };
        assert!(registry.upsert_annotation(&pit_bookmark).unwrap());

        // 2. Import partition containing two auto-annotations
        let auto_stint = Annotation {
            id: "auto_stint_p1_110000".to_owned(),
            track_day_id: "2026-11-12".to_owned(),
            name: "Driving Stint #1".to_owned(),
            category: "driving_stint".to_owned(),
            color: "#22c55e".to_owned(),
            start_utc_ms: 110_000,
            end_utc_ms: Some(190_000),
            author: "daqcore".to_owned(),
            source: "auto".to_owned(),
            notes: "Auto-detected motion stint".to_owned(),
            updated_at_ms: 200,
            deleted: false,
        };
        let auto_fault = Annotation {
            id: "auto_fault_p1_140000".to_owned(),
            track_day_id: "2026-11-12".to_owned(),
            name: "Fault: imd_fault".to_owned(),
            category: "fault".to_owned(),
            color: "#ef4444".to_owned(),
            start_utc_ms: 140_000,
            end_utc_ms: Some(145_000),
            author: "daqcore".to_owned(),
            source: "auto".to_owned(),
            notes: "Auto-detected fault".to_owned(),
            updated_at_ms: 200,
            deleted: false,
        };
        let manifest = sample_manifest(
            "epoch_100000_p1",
            "2026-11-12",
            100_000,
            200_000,
            "hash_p1",
            vec![auto_stint.clone(), auto_fault.clone()],
        );
        registry.register_manifest(&manifest, true).unwrap();

        let active = registry
            .list_annotations_for_day("2026-11-12", false)
            .unwrap();
        assert_eq!(active.len(), 3);

        // 3. Engineer renames/promotes auto_stint to a manual event, and soft-deletes auto_fault
        let mut promoted_stint = auto_stint.clone();
        promoted_stint.name = "Skidpad Run 1 (Wet)".to_owned();
        promoted_stint.category = "event".to_owned();
        promoted_stint.source = "manual".to_owned();
        promoted_stint.author = "vd_lead".to_owned();
        promoted_stint.updated_at_ms = 500;
        assert!(registry.upsert_annotation(&promoted_stint).unwrap());

        assert!(
            registry
                .soft_delete_annotation(&auto_fault.id, 600)
                .unwrap()
        );

        // 4. Re-import the partition with newer auto timestamps (updated_at_ms = 9999)
        // Verify that `auto` annotations do NOT overwrite `manual` edits or resurrect soft-deleted annotations!
        let mut reimport_stint = auto_stint.clone();
        reimport_stint.updated_at_ms = 9_999;
        let mut reimport_fault = auto_fault.clone();
        reimport_fault.updated_at_ms = 9_999;
        let manifest_reimport = sample_manifest(
            "epoch_100000_p1",
            "2026-11-12",
            100_000,
            200_000,
            "hash_p1",
            vec![reimport_stint, reimport_fault],
        );
        registry
            .register_manifest(&manifest_reimport, true)
            .unwrap();

        let fetched_stint = registry
            .get_annotation(&auto_stint.id)
            .unwrap()
            .expect("stint exists");
        assert_eq!(fetched_stint.name, "Skidpad Run 1 (Wet)");
        assert_eq!(fetched_stint.source, "manual");
        assert_eq!(fetched_stint.updated_at_ms, 500);

        let fetched_fault = registry
            .get_annotation(&auto_fault.id)
            .unwrap()
            .expect("fault tombstone exists");
        assert!(fetched_fault.deleted);

        // 5. Query range [120_000 .. 130_000]: should return promoted_stint (interval) + pit_bookmark (point), excluding soft-deleted fault
        let in_window = registry
            .query_annotations_in_range(120_000, 130_000, false)
            .unwrap();
        assert_eq!(in_window.len(), 2);
        assert_eq!(in_window[0].id, "auto_stint_p1_110000");
        assert_eq!(in_window[1].id, "manual_bookmark_1");

        // 6. Delta query for cloud sync (`list_annotations_updated_since(500)`)
        let delta = registry.list_annotations_updated_since(500).unwrap();
        assert_eq!(delta.len(), 2);
        assert_eq!(delta[0].id, "auto_stint_p1_110000");
        assert_eq!(delta[1].id, "auto_fault_p1_140000");
        assert!(delta[1].deleted);
    }
}
