use crate::{
    log_parse::{auto_annotate, correlate},
    storage::partition_writer::PartitionManifest,
};
use diesel::connection::SimpleConnection as _;
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
    pub size_bytes: u64,
    pub rtc_unix_ms: Option<i64>,
    pub mtime_unix_ms: i64,
    pub relative_path: String,
    pub is_local: bool,
}

pub use auto_annotate::Annotation;

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
}
