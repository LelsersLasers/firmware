use std::{error, path};

use daqcore::storage::Registry;

pub type Error = Box<dyn error::Error + Send + Sync>;

/// Open a file-backed SQLite metadata registry and run embedded Diesel migrations.
pub fn init_registry(database_path: impl AsRef<path::Path>) -> Result<Registry, Error> {
    Ok(Registry::open(database_path)?)
}
