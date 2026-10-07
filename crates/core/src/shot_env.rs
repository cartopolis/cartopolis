//! The weather and clock a scripted capture asks to be shot in.
//!
//! Lives here, rather than beside the client's shot harness that applies it,
//! because two crates build it: the client (from a preset manifest, the perf
//! suite and the command line) and the client's MCP server crate
//! (`cartopolis_mcp`, from a `view` call), which may not depend on the client.

/// The world state a viewpoint wants to be shot in — the parts of "what does it
/// look like from there" that aren't the camera. Set per shot by a preset
/// manifest (`--shots`); a plain `--shot` run leaves them `None` and the whole
/// run keeps whatever `--time` / `--fog` / `--clouds` / `--rain` seeded.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct ShotEnv {
    /// Hour of day, 0–24.
    pub time: Option<f32>,
    pub fog: Option<f32>,
    pub clouds: Option<f32>,
    pub rain: Option<f32>,
}

impl ShotEnv {
    /// Nothing to apply — the run-wide state stands.
    pub fn is_empty(&self) -> bool {
        *self == ShotEnv::default()
    }
}
