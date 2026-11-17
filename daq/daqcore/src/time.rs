//! Wall-clock timestamps; arithmetic is integer milliseconds, labels only use chrono.

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Time(i64);

impl Time {
    pub fn now() -> Self {
        Self(chrono::Utc::now().timestamp_millis())
    }

    pub fn from_unix_millis(ms: i64) -> Self {
        Self(ms)
    }

    pub fn unix_millis(self) -> i64 {
        self.0
    }

    pub fn offset(self, ms: i64) -> Self {
        Self(self.0.saturating_add(ms))
    }

    pub fn secs(self, other: Self) -> f64 {
        (self.0 as i128 - other.0 as i128) as f64 / 1000.0
    }

    pub fn label(self) -> String {
        chrono::DateTime::from_timestamp_millis(self.0)
            .map(|t| {
                t.with_timezone(&chrono::Local)
                    .format("%H:%M:%S%.3f")
                    .to_string()
            })
            .unwrap_or_else(|| self.0.to_string())
    }
}
