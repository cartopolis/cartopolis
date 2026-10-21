//! `systems::dev` — one file per concern; see systems/mod.rs for the map.

/// The group's registration: its resources and its systems, each in its
/// `crate::phase::ClientPhase`. Added by `run_client`.
mod plugin;
pub use plugin::DevPlugin;
#[cfg(mcp)]
pub use plugin::McpPlugin;
pub use plugin::{PerfPlugin, ShotHarnessPlugin};

/// The aerial photograph a capture is checked against (`--aerial-reference`).
#[cfg(not(target_arch = "wasm32"))]
pub mod aerial_reference;
/// The clock the moving world animates against; frozen between frames of a movie.
pub mod anim_clock;
pub mod camera_pacing;
pub mod desktop_integration;
pub mod dev_panel;
pub mod diagnostics;
pub mod frame_pacing;
pub mod frame_split;
#[cfg(mcp)]
pub mod mcp;
pub mod perf;
pub mod pipeline_health;
pub mod render_census;
pub mod shot_harness;
pub mod shot_presets;
/// Per-system CPU time, counted in process and read per perf window.
#[cfg(feature = "perf")]
pub mod system_timings;
pub mod upload_meter;
pub mod window_icon;
