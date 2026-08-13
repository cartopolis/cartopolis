//! Bevy-free map geodata: MVT decode and rasterisation, road classification, tree
//! scattering, and the routing graph.
//!
//! Everything here is a pure function over bytes and geometry. That is the crate's
//! whole contract — see `Cargo.toml` for why it is enforced by a crate boundary
//! rather than a convention.
//!
//! `cartopolis` re-exports these four modules from `systems`, so client code still
//! writes `systems::vector_tiles` and does not name this crate directly.

pub mod furniture;
pub mod mip;
// Allocation budgets. `cfg(test)` on purpose: the counting allocator must not
// exist in a shipped build, and nothing outside the suite reads it.
#[cfg(test)]
pub mod perf;
#[cfg(test)]
mod perf_budgets;

// Two readers of the tile's `pois` layer, kept separate on purpose: `places`
// answers "what can the avatar walk up to" (proximity prompts, house numbers,
// street furniture as colliders), `pois` answers "what stands on the map"
// (the data-layer markers that used to come from Overpass). See the note.
pub mod places;
pub mod pois;
pub mod road_texture;
pub mod routing;
pub mod scatter;
pub mod vector_tiles;

// The counting allocator that `perf::Cost::measure` reads. A `#[global_allocator]`
// is per-binary and must live at the crate root; under `cfg(test)` it is installed
// only in the test binary, so a shipped build allocates through the system
// allocator with no counters and no branch.
#[cfg(test)]
#[global_allocator]
static COUNTING_ALLOC: perf::Counting = perf::Counting;
