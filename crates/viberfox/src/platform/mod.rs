//! Platform providers (ADR-030 substitutable seams).
//!
//! Where the client touches something the browser does differently from a desktop
//! OS, the mechanism lives behind a small provider here: one API the rest of the
//! client programs against, with the native impl selected by
//! `cfg(not(target_arch = "wasm32"))` and the wasm impl by
//! `cfg(target_arch = "wasm32")`. Feature code does not branch on platform.
//!
//! First seam: [`http`] — one async fetch on Bevy's `IoTaskPool`, replacing the
//! per-fetcher `std::thread::spawn` + blocking `ureq` pattern (ADR-033 Phase 1).

pub mod http;
pub mod storage;
pub mod work;
