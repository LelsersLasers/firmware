//! Bus-aware CAN encoding, decoding, and metadata from a SuperDBC artifact.
//!
//! Start with [`database::Database::load`] or [`database::Database::from_json`].
//! Lookups use a bus ID plus a [`crate::frame::CanIdentity`]. Reuse
//! borrowed [`message::Message`] definitions
//! for repeated codec operations; [`decode::DecodedMessage`] results own their
//! contents so they can outlive a database replacement.
//!
//! Physical encoding applies inverse scaling and offset. Raw encoding accepts
//! exact unscaled integers or Float32 values

pub mod database;
pub mod decode;
pub mod encode;
pub mod error;
pub mod message;

mod extract;
mod model;

use std::path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum BusSelection {
    Vehicle,
    Charger,
    Single(u8),
}

impl Default for BusSelection {
    fn default() -> Self {
        Self::Single(0)
    }
}

impl BusSelection {
    pub const VEHICLE_BUS_NAMES: [&'static str; 3] = ["VCAN", "MCAN", "SCAN"];
    pub const CHARGER_BUS_NAMES: [&'static str; 3] = ["VCAN", "CCAN", "SCAN"];

    pub fn is_multi_bus(self) -> bool {
        matches!(self, Self::Vehicle | Self::Charger)
    }

    pub fn default_single(database: &database::Database) -> Option<Self> {
        database
            .bus_by_id(0)
            .or_else(|| database.buses().first())
            .map(|bus| Self::Single(bus.bus_id()))
    }

    pub fn resolve_buses(self, database: &database::Database) -> Option<Vec<&database::Bus>> {
        match self {
            Self::Vehicle => Self::VEHICLE_BUS_NAMES
                .into_iter()
                .map(|name| database.bus(name))
                .collect(),
            Self::Charger => Self::CHARGER_BUS_NAMES
                .into_iter()
                .map(|name| database.bus(name))
                .collect(),
            Self::Single(bus_id) => database.bus_by_id(bus_id).map(|bus| vec![bus]),
        }
    }

    pub fn display_name(self, database: &database::Database) -> String {
        match self {
            Self::Vehicle => "Vehicle (VCAN, MCAN, SCAN)".to_owned(),
            Self::Charger => "Charger (VCAN, CCAN, SCAN)".to_owned(),
            Self::Single(bus_id) => match database.bus_by_id(bus_id) {
                Some(bus) => format!("{} ({})", bus.name(), bus_id),
                None => format!("Bus {bus_id}"),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseSource {
    pub path: path::PathBuf,
    pub selection: BusSelection,
}

#[derive(Debug, Clone)]
pub struct DatabaseSelection {
    pub database: database::Database,
    pub selection: BusSelection,
}

impl DatabaseSelection {
    pub fn active_buses(&self) -> Vec<&database::Bus> {
        self.selection
            .resolve_buses(&self.database)
            .unwrap_or_default()
    }

    pub fn bus_name(&self, bus_id: u8) -> &str {
        self.database
            .bus_by_id(bus_id)
            .map_or("Unknown", |bus| bus.name())
    }

    pub fn source(&self) -> Option<DatabaseSource> {
        Some(DatabaseSource {
            path: self.database.path()?.to_owned(),
            selection: self.selection,
        })
    }
}
