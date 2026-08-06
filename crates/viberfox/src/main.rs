//! The client binary — a shim over the `viberfox` lib target.
//!
//! Everything real lives in `lib.rs`. Keeping the crate bin-only meant
//! `cargo test` had to recompile all ~52k lines under `cfg(test)` as a separate
//! artifact, because a bin has no rlib for a test harness to link against.

fn main() {
    viberfox::run();
}
