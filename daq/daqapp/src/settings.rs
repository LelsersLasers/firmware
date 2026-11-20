use crate::{ui, workspace_io};
use std::{fs, path};

pub const SETTINGS_PATH: &str = "settings.json";
pub const DEFAULT_LOG_FOLDER: &str = "logs";
pub const DEFAULT_CLOUD_URL: &str = "http://127.0.0.1:3000";

fn default_cloud_url() -> String {
    DEFAULT_CLOUD_URL.to_owned()
}

pub fn database_dir() -> Option<path::PathBuf> {
    let path = path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../superdbc");
    path.is_dir().then_some(path)
}

const DEFAULT_UDP_PORT: u16 = 5005;
const DEFAULT_CAN_SPEED: daqnative::connection::CanBusSpeed =
    daqnative::connection::CanBusSpeed::Kbps500;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Settings {
    pub superdbc_path: Option<path::PathBuf>,
    pub bus_selection: Option<daqnative::can_thread::BusSelection>,
    pub selected_source: Option<daqnative::connection::ConnectionSource>,
    pub selected_speed: daqnative::connection::CanBusSpeed,
    pub udp_port: u16,
    pub theme: ui::theme::ThemeSelection,
    pub pixels_per_point: Option<f32>,
    pub log_folder: Option<path::PathBuf>,
    pub window_secs: f64,
    #[serde(default = "default_cloud_url")]
    pub cloud_url: String,
    #[serde(default)]
    pub cloud_write_token: String,
    #[serde(default)]
    pub cloud_auto_connect: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_view: Option<workspace_io::WorkspaceView>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            superdbc_path: None,
            bus_selection: None,
            selected_source: None,
            selected_speed: DEFAULT_CAN_SPEED,
            udp_port: DEFAULT_UDP_PORT,
            theme: ui::theme::ThemeSelection::default(),
            pixels_per_point: None,
            log_folder: None,
            window_secs: 30.0,
            cloud_url: default_cloud_url(),
            cloud_write_token: String::new(),
            cloud_auto_connect: false,
            workspace_view: None,
        }
    }
}

impl Settings {
    fn path() -> path::PathBuf {
        path::PathBuf::from(SETTINGS_PATH)
    }

    pub fn load() -> Self {
        let path = Self::path();
        if let Some(settings) = Self::load_from(&path) {
            return settings;
        }
        let default = Settings::default();
        if !path.exists() {
            default.save();
        } else {
            log::warn!(
                "Failed to parse existing {}; using default settings in memory without overwriting file",
                path.display()
            );
        }
        default
    }

    pub fn load_from(path: &path::Path) -> Option<Self> {
        let json = fs::read_to_string(path).ok()?;
        serde_json::from_str(&json).ok()
    }

    pub fn save(&self) {
        self.save_to(&Self::path());
    }

    pub fn save_to(&self, path: &path::Path) {
        let json = match serde_json::to_string_pretty(self) {
            Ok(json) => json,
            Err(error) => {
                log::error!("Failed to serialize settings: {error}");
                return;
            }
        };

        let tmp_path = path.with_extension("json.tmp");
        if let Err(error) = fs::write(&tmp_path, json) {
            log::error!("Failed to write {}: {error}", tmp_path.display());
            return;
        }
        if let Err(error) = fs::rename(&tmp_path, path) {
            log::error!(
                "Failed to rename {} to {}: {error}",
                tmp_path.display(),
                path.display()
            );
        }
    }
}
