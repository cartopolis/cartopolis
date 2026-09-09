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
//!
//! [`android`] is the exception to the shape above and is named after its platform
//! on purpose: it is not one seam with two implementations but the *phone half* of
//! several — the device location provider, keep-screen-on, `open_url` — collected
//! because every one of them is a JNI call into the Android framework and they
//! share the plumbing (the JVM handle, the activity, local-reference discipline,
//! exception clearing). Callers still program against the seam, not against this:
//! `location::LocationWatch` is what `systems::driver` uses on every platform.
//!
//! [`orientation`] is the compass beside [`location`]'s position, and is the one
//! seam whose Android arm is *not* in [`android`] — it is the NDK's C sensor API
//! rather than JNI, for the reason its own module note gives.

/// Android platform services. Gated on the target because every line of it is JNI
/// against the framework — see the module's own note for the no-dex constraint that
/// shaped it.
#[cfg(target_os = "android")]
pub mod android;
pub mod audio;
pub mod cell_source;
pub mod fetch_gate;
pub mod http;
pub mod location;
pub mod model_file;
pub mod orientation;
pub mod photo;
pub mod storage;
pub mod stream;
pub mod watchdog;
pub mod work;
pub mod workers;
