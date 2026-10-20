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
// Not on the web: a call there uses the browser's camera (`web_call`).
#[cfg(not(target_arch = "wasm32"))]
pub mod camera;
pub mod cell_source;
pub mod fetch_gate;
#[cfg(not(target_arch = "wasm32"))]
pub mod gallery;
// The raw GNSS measurements under the location fix, for the high-precision
// mode. Compiled everywhere so its holders need no `cfg`; Android only.
pub mod gnss_raw;
// An external receiver on a cable — an RTK rover — asked before `location`'s
// platform provider. Compiled everywhere; inert on the web.
pub mod gnss_receiver;
pub mod heap;
pub mod http;
// A call's video encoded by the phone's own codecs (`MediaCodec`).
#[cfg(target_os = "android")]
pub mod hw_video;
pub mod location;
pub mod model_file;
pub mod orientation;
pub mod photo;
// Compiled everywhere so its holders need no `cfg`; it captures on Android only.
pub mod precise_sensors;
// The screen a call shares; not on the web, where the browser shares it.
#[cfg(not(target_arch = "wasm32"))]
pub mod screen;
pub mod storage;
pub mod stream;
pub mod sync;
pub mod warm;
pub mod watchdog;
#[cfg(target_arch = "wasm32")]
pub mod web_call;
pub mod work;
pub mod workers;
