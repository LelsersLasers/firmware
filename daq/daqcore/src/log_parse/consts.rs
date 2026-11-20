// identity bit masks
pub const MAX_JUMP_MS: u32 = 300_000; // 300 seconds
pub const BUS_ID_MASK: u32 = 0xE0000000; // BUS ID is in the 3 highest bits of the identity
pub const BUS_ID_OFFSET: u8 = 29; // Lowest bit of BUS ID is 29 bits after the first

// SD card file-rotation constants
pub const RTC_FILE_ROTATION_MAX_GAP_MS: i64 = 90_000; // 1.5x the 60s SD_NEW_FILE_PERIOD_MS
pub const CONTINUATION_MAX_GAP_MS: u32 = 5_000; // Max tick delta when rotating from log-0.log to an RTC file

// Partition & overview pyramid constants
pub const MAX_PARTITION_SPAN_MS: i64 = 15 * 60 * 1_000; // 15 minutes
pub const BUCKET_10HZ_MS: i64 = 100; // 100 ms buckets (10 Hz)
pub const BUCKET_1HZ_MS: i64 = 1_000; // 1,000 ms buckets (1 Hz)
