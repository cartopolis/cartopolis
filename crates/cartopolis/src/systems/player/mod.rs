//! `systems::player` — one file per concern; see systems/mod.rs for the map.

/// The group's registration: its resources and its systems, each in its
/// `crate::phase::ClientPhase`. Added by `run_client`.
mod plugin;
pub use plugin::PlayerPlugin;

pub mod avatar;
pub mod collision;
pub mod free_camera;
pub mod gizmo;
pub mod interact;
pub mod locomotion;
pub mod picking;
pub mod prim_geo;
pub mod sit_pose;
// The soundscape's *playback* needs `bevy_audio`, which on Linux needs the ALSA
// development headers at build time — see the `audio` feature in this crate's
// Cargo.toml. The synthesis and the volume/step-rate maths are pure and compile
// either way, so they stay covered by the suite on a host that cannot build the
// backend at all.
pub mod soundscape;
pub mod touch;
