//! Wall-clock time, on every target.

/// Milliseconds since the Unix epoch. `web_time`, not `std::time`: on wasm
/// `std::time::SystemTime::now` panics. A clock before 1970 reads as 0.
#[must_use]
pub fn unix_ms() -> u64 {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
