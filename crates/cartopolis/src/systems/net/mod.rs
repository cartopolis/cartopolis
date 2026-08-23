//! `systems::net` — one file per concern; see systems/mod.rs for the map.

/// Keeping the connection alive while the app is off screen (Android's
/// foreground service).
pub mod background;
pub mod network;
pub mod session;
pub mod user_store;
/// Proximity voice: capture, the codec, and the playback of what arrives.
pub mod voice;
/// The browser's carrier for the same wire `network` speaks over TCP. Only the
/// web build has one, and only the web build compiles it.
#[cfg(target_arch = "wasm32")]
pub mod ws_transport;
