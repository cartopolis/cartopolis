//! The ocean at globe scale (ADR-025 tier, the counterpart to `globe_weather`).
//!
//! `systems::water` gives the *local* tier real water: a wave field, a shoreline,
//! shallows. None of that transfers to orbit — from 3,000 km a wave is many
//! orders of magnitude below a pixel, and a normal map at that scale is wasted
//! bandwidth. What actually distinguishes an ocean from a continent in every
//! photograph ever taken from space is far simpler and entirely a **BRDF**
//! difference: the sea is dark and glossy, the land is bright and matte, and the
//! sun draws a broad glint across the water somewhere near the sub-solar point.
//!
//! So this is not a wave shader. It is one number per texel — "is this water?" —
//! driving roughness, specular and tint on the globe's own tile materials. The
//! mask rides in the **alpha channel of the tile imagery** the globe already
//! uploads (`tile_loader::load_globe_tile_rgba`), so it costs no extra texture,
//! binding or upload; the shader reads it back out of the sampled base colour
//! before `alpha_discard` flattens it.
//!
//! Everything ramps on `GlobeState::space_factor`, like the rest of the tier
//! (see the CLAUDE.md note): the flat map tiles just below the boundary have no
//! gloss at all, so switching it on at the threshold would put a hard cut in the
//! middle of a climb — the exact failure the albedo ramp exists to avoid.

use bevy::prelude::*;

// The globe's surface material and its ocean uniforms live in
// `cartopolis_materials`, so that Bevy's per-material plugin code is generated
// there rather than in this crate.
pub use cartopolis_materials::globe_ocean::{GlobeOcean, GlobeSurfaceMaterial};

/// Current ocean strength, so a tile streaming in mid-climb comes up matching the
/// tiles around it instead of popping glossy — the same contract
/// `GlobeSurfaceAlbedo` has for the albedo ramp, and written by the same system.
#[derive(Resource, Default)]
pub struct GlobeOceanGloss(pub f32);
