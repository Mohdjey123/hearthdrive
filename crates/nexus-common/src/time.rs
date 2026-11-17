use std::time::{SystemTime, UNIX_EPOCH};

/// Current time as Unix epoch milliseconds.
pub fn now_ms() -> i64 {
    system_time_ms(SystemTime::now()).unwrap_or(0)
}

pub fn system_time_ms(t: SystemTime) -> Option<i64> {
    t.duration_since(UNIX_EPOCH).ok().map(|d| d.as_millis() as i64)
}
