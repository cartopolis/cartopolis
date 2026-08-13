//! `systems::net` — one file per concern; see systems/mod.rs for the map.

pub mod network;
pub mod session;
pub mod user_store;
/// The browser's carrier for the same wire `network` speaks over TCP. Only the
/// web build has one, and only the web build compiles it.
#[cfg(target_arch = "wasm32")]
pub mod ws_transport;
