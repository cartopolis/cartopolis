//! `systems::dev` — one file per concern; see systems/mod.rs for the map.

/// The aerial photograph a capture is checked against (`--aerial-reference`).
#[cfg(not(target_arch = "wasm32"))]
pub mod aerial_reference;
pub mod desktop_integration;
pub mod dev_panel;
pub mod diagnostics;
pub mod frame_pacing;
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
