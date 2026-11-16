//! Portable CAN bus load reading types shared across worker threads and UI viewers.

pub const WINDOWS_SECS: [u64; 4] = [1, 5, 10, 30];

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BusLoadReading {
    pub bus_id: u8,
    pub bus_name: String,
    pub values: [f32; 4],
}
