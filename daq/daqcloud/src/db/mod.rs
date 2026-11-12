use std::{error, path};

use daqcore::storage::Registry;

pub type Error = Box<dyn error::Error + Send + Sync>;

/// Open a file-backed SQLite metadata registry and run embedded Diesel migrations.
pub fn init_registry(database_path: impl AsRef<path::Path>) -> Result<Registry, Error> {
    Ok(Registry::open(database_path)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use daqcore::storage::TrackDay;

    #[test]
    fn test_daqcloud_init_registry_runs_migrations() {
        let temp_path = std::env::temp_dir().join(format!(
            "daqcloud_registry_test_{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&temp_path);

        let registry = init_registry(&temp_path).expect("daqcloud registry init");
        registry
            .upsert_track_day(&TrackDay {
                id: "2026-11-12".to_owned(),
                title: "Cloud Test Day".to_owned(),
                location: "Purdue Grand Prix Track".to_owned(),
                start_utc_ms: 1_000,
                end_utc_ms: 2_000,
            })
            .unwrap();

        let days = registry.list_track_days().unwrap();
        assert_eq!(days.len(), 1);
        assert_eq!(days[0].title, "Cloud Test Day");

        drop(registry);
        let _ = std::fs::remove_file(&temp_path);
    }
}
