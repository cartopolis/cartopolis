//! **Terrain relief**: the measured ground, as one field the whole map stands on.
//!
//! `systems::height` answers questions about *specific* things — how high is this
//! bridge deck, what is the ground under the anchor. This module asks the one
//! question everything else needs: **how high is the ground here**, for any point
//! near the camera, cheaply, thousands of times per mesh build. It is the layer
//! that turns the client's flat `y = 0` plane into relief.
//!
//! # One box, baked once
//!
//! A consumer cannot afford a round trip per vertex, so this is not a cache of
//! per-question patches: it is a single [`HeightRequest`] — 6.4 km of ground at
//! 256×256 (25 m/sample, ~256 KB) — fetched once per map anchor and **baked** into
//! a regular grid in the anchored world-XZ frame. After that, [`TerrainField::at`]
//! is a bilinear lookup with no I/O and no allocation, and every ground-attached
//! system samples the same field, which is what keeps the road, the water, the
//! trees, the buildings and the avatar on the *same* surface.
//!
//! Baking converts each grid point through `world → lat/lng → RD` rather than
//! mapping the RD box onto the world box directly. RD's grid is rotated against
//! Web Mercator's by the meridian convergence (~1° over the Netherlands) and
//! scaled slightly differently; taking the axis-aligned shortcut puts the corners
//! of a 6.4 km field off by ~80 m, which is a hill in the wrong place.
//!
//! # Relative to the anchor, and tapered to nothing at the edge
//!
//! Samples are stored **relative to the ground at the anchor**, so `y = 0` keeps
//! meaning "the ground where you are" and every existing placement stays right at
//! the map's centre. The alternative — absolute metres above NAP — would drop the
//! whole world 322 m below the camera in Vaals and lift it above in the polder,
//! for no gain: what the eye reads as relief is the *difference* between here and
//! there.
//!
//! The field is finite (6.4 km) but the map is not: the textured clipmap runs to
//! the horizon and the world beyond AHN is flat by definition. So the field
//! **tapers to exactly zero** at its own boundary, baked into the samples rather
//! than applied at lookup — outside the box `at()` returns 0, and the two meet
//! without a step. The cost is real and worth naming: a hillside crossing the
//! boundary is flattened toward the anchor's level over the last ~600 m. Measured
//! against the alternative it is cheap — see `docs/notes/terrain-relief.md` for
//! what NL relief actually is over a 6.4 km box.
//!
//! # No-data is normal
//!
//! AHN's DTM is full of holes and that is not an error: bridges and buildings are
//! *cut out* of it, the sea has no measurement, and a box near the border is half
//! Germany. Small holes are filled from their neighbours (out to [`cartopolis_geo::ahn::FILL_REACH_M`])
//! because a cut-out viaduct must not read as a pit;
//! what is left over — the sea, the far side of a border — settles at the anchor's
//! own level, which is flat, and flat is the honest answer for ground we have not
//! measured.

use bevy::prelude::*;

use crate::components::FreeCamera;
use crate::grid::WORLD_CELL_EDGE;
use crate::systems::geo_nav::{world_to_lat_lng, WorldFrame};
use crate::systems::height::{wgs84_to_rd, HeightLayer, HeightModel, HeightPatch, HeightRequest};
use crate::systems::map_stream::MapStream;
use futures_channel::oneshot;
use std::sync::{Arc, Mutex};

/// Half-extent of the field in world metres (a 6.4 km box). Sized to cover the
/// geometry ring and the finest textured tiles around it, which is everything
/// close enough for relief to read as relief.
pub const TERRAIN_HALF_M: f32 = 3_200.0;

/// Samples per side, so that [`TERRAIN_SAMPLE_M`] is 3.125 m over
/// [`TERRAIN_HALF_M`]'s 6.4 km box. `2049`, not `2048`, so that the *intervals*
/// between samples are a power of two and [`build_floors`]' pyramid halves exactly.
///
/// **This was 256 (25 m) and 25 m is the resolution at which a Dutch city has no
/// slopes.** Measured over Groningen 2026-09-13, PDOK's `dtm_05m` over the field's
/// own box, comparing the surface normal a mesh would be shaded by:
///
/// | field spacing | median slope | Lambert term against flat ground, sun at 30° |
/// |---|---|---|
/// | 25 m | 0.46° | 0.96 – 1.04 |
/// | 12.5 m | 0.72° | 0.92 – 1.07 |
/// | 6.25 m | 1.07° | 0.86 – 1.13 |
/// | 3.125 m | 1.34° | 0.77 – 1.19 |
///
/// The relief was never missing: the Hondsrug rises 7 m through the centre of that
/// box and falls to −0.7 m a kilometre west, and the 25 m field carried all of it.
/// It carried it over 256 samples, so no facet ever tilted more than half a degree
/// and the whole ridge shaded within ±4 % of flat — present, correct, and invisible
/// under the map imagery. What 6.25 m adds is not more ground, it is ground with a
/// gradient in it.
///
/// It is not finer than that because this is **one** request and one bake: 2049² is
/// 12 MB on the wire (measured, 4.5 s) and 17 MB resident, and AHN's own 0.5 m over
/// the same box would be 12800², 625 MB and 144 requests. Half a metre is what the
/// fine tier ([`FINE_HALF_M`]) follows the camera to reach, for the consumers that
/// ask about a *point* rather than build a mesh.
pub const TERRAIN_GRID: usize = 2049;

/// Ground metres between adjacent samples (3.125 m). Public because it is the only
/// honest answer to "how finely should I subdivide a mesh that stands on this
/// field": below it a grid interpolates measurements that were never taken, and
/// pays full geometry price for them. [`displaced_grid`]'s callers size themselves
/// off this rather than off a constant chosen for one zoom level.
pub const TERRAIN_SAMPLE_M: f32 = TERRAIN_HALF_M * 2.0 / (TERRAIN_GRID as f32 - 1.0);

/// Half-extent of the **fine tier** in world metres (a 512 m box), and its grid.
///
/// 0.5 m is AHN's own sampling, and the reason to want it is that 25 m cannot express
/// what a person standing on the ground is looking at. Measured on the Zuiderhaven
/// 2026-09-11: at half a metre a Groningen canal is a clean 10.5 m run of no-data
/// between two distinct bank levels; at 25 m it is one sample.
///
/// The size is a memory and a citizenship decision, not a quality one. One WCS request
/// answers a 1024² box (1.5 MB on the wire, 4 MB decoded, 0.5 s measured); the *whole*
/// coarse box at half a metre would be 12800², 625 MB and 144 requests. So this is a
/// tier that follows the camera, never a replacement for the field.
pub const FINE_HALF_M: f32 = 256.0;
const FINE_GRID: usize = 1024;

/// Ground metres between adjacent fine samples (~0.5 m).
pub const FINE_SAMPLE_M: f32 = FINE_HALF_M * 2.0 / (FINE_GRID as f32 - 1.0);

/// How far the camera may drift from the fine tier's centre before a new box is
/// fetched. The centre is snapped to this lattice, which *is* the hysteresis: the
/// wanted box only changes when the camera crosses a line, so a camera sitting still
/// asks for nothing and walking back onto a box re-uses it.
const FINE_RECENTRE_M: f32 = 96.0;

/// Width of the ring, in world metres, over which the fine tier fades into the coarse
/// field at its own boundary.
///
/// Not the [`edge_taper`] the coarse field uses, and the difference matters: that one
/// fades to *flat*, because outside the coarse field there are no measurements at all.
/// Here there are — the coarse field covers this ground too — so the fine tier fades to
/// **the coarse answer**, and the two agree at the rim instead of meeting at a step.
const FINE_BLEND_M: f32 = 24.0;

/// Fraction of the half-extent held at full strength before the edge taper begins.
const TAPER_START: f32 = 0.82;

/// The same for a field over graded ground, which follows the camera: the taper
/// moves with the field, so every cell standing in it is rebuilt each time the
/// field does. At 0.82 that was every building cell of the ring (Deck,
/// 2026-09-24, 29 of 29 after a 2.4 km move); at 0.95 the fade is 160 m wide and
/// three kilometres from the camera, which a following field keeps it.
const GRADED_TAPER_START: f32 = 0.95;

/// How far the no-data fill reaches from the nearest measurement, in **metres**.
/// Covers a cut-out viaduct or a building footprint and stops well short of smearing
/// a coastline across the North Sea; past it the ground stays unknown and settles at
/// the anchor's own level.
///
/// A distance, not a pass count. It used to be six one-cell dilations "reaching
/// ~150 m", which was true of the 25 m field it was written for and would be 37 m of
/// the 6.25 m one — every building footprint an unfilled pit, with nothing in the
/// constant to say the resolution had moved out from under it.

/// Extra RD metres requested around the field's own box, so a bilinear lookup at
/// the very edge still has measured samples on both sides of it.
const REQUEST_PAD_M: f64 = 80.0;

/// The sampled ground, shareable by value.
///
/// Cloning is an `Arc` bump, so a mesh-building **worker thread** can carry the
/// terrain with it. That is not a convenience: a building's base is one height for
/// the whole footprint, and the only place footprints exist as such is inside the
/// worker that assembles the cell — by the time a cell reaches the main thread it
/// is one merged triangle soup with no buildings in it any more.
#[derive(Clone, Default)]
pub struct TerrainSnapshot {
    /// `TERRAIN_GRID²` samples, all finite, row-major with x fastest and both axes
    /// ascending in world metres. Empty until the first bake lands.
    samples: std::sync::Arc<Vec<f32>>,
    /// Half-extent the samples span, in world metres.
    half_m: f32,
    /// Samples per side. Carried rather than read off [`TERRAIN_GRID`], because there
    /// is now more than one resolution: the coarse field is 256² over 6.4 km and the
    /// fine tier is [`FINE_GRID`]² over [`FINE_HALF_M`]. A snapshot that took its row
    /// stride from a module constant would silently index the other one's samples as
    /// if they were its own.
    grid: usize,
    /// Centre of the box, in the anchored world-XZ frame. `(0, 0)` for the coarse
    /// field, which is centred on the map anchor by construction; the fine tier
    /// follows the camera and is not.
    centre: Vec2,
    /// Lower envelopes of the same ground, one per doubling of sample spacing —
    /// what [`floor_at`](Self::floor_at) reads. Empty on the fine tier, which no
    /// mesh samples.
    floors: std::sync::Arc<Vec<FloorLevel>>,
    /// Per sample, [`WET`] inside a registered water body, [`BANK`] beside one, 0
    /// elsewhere; empty where no water is registered. What [`land_at`](Self::land_at)
    /// and [`sunk_at`](Self::sunk_at) read — see [`bank_flags`].
    banks: std::sync::Arc<Vec<u8>>,
}

/// A sample inside a registered water body: carved to its level less a bed.
const WET: u8 = 1;
/// A sample within one spacing of a [`WET`] one, on dry ground.
const BANK: u8 = 2;

/// Which samples of an `n`² field lie in a registered water body, and which beside
/// one — the mask that lets the ground beside a canal and the canal itself be
/// drawn at two heights with a wall between them.
///
/// **The field cannot hold a quay.** A canal is carved into it at the sample
/// spacing, so bilinear interpolation between the last sample on the street and
/// the first in the canal is a slope 3 m wide on a desktop and 12.5 m wide on a
/// handset. Everything read off the field sagged into that slope: the street ran
/// downhill into the water and the water, clamped above the basemap, climbed up to
/// meet it (the handset captures of 2026-09-25, "the inner canals don't have
/// walls"). The mask is what lets the two sides stop sharing a surface: the ground
/// reads only dry samples ([`TerrainSnapshot::land_at`]), the basemap and the water
/// read the bank as part of the canal ([`TerrainSnapshot::sunk_at`]), and
/// `map_geometry::quay_walls` stands a wall in the step between.
///
/// **One spacing of bank, not zero.** A graded field is carved on atlas's lattice
/// and resampled onto this one, so the first sample outside a ring can carry part
/// of the carve in it; a handset's thinned tiles move that a whole atlas sample.
/// Treating it as bank rather than street is what keeps that partial dip out of the
/// street. The ring test is the strict one the carve itself makes
/// (`WaterLevelField::level_at`), so on a field graded here `WET` is exactly the
/// carved set.
fn bank_flags(
    n: usize,
    half_m: f32,
    centre: Vec2,
    water: &crate::systems::water_levels::WaterLevelField,
) -> Vec<u8> {
    if water.is_empty() || n < 2 {
        return Vec::new();
    }
    let lattice = field_lattice(n, half_m, centre);
    let mut flags = vec![0u8; n * n];
    // By scanline, and independent per row (`for_rows`).
    for_rows(&mut flags, n, |z, row| {
        water.mark_wet_row(&lattice, z, row, WET)
    });
    if !flags.contains(&WET) {
        return Vec::new();
    }
    for z in 0..n {
        for x in 0..n {
            if flags[z * n + x] == WET {
                continue;
            }
            let beside = (z.saturating_sub(1)..=(z + 1).min(n - 1)).any(|zz| {
                (x.saturating_sub(1)..=(x + 1).min(n - 1)).any(|xx| flags[zz * n + xx] == WET)
            });
            if beside {
                flags[z * n + x] = BANK;
            }
        }
    }
    flags
}

/// [`TerrainSnapshot::land_at`] as the shared refinement asks about ground — what
/// the surfaces beside the water are refined against and stood on.
pub struct LandField<'a>(&'a TerrainSnapshot);

impl cartopolis_geo::terrain_refine::GroundField for LandField<'_> {
    fn min_ground_edge_m(&self) -> f32 {
        self.0.min_ground_edge_m()
    }

    fn at(&self, x: f32, z: f32) -> f32 {
        self.0.land_at(x, z)
    }
}

/// One rung of the lower-envelope pyramid: the field, min-filtered so that a mesh
/// with quads this coarse cannot rise above the true ground anywhere.
#[derive(Clone)]
struct FloorLevel {
    samples: Vec<f32>,
    grid: usize,
    /// Metres between adjacent samples at this rung.
    spacing: f32,
}

/// Rungs of the pyramid. Five doublings of [`TERRAIN_SAMPLE_M`] reach 100 m quads,
/// past the coarsest a relieved clipmap tile can ask for ([`RELIEF_MAX_TILE_M`]
/// divided by `map_stream`'s division count).
const FLOOR_LEVELS: usize = 5;

/// Build the lower-envelope pyramid over a baked field.
///
/// **What it is for.** A clipmap level far from the camera does not need a vertex
/// every 3 m and cannot afford one, so its quads span many field samples and its
/// surface only chords the ground. Chording is fine on its own; what is not fine is
/// that the levels are stacked 0.4 m apart, so a coarse level that chords *above* the
/// fine level drawn over it shows through as blurry imagery in the middle of detailed
/// ground. That is why every relieved level used to be forced to one quad per field
/// sample — which tied a tile 3 km away to the resolution the tile underfoot needs,
/// and is what pinned the field at 25 m.
///
/// So the coarse levels read a **minimum** instead of the measurement, and then the
/// stacking is margin rather than the whole guarantee. The rung for quads of `q`
/// metres holds, at each of its samples, the lowest ground within ±2 of its own
/// spacing; interpolating it anywhere in a cell is at most the largest of the four
/// corners, each of which is a minimum over a box containing everything the mesh cell
/// touches. So the drawn surface is **everywhere at or below the measured ground**,
/// at any relief and with no tuning constant, and a level can no more rise through
/// the one above it than a flat plane could.
///
/// Built as a plain 2× min-reduction chain (so the whole pyramid costs about a third
/// of one pass over the field) with a 5-wide min over each rung at the end — five and
/// not three because a mesh vertex has to bound the cells on *both* sides of it.
fn build_floors(base: &[f32], n: usize, sample_m: f32) -> Vec<FloorLevel> {
    let mut out = Vec::with_capacity(FLOOR_LEVELS);
    // `reduced` is the aligned 2×2 min chain: `reduced[a]` is the lowest sample in the
    // block of base cells this rung stands for.
    let mut reduced = base.to_vec();
    let mut rn = n;
    for level in 0..FLOOR_LEVELS {
        if rn < 3 {
            break;
        }
        let half = (rn - 1) / 2 + 1;
        let mut next = vec![f32::MAX; half * half];
        for z in 0..half {
            for x in 0..half {
                let mut m = f32::MAX;
                for dz in 0..2 {
                    for dx in 0..2 {
                        let (sx, sz) = ((x * 2 + dx).min(rn - 1), (z * 2 + dz).min(rn - 1));
                        m = m.min(reduced[sz * rn + sx]);
                    }
                }
                next[z * half + x] = m;
            }
        }
        reduced = next;
        rn = half;
        // Widen to ±2 rungs, so a mesh vertex placed anywhere in a cell bounds the
        // ground under every cell that vertex belongs to, not only the one it fell in.
        let mut widened = vec![f32::MAX; rn * rn];
        for z in 0..rn {
            for x in 0..rn {
                let mut m = f32::MAX;
                for dz in -2i32..=2 {
                    let sz = (z as i32 + dz).clamp(0, rn as i32 - 1) as usize;
                    for dx in -2i32..=2 {
                        let sx = (x as i32 + dx).clamp(0, rn as i32 - 1) as usize;
                        m = m.min(reduced[sz * rn + sx]);
                    }
                }
                widened[z * rn + x] = m;
            }
        }
        out.push(FloorLevel {
            samples: widened,
            grid: rn,
            spacing: sample_m * (1 << (level + 1)) as f32,
        });
    }
    out
}

/// One finished field bake, on its way back from a worker.
///
/// `baked` is `None` for a box AHN has no measurement anywhere in — an answer, and
/// one the field has to remember rather than re-ask.
/// The clock and the camera, as one system parameter: `update_terrain_field` is at
/// Bevy's sixteen.
#[derive(bevy::ecs::system::SystemParam)]
pub struct FieldClock<'w, 's> {
    time: Res<'w, Time>,
    camera: Query<
        'w,
        's,
        (&'static big_space::prelude::GridCell, &'static Transform),
        With<FreeCamera>,
    >,
}

impl FieldClock<'_, '_> {
    /// The camera in the anchored world-XZ frame.
    fn camera_xz(&self) -> Option<Vec2> {
        let (cell, tf) = self.camera.single().ok()?;
        Some(Vec2::new(
            cell.x as f32 * WORLD_CELL_EDGE + tf.translation.x,
            cell.z as f32 * WORLD_CELL_EDGE + tf.translation.z,
        ))
    }
}

/// How far the camera may get from the field's centre before the field follows
/// it, and the lattice the centre snaps to — a whole number of samples at every
/// grid a budget picks, so the samples two fields share sit at the same places
/// and come out the same. Graded ground only: see `update_terrain_field`.
const FOLLOW_M: f32 = 1_600.0;

/// Half-extent of a field over graded ground: 4.8 km, half as wide again as
/// [`TERRAIN_HALF_M`], at the same spacing, so the grid is `(grid − 1)·3/2 + 1`
/// (3073² on the desktop, ~38 MB). See `update_terrain_field`.
const GRADED_HALF_M: f32 = TERRAIN_HALF_M * 1.5;

/// Margin around a cell whose ground its build reads beyond its own edge: the
/// tunnel lids' reach (`tunnels::DATUM_M`, 30 m) with room.
const CELL_DIGEST_PAD_M: f32 = 50.0;
const FOLLOW_LATTICE_M: f32 = 800.0;

struct CoarseBake {
    /// The field's centre in the anchored frame, and its half-extent.
    centre: Vec2,
    half: f32,
    gen: u32,
    /// Samples per side this bake used — `quality::Budget::terrain_grid`, which a
    /// handset lowers, so the snapshot cannot read it off [`TERRAIN_GRID`].
    grid: usize,
    water_rev: u32,
    /// The road-mask revision this bake graded against, so a mask that grew while
    /// it was out asks for another one.
    road_rev: u32,
    deck_rev: u64,
    /// The rings the bake carved with, handed back so the water surfaces are drawn
    /// against the same numbers the ground was cut to.
    water: crate::systems::water_levels::WaterLevelField,
    /// The finished ground — samples, [`bank_flags`] (empty with no water
    /// registered) and floors — with its datum and carved-sample count.
    baked: Option<(TerrainSnapshot, f32, usize)>,
    /// `(abutment samples, samples the embankment raised)` — see
    /// [`TerrainField::approach_samples`].
    approach: (usize, usize),
}

/// One finished fine-tier bake, on its way back from a worker.
///
/// `samples` is `None` for a box AHN has no measurement anywhere in — an answer, and
/// one the tier has to remember rather than re-ask.
struct FineBake {
    centre: Vec2,
    gen: u32,
    water_rev: u32,
    samples: Option<(Vec<f32>, usize)>,
    /// The same ground as a **slope residual**, `Rg8Snorm`, [`FINE_GRID`]² —
    /// `systems::ground_detail` binds it to the ground materials so a surface can shade
    /// at half a metre without carrying a vertex there. Produced on the worker that
    /// already holds the samples rather than in a second pass over them.
    slope: Option<Vec<u8>>,
}

/// The measured ground near the anchor, as a sampled field.
///
/// Heights are metres **relative to the ground at the map anchor** (see the module
/// docs), positive upward, in the same anchored world-XZ frame the map surfaces
/// and the building colliders use.
#[derive(Resource, Default)]
pub struct TerrainField {
    ground: TerrainSnapshot,
    /// Map-anchor generation this field has already **answered for** — with
    /// samples or without. A re-anchor invalidates it because the whole frame
    /// moved.
    ///
    /// "Answered", not "baked", and the distinction is the whole point. It used to
    /// be a bare `u32` paired with an `is_ready()` test, which meant a box AHN has
    /// no data for — all sea, over the border, anywhere outside the Netherlands
    /// that `covers` still admits — never satisfied the guard: `bake` returned
    /// `None`, `clear()` early-returned because the field was already empty,
    /// `is_ready()` stayed false, and `update_terrain_field` re-baked 65,536
    /// samples on the main thread on **every frame, forever**. The comment beside
    /// it claimed the opposite. An empty answer is still an answer, and only an
    /// `Option` can say so.
    answered_gen: Option<u32>,
    /// Absolute height (m above NAP) that `0.0` means here — the ground under the
    /// anchor. Not used for placement (everything is relative); reported, because
    /// it is the one number that says *where* the field is.
    pub datum_m: f32,
    /// Bumped on every change, including going away. Consumers that bake the field
    /// into a mesh compare it to know when their geometry is stale.
    pub revision: u32,
    /// Lowest and highest sample, metres relative to the datum — the field's own
    /// answer to "is there any relief here at all".
    pub range_m: (f32, f32),
    /// Water-ring revision the coarse field was baked against. The rings land *after*
    /// the field they carve, so "answered" is a pair: re-bake when either the anchor or
    /// the set of rings moves. Without the second half the first field over a city is
    /// the only one that ever exists, and it is the one baked before any water arrived.
    answered_water_rev: Option<u32>,
    /// Road-mask revision the coarse field was graded against. The third half of
    /// the same pair: the roads land after the field too, and without this the only
    /// field a city ever gets is the one baked before any road arrived — which is
    /// the one with a ridge along every street.
    answered_road_rev: Option<u32>,
    /// Deck revision the coarse field built its approach embankments against.
    ///
    /// The fourth of the same family, and the one that arrives latest of all: a
    /// deck's height is measured from the survey a few frames after its cell
    /// lands (`map_geometry::resolve_bridge_decks`), so the first field over a
    /// city is always baked before any viaduct has a height. Without this the
    /// only field a city gets is that one, and every bridge keeps the vertical
    /// step at both ends that `bridge-heights.md` has recorded as unfixed since
    /// the decks landed.
    answered_deck_rev: Option<u64>,
    /// What the last bake's approach grading did: samples pinned at a deck's
    /// abutment, and samples the embankment raised.
    ///
    /// **A metric, because an embankment is not a thing a capture can assert.**
    /// The ramp is 150 m of ground rising six metres, which from any angle that
    /// shows the viaduct reads as ground; a number says whether it was built.
    /// Zero abutments with decks standing is the failure this reports — the
    /// approaches never reached the field — and it is the shape the whole feature
    /// had for a month while the grading sat wired into `carto-view` alone.
    pub approach_samples: (usize, usize),
    /// The measured water levels the bakes carved with, kept so the surfaces can be
    /// drawn against the same numbers. One build per bake rather than one per mesh.
    water: crate::systems::water_levels::WaterLevelField,
    /// Samples the last coarse and fine bakes actually rewrote to a water level.
    ///
    /// Reported in `--dump-state`, and the reason is a wrong turn worth not taking
    /// twice: `terrain_relief_m` was read as evidence the carve had done nothing, and
    /// it is not — it is the range over the whole 6.4 km box, which already spans tens
    /// of metres of the Hondsrug, so a canal cut to −4 m never widens it. "Did the
    /// carve write anything" has to be counted where it happens.
    pub carved_coarse: usize,
    pub carved_fine: usize,
    /// The half-metre tier around the camera ([`FINE_HALF_M`]), empty until one lands.
    /// Read through [`TerrainField::ground_at`] and nowhere else.
    fine: TerrainSnapshot,
    /// Centre of the box the fine tier has *answered* for, so a camera sitting still
    /// costs a comparison rather than a bake.
    fine_wanted: Option<Vec2>,
    /// Map-anchor generation the fine tier's centre is expressed in.
    ///
    /// Not redundant with `answered_gen`. The coarse field is centred on the anchor by
    /// construction, so a re-anchor re-fetches it and the centre is still `(0,0)`; the
    /// fine tier carries an *offset* from that anchor, and a re-anchor moves the origin
    /// those metres are counted from. Without this the tier would keep describing
    /// ground a few hundred metres from where it claims to be — silently, because it
    /// would still be perfectly smooth.
    fine_gen: Option<u32>,
    /// Water-ring revision the fine tier was baked against; the rings land after it too.
    fine_water_rev: Option<u32>,
    /// The field bake in flight (the anchor generation it measures), and where its
    /// answer lands.
    ///
    /// On [`platform::workers`](crate::platform::workers) for the same reason the fine
    /// tier is, and it did not used to be: at 25 m the field was 65,536 samples and
    /// ran inline on the main thread. At [`TERRAIN_SAMPLE_M`] it is four million — a
    /// `wgs84_to_rd` polynomial each, then a distance transform to close the holes and
    /// a min pyramid over the result — which is a long frame, and a long frame is the
    /// documented way to lose the device on the hardware this has to run on.
    coarse_job: Option<u32>,
    coarse_sink: std::sync::Arc<std::sync::Mutex<Option<CoarseBake>>>,
    /// The anchor generation a field is *expected* for — `None` with the layer off or
    /// outside AHN. Paired with `answered_gen` it is the streamer-style "still working"
    /// signal the shot harness settles on.
    wanted_gen: Option<u32>,
    /// The bake in flight, and where its answer lands.
    ///
    /// On `platform::workers` rather than inline, for the reason the grass mask is:
    /// resampling 1024² samples and closing their holes is ~30 million operations,
    /// which is a frame or more even on a desktop. The tier re-centres every ~96 m of
    /// walking, so inline this would be a visible hitch every half-minute on foot — and
    /// a long frame is the documented way to lose the device on the hardware this has
    /// to run on.
    fine_job: Option<(Vec2, u32)>,
    fine_sink: std::sync::Arc<std::sync::Mutex<Option<FineBake>>>,
    /// The tier's residual slope, waiting for `systems::ground_detail` to take it.
    /// `None` once taken — it is a megabyte and there is no second reader.
    fine_slope: Option<Vec<u8>>,
    /// Bumped whenever the fine tier changes.
    ///
    /// Deliberately **not** `revision`. That one is the retear signal: sixteen systems
    /// answer it by rebuilding every map cell, both building streamers, the interiors
    /// and the route line. The fine tier re-centres every ~96 m of walking, so folding
    /// it into `revision` would retear the whole city about every minute on foot.
    pub fine_revision: u32,
    /// The inputs have moved past the last bake and a new one has not been queued
    /// yet — held for the surface ring, or waiting on the download. Part of
    /// [`streaming_busy`](Self::streaming_busy), because every bake retears the
    /// ground and a capture taken before it would show the ground it replaces.
    bake_owed: bool,
    /// The anchor generation whose ground is graded on atlas and on its way, and
    /// when the wait for it began — see [`Self::holds_builds`].
    build_hold: Option<(u32, f32)>,
    /// Where the installed field is centred, in the anchored frame. The origin, or
    /// over graded ground the camera's lattice point ([`FOLLOW_M`]).
    answered_centre: Option<Vec2>,
    /// The datum this anchor's fields are relative to, fixed at its first bake: a
    /// field that follows the camera must not move the zero every other height in
    /// the world is measured from.
    gen_datum: Option<(u32, f32)>,
    /// Cell jobs that have fetched their inputs and are waiting for that ground
    /// ([`Self::ground_later`]). Answered with whatever is in place once the hold
    /// ends, however it ends.
    ground_waiters: Arc<Mutex<Vec<oneshot::Sender<GroundArrival>>>>,
}

/// Ground a waiting cell job is handed: the placement (`None` for flat) and the
/// field revision it is, which the cell records so that only cells standing on an
/// older revision are rebuilt when the field changes.
pub type GroundArrival = (Option<GroundPlacement>, u32);

/// Longest graded ground waits for the water levels before it is baked without
/// them. They are a few dozen small files from our own host.
const GRADED_WATER_HOLD_S: f32 = 3.0;

/// Longest the ground and building cells wait for graded ground before building
/// flat anyway. It lands in ~3 s warm and ~5 s cold on the Deck (2026-09-24).
const GRADED_HOLD_S: f32 = 8.0;

impl TerrainField {
    /// Whether cells for this anchor should wait before they are built.
    ///
    /// **Only for ground atlas has graded.** That field needs nothing from the
    /// cells and is a few seconds away, so building them flat first spends a whole
    /// ring of cell jobs on ground that is torn down the moment it lands — measured
    /// on the Deck, every surface and building cell built twice in a warm start.
    /// Ground graded here cannot be waited for: its bake needs the cells' roads.
    #[must_use]
    pub fn holds_builds(&self, gen: u32) -> bool {
        matches!(self.build_hold, Some((g, _)) if g == gen)
    }

    /// The ground for a cell job to be built on, when the hold ends. The job
    /// fetches its inputs meanwhile: holding the fetch as well cost a cold start
    /// its first five seconds of downloads (Deck, 2026-09-24: 20.4 s → 24.9 s).
    #[must_use]
    pub fn ground_later(&self) -> oneshot::Receiver<GroundArrival> {
        let (tx, rx) = oneshot::channel();
        match self.ground_waiters.lock() {
            Ok(mut w) => w.push(tx),
            Err(p) => p.into_inner().push(tx),
        }
        rx
    }

    /// Hand every waiting job the ground in place now.
    fn release_waiters(&self) {
        let waiting: Vec<_> = match self.ground_waiters.lock() {
            Ok(mut w) => w.drain(..).collect(),
            Err(p) => p.into_inner().drain(..).collect(),
        };
        if waiting.is_empty() {
            return;
        }
        let arrival = (self.placement(), self.revision);
        for tx in waiting {
            let _ = tx.send(arrival.clone());
        }
    }

    /// Whether a field is expected here and has not arrived yet.
    ///
    /// The bake is on a worker now, so a scripted capture can reach the shutter with the
    /// ground still flat — and would photograph exactly what "the layer does nothing"
    /// looks like, with `terrain_ready=false` the only thing saying otherwise. The other
    /// streamers already report this shape and `shot_harness` already settles on it.
    #[must_use]
    pub fn streaming_busy(&self) -> bool {
        match self.wanted_gen {
            None => false,
            Some(g) => self.answered_gen != Some(g) || self.coarse_job.is_some() || self.bake_owed,
        }
    }

    /// A by-value handle on the same measurements, for a worker thread.
    #[must_use]
    pub fn snapshot(&self) -> TerrainSnapshot {
        self.ground.clone()
    }

    /// Ground height at a point, preferring the half-metre tier where it reaches.
    ///
    /// **For point consumers only** — the avatar's footing, the collision eject, a car
    /// on a road, a pin over the map. Anything that *bakes a mesh* must keep calling
    /// [`TerrainSnapshot::at`] on the coarse field, and that is not a style preference:
    /// subdivision is sized off [`TERRAIN_SAMPLE_M`] ([`MAX_GROUND_EDGE_M`],
    /// `map_stream::relief_divisions`), so a mesh with a vertex every 24 m that sampled
    /// a half-metre field would alias it — and would change shape every time the tier
    /// re-centres, which is a 25 m grid twitching as you walk.
    ///
    /// Same contract as `at`: `0.0` when nothing is known, so a caller adds it
    /// unconditionally.
    #[must_use]
    pub fn ground_at(&self, wx: f32, wz: f32) -> f32 {
        let coarse = self.ground.at(wx, wz);
        if !self.fine.is_ready() {
            return coarse;
        }
        let w = self.fine.rim_weight(wx, wz);
        if w <= 0.0 {
            return coarse;
        }
        coarse + (self.fine.at(wx, wz) - coarse) * w
    }

    /// Whether the half-metre tier currently carries measurements.
    #[must_use]
    pub fn fine_ready(&self) -> bool {
        self.fine.is_ready()
    }

    /// Take the tier's residual-slope texels, if a bake has just handed some over.
    ///
    /// Consumed rather than borrowed: `systems::ground_detail` copies it straight into
    /// an `Image`, and a second copy sitting here for the life of the tier would serve
    /// nobody.
    #[must_use]
    pub fn take_fine_slope(&mut self) -> Option<(Vec<u8>, usize, Vec2, f32)> {
        let bytes = self.fine_slope.take()?;
        Some((bytes, FINE_GRID, self.fine.centre, FINE_HALF_M))
    }

    /// The measured surface height of the water at a point, in the world's own frame,
    /// or `None` where no registered body covers it.
    ///
    /// Deliberately *not* the ground under it: the ground there has been carved to this
    /// minus a bed (`cartopolis_geo::terrain_grade::WATER_BED_M`), so a sheet laid on the ground
    /// would be half a metre under its own canal.
    #[must_use]
    pub fn water_surface_at(&self, wx: f32, wz: f32) -> Option<f32> {
        water_surface_near(&self.water, self.datum_m, wx, wz)
    }

    /// What a cell over this world box was built on — the heights under it (with a
    /// margin, for the drape's and the tunnels' reach) and the water over it — as
    /// one number. A new field that gives a cell the same number leaves it
    /// standing (`MapSurfaces::built_rev`, `Lod22Buildings::built_rev`).
    #[must_use]
    pub fn cell_digest(&self, min: Vec2, max: Vec2, with_water: bool) -> u64 {
        let pad = Vec2::splat(CELL_DIGEST_PAD_M);
        let (lo, hi) = (min - pad, max + pad);
        // Only for a consumer that draws water: the buildings stand on the ground
        // and nothing else, and every water cell the ring loads as the camera moves
        // would otherwise rebuild them.
        let water = if with_water && self.is_ready() {
            self.water.digest_over(lo.to_array(), hi.to_array())
        } else {
            0
        };
        self.ground.digest_over(lo, hi) ^ water.rotate_left(17) ^ self.datum_m.to_bits() as u64
    }

    /// Everything a mesh needs to be stood on this ground, by value, for a worker —
    /// or `None` while there is no field to stand on.
    ///
    /// The coarse samples and the water rings are both behind `Arc`s, so this is a
    /// handful of reference counts rather than a copy of the field.
    #[must_use]
    pub fn placement(&self) -> Option<GroundPlacement> {
        self.is_ready().then(|| GroundPlacement {
            ground: self.ground.clone(),
            water: self.water.clone(),
            datum_m: self.datum_m,
        })
    }

    /// How far below the world's own zero the deepest registered water surface sits, in
    /// metres. `0.0` with nothing loaded.
    ///
    /// Paired with `carved_*` it separates "the register says nothing here" from "it
    /// says four metres and nothing moved".
    #[must_use]
    pub fn water_drop_m(&self) -> f32 {
        self.water
            .lowest_nap()
            .map_or(0.0, |nap| self.datum_m - nap)
    }

    fn clear(&mut self) {
        self.clear_fine();
        if !self.is_ready() {
            return;
        }
        self.ground = TerrainSnapshot::default();
        self.range_m = (0.0, 0.0);
        self.revision = self.revision.wrapping_add(1);
    }

    /// Drop the fine tier's samples, keeping the record of which box was asked for.
    ///
    /// The split from [`clear_fine`](Self::clear_fine) is not tidiness. A box AHN has
    /// nothing for — a lake, the far side of the border — is an *answer*, and one this
    /// has to remember: forgetting it here would re-queue the same request and re-bake a
    /// million samples on the next frame, and every frame after, for as long as the
    /// camera stood there. The coarse field carries a scar from exactly this bug (see
    /// `answered_gen`).
    fn drop_fine_samples(&mut self) {
        if !self.fine.is_ready() {
            return;
        }
        self.fine = TerrainSnapshot::default();
        self.fine_revision = self.fine_revision.wrapping_add(1);
    }

    /// Forget a field bake that is on a worker.
    ///
    /// The job still runs and still writes to the sink — nothing can cancel a queued
    /// closure — so the sink is emptied here and the result is matched against the
    /// live generation when it is drained.
    fn abandon_coarse_job(&mut self) {
        self.coarse_job = None;
        if let Ok(mut g) = self.coarse_sink.lock() {
            *g = None;
        }
    }

    /// Drop the fine tier *and* forget what was asked for. For the cases where the
    /// question itself stopped being meaningful: the layer switched off, the coarse
    /// field it borrows a datum from going away, and a re-anchor.
    fn clear_fine(&mut self) {
        self.fine_wanted = None;
        self.fine_gen = None;
        self.fine_water_rev = None;
        // Abandon whatever is on a worker: it measures ground in a frame that no longer
        // exists. The job still runs and still writes to the sink — nothing can cancel a
        // queued closure — so the sink is emptied here and the result is matched against
        // the live generation when it is drained.
        self.fine_job = None;
        if let Ok(mut g) = self.fine_sink.lock() {
            *g = None;
        }
        self.drop_fine_samples();
    }
}

/// Everything a consumer asks of the terrain is asked of the snapshot; the resource
/// just carries the current one.
impl std::ops::Deref for TerrainField {
    type Target = TerrainSnapshot;
    fn deref(&self) -> &Self::Target {
        &self.ground
    }
}

fn water_surface_near(
    water: &crate::systems::water_levels::WaterLevelField,
    datum_m: f32,
    wx: f32,
    wz: f32,
) -> Option<f32> {
    water
        .level_near(wx, wz, crate::systems::water_levels::JOIN_TOLERANCE_M)
        .map(|nap| nap - datum_m)
}

/// What standing a mesh on the measured ground asks of the terrain: the coarse field
/// through `Deref`, plus the registered water levels a canal's surface is drawn at.
///
/// Two implementors because the same placement runs in two places — on the main
/// thread against the live [`TerrainField`] (the bridge decks, which wait there for
/// their height), and on the surface worker against a [`GroundPlacement`].
pub trait MeasuredGround: std::ops::Deref<Target = TerrainSnapshot> {
    fn water_surface_at(&self, wx: f32, wz: f32) -> Option<f32>;
    /// The NAP height this field's zero stands for: an absolute height from
    /// atlas (a road's, a deck's) minus this is a height in the field's frame.
    fn datum_m(&self) -> f32;
}

impl MeasuredGround for TerrainField {
    fn water_surface_at(&self, wx: f32, wz: f32) -> Option<f32> {
        TerrainField::water_surface_at(self, wx, wz)
    }
    fn datum_m(&self) -> f32 {
        self.datum_m
    }
}

/// [`TerrainField::placement`]: the ground a surface cell is stood on, carried onto
/// the worker that tessellates the cell.
#[derive(Clone)]
pub struct GroundPlacement {
    ground: TerrainSnapshot,
    water: crate::systems::water_levels::WaterLevelField,
    datum_m: f32,
}

impl std::ops::Deref for GroundPlacement {
    type Target = TerrainSnapshot;
    fn deref(&self) -> &Self::Target {
        &self.ground
    }
}

impl GroundPlacement {
    /// A placement over a hand-built field and rings, with the bank mask worked
    /// out as a bake would — for tests that stand meshes on a canal.
    #[cfg(test)]
    pub(crate) fn for_test(
        mut ground: TerrainSnapshot,
        water: crate::systems::water_levels::WaterLevelField,
    ) -> Self {
        ground.banks = Arc::new(bank_flags(
            ground.grid,
            ground.half_m,
            ground.centre,
            &water,
        ));
        Self {
            ground,
            water,
            datum_m: 0.0,
        }
    }
}

impl MeasuredGround for GroundPlacement {
    fn water_surface_at(&self, wx: f32, wz: f32) -> Option<f32> {
        water_surface_near(&self.water, self.datum_m, wx, wz)
    }
    fn datum_m(&self) -> f32 {
        self.datum_m
    }
}

impl TerrainSnapshot {
    /// An `n`² field over `±half_m` about the origin, for tests outside this module.
    #[cfg(test)]
    pub(crate) fn from_samples_for_test(samples: Vec<f32>, n: usize, half_m: f32) -> Self {
        Self {
            samples: Arc::new(samples),
            half_m,
            grid: n,
            centre: Vec2::ZERO,
            floors: Arc::default(),
            banks: Arc::default(),
        }
    }

    /// Whether the field carries measurements. `false` means every consumer should
    /// behave exactly as it did before this module existed.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        !self.samples.is_empty()
    }

    /// Where the field is: its centre in the anchored frame and its half-extent.
    /// Not the anchor and not [`TERRAIN_HALF_M`] once the field follows the camera
    /// (`FOLLOW_M`).
    #[must_use]
    pub fn extent(&self) -> (Vec2, f32) {
        (self.centre, self.half_m)
    }

    /// Every sample over a world box, as one number that two fields agree on
    /// exactly when they agree about that ground — whatever their centres, since a
    /// field re-centres by whole samples (`FOLLOW_LATTICE_M`) and a sample at one
    /// world place resamples to the same value. The cells use it to rebuild only
    /// what a new field changed.
    #[must_use]
    pub fn digest_over(&self, min: Vec2, max: Vec2) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut eat = |v: u64| {
            h ^= v;
            h = h.wrapping_mul(0x100_0000_01b3);
        };
        if self.samples.is_empty() || self.grid < 2 {
            eat(0);
            return h;
        }
        let n = self.grid;
        #[allow(clippy::cast_precision_loss)]
        let step = self.half_m * 2.0 / (n - 1) as f32;
        let (ox, oz) = (self.centre.x - self.half_m, self.centre.y - self.half_m);
        let idx = |v: f32, o: f32| ((v - o) / step).floor();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let clamp = |f: f32| f.clamp(0.0, (n - 1) as f32) as usize;
        let (x0, x1) = (clamp(idx(min.x, ox)), clamp(idx(max.x, ox) + 1.0));
        let (z0, z1) = (clamp(idx(min.y, oz)), clamp(idx(max.y, oz) + 1.0));
        // Where the box starts on the world lattice, so two fields that cover
        // different parts of it cannot collide.
        #[allow(clippy::cast_possible_truncation)]
        let (gx, gz) = (
            (ox / step).round() as i64 + x0 as i64,
            (oz / step).round() as i64 + z0 as i64,
        );
        eat(gx as u64);
        eat(gz as u64);
        eat((x1 - x0) as u64);
        eat((z1 - z0) as u64);
        for z in z0..=z1 {
            for x in x0..=x1 {
                eat(u64::from(self.samples[z * n + x].to_bits()));
            }
        }
        h
    }

    /// Ground height at a point in the anchored world-XZ frame, metres relative to
    /// the anchor's ground. `0.0` outside the field, and whenever it is off, out of
    /// coverage or not yet fetched — so a caller can add it unconditionally.
    #[must_use]
    pub fn at(&self, wx: f32, wz: f32) -> f32 {
        self.sample_grid(&self.samples, self.grid, wx, wz)
    }

    /// The highest measured ground within `radius_m` of a point — the level of the
    /// land *around* a hollow rather than in it.
    ///
    /// **This is how a tunnel stops being dug twice.** AHN measures the real thing:
    /// where a Dutch tunnel is an open cut, the bare-ground model *is* the trench
    /// floor (the Groningen ring falls from +5.5 m to −3.6 m across forty metres,
    /// measured 2026-09-16). So the ground sheet a tunnel's depth is expressed
    /// against is, at exactly those points, already seven metres down — and adding
    /// an invented seven on top of a measured nine puts the carriageway sixteen
    /// metres under the street. Subtracting what the measurement already accounts
    /// for needs a datum that the hollow itself does not drag down, which is this.
    ///
    /// A ring of samples rather than a filtered grid: unlike
    /// [`floor_at`](Self::floor_at), whose lower envelope every clipmap tile in the
    /// pyramid reads on every rebuild, this is asked only by the tunnel meshes of the
    /// cells that have one, so nine lookups at the call site beat a whole
    /// max-filtered level nobody else wants.
    #[must_use]
    pub fn surrounding(&self, wx: f32, wz: f32, radius_m: f32) -> f32 {
        let mut hi = self.at(wx, wz);
        for k in 0..8 {
            let a = std::f32::consts::TAU * k as f32 / 8.0;
            hi = hi.max(self.at(wx + radius_m * a.cos(), wz + radius_m * a.sin()));
        }
        hi
    }

    /// The level ground over a lid, or beside a cutting, is put back to: the 80th
    /// percentile of sixteen survey samples, eight at `radius_m` and eight at twice
    /// it.
    ///
    /// **Both obvious answers were measured and both fail** (the ring's portals,
    /// 2026-09-23). The *highest* sample of one ring, which [`surrounding`](Self::surrounding)
    /// still takes for how much the survey dug, jumps whenever anything raised is
    /// within reach: beside the Europaweg it lifted a lid 4–5 m above the street. The
    /// *middle* sample fails the other way wherever the survey caught a pit wider
    /// than the ring: around the north cutting's mouth it came back at −5.4 m against
    /// a street at +0.2, so the ground beside the cutting was filled only to trench
    /// depth and the lid stood five metres above it as a wall across the mouth. The
    /// outer ring reaches past such a pit, and the 80th percentile ignores the top
    /// three samples, which is where a ramp or an embankment lands.
    #[must_use]
    pub fn lid_level(&self, wx: f32, wz: f32, radius_m: f32) -> f32 {
        let mut ring = [0.0f32; 16];
        for (k, h) in ring.iter_mut().enumerate() {
            let a = std::f32::consts::TAU * (k % 8) as f32 / 8.0
                + if k >= 8 {
                    std::f32::consts::PI / 8.0
                } else {
                    0.0
                };
            let r = if k >= 8 { radius_m * 2.0 } else { radius_m };
            *h = self.at(wx + r * a.cos(), wz + r * a.sin());
        }
        ring.sort_by(f32::total_cmp);
        ring[12]
    }

    /// Longest ground edge to leave unsplit before displacement, for *this* field.
    ///
    /// Derived rather than constant because the field's resolution is a budget
    /// decision now (`quality::Budget::terrain_grid`): a handset gets a coarser box,
    /// and refining its surfaces to [`MAX_GROUND_EDGE_M`] against it would pay four
    /// times the triangles for vertices landing between measurements. Following the
    /// snapshot means the two can never drift apart, on any tier, with no plumbing —
    /// which matters because the callers are mesh builders that have a field and no
    /// budget.
    #[must_use]
    pub fn ground_edge_m(&self) -> f32 {
        if !self.is_ready() {
            // Nothing to be displaced *by*, so nothing to split for. `refine_for_terrain`
            // returns after one scan that finds no long edge. This is not merely tidy:
            // with the layer off the old constant still cut every ground polygon in the
            // city at 24 m, for a displacement of exactly zero.
            return f32::INFINITY;
        }
        MAX_GROUND_EDGE_M.max(self.sample_m() * 3.9)
    }

    /// The floor the crease rule refines to, for *this* field — half its own sample
    /// spacing, never finer than [`MIN_GROUND_EDGE_M`] is for the full one.
    ///
    /// Derived for the same reason [`Self::ground_edge_m`] is, and it was the half
    /// of that pair that got left behind: the long-edge limit followed the snapshot
    /// while the floor stayed a constant cut for the desktop's 3.125 m field. On a
    /// handset's 12.5 m field that put eleven halvings between the two, so one quad
    /// could eat a thousand vertices of [`CREASE_VERTEX_BUDGET`] resolving a
    /// bilinear filter, and the cell ran out with most of itself unrefined.
    #[must_use]
    pub fn min_ground_edge_m(&self) -> f32 {
        MIN_GROUND_EDGE_M.max(self.sample_m() * 0.5)
    }

    /// Metres between adjacent samples. `0.0` on an empty snapshot.
    #[must_use]
    pub fn sample_m(&self) -> f32 {
        if self.grid < 2 {
            return 0.0;
        }
        self.half_m * 2.0 / (self.grid - 1) as f32
    }

    /// Ground height for a mesh whose quads are `quad_m` across — the measurement
    /// itself where the mesh is fine enough to carry it, and a **lower envelope**
    /// where it is not.
    ///
    /// The contract is the one [`build_floors`] proves: the surface a mesh draws
    /// through these values never rises above the measured ground anywhere it covers.
    /// That is what lets a distant clipmap level be as coarse as its distance deserves
    /// without showing through the detailed level in front of it.
    #[must_use]
    pub fn floor_at(&self, wx: f32, wz: f32, quad_m: f32) -> f32 {
        match self.floor_for(quad_m) {
            // The bank sunk with the canal, so the basemap stays under a level sheet
            // right up to its wall; the rungs below are built from the same values.
            None => self.sunk_at(wx, wz),
            Some(f) => self.sample_grid(&f.samples, f.grid, wx, wz),
        }
    }

    /// The normal of the surface [`floor_at`](Self::floor_at) describes. Shading has
    /// to come from the geometry actually drawn, or a coarse level is lit as if it
    /// followed detail it does not have.
    #[must_use]
    pub fn floor_normal_at(&self, wx: f32, wz: f32, quad_m: f32) -> Vec3 {
        let Some(f) = self.floor_for(quad_m) else {
            if self.banks.is_empty() {
                return self.normal_at(wx, wz);
            }
            let d = self.sample_m();
            let dx = self.sunk_at(wx + d, wz) - self.sunk_at(wx - d, wz);
            let dz = self.sunk_at(wx, wz + d) - self.sunk_at(wx, wz - d);
            return Vec3::new(-dx, 2.0 * d, -dz).normalize();
        };
        let d = f.spacing;
        let dx = self.sample_grid(&f.samples, f.grid, wx + d, wz)
            - self.sample_grid(&f.samples, f.grid, wx - d, wz);
        let dz = self.sample_grid(&f.samples, f.grid, wx, wz + d)
            - self.sample_grid(&f.samples, f.grid, wx, wz - d);
        Vec3::new(-dx, 2.0 * d, -dz).normalize()
    }

    /// The coarsest-but-sufficient rung for quads of `quad_m`, or `None` when the mesh
    /// should stand on the measurements themselves.
    ///
    /// **The finest relieved level takes no envelope, and that is the point of the
    /// threshold being a whole rung rather than an epsilon.** An envelope is what keeps
    /// a level from rising through the level drawn *over* it; the finest one has
    /// nothing over it, so lowering it buys nothing and costs the detail this whole
    /// change is about — measured over Groningen, the 6.25 m rung sits a median 0.30 m
    /// and a p95 1.95 m below the ground, because a min filter in a canal city finds
    /// the canal. Below it every rung is monotonically lower than the one above
    /// (rung *k*'s window contains rung *k−1*'s), so the stack cannot cross itself.
    ///
    /// What the finest level costs instead is a chord: at twice the sample spacing it
    /// departs from the field by a p95 of 0.17 m over the same ground, against 0.4 m of
    /// level stacking and roughly a metre of envelope under it.
    fn floor_for(&self, quad_m: f32) -> Option<&FloorLevel> {
        if quad_m <= self.sample_m() * 2.0 {
            return None;
        }
        self.floors
            .iter()
            .find(|f| f.spacing >= quad_m)
            .or_else(|| self.floors.last())
    }

    /// Bilinear lookup into one of this snapshot's grids. `0.0` outside the box, and
    /// on an empty snapshot — the answer every consumer adds unconditionally.
    fn sample_grid(&self, samples: &[f32], n: usize, wx: f32, wz: f32) -> f32 {
        if samples.is_empty() || n < 2 {
            return 0.0;
        }
        let span = self.half_m * 2.0;
        let (wx, wz) = (wx - self.centre.x, wz - self.centre.y);
        // Sample coordinates, in grid cells from the south-west corner.
        let fx = (wx + self.half_m) / span * (n - 1) as f32;
        let fz = (wz + self.half_m) / span * (n - 1) as f32;
        if !(0.0..=(n - 1) as f32).contains(&fx) || !(0.0..=(n - 1) as f32).contains(&fz) {
            return 0.0;
        }
        let (x0, z0) = (fx.floor() as usize, fz.floor() as usize);
        let (x1, z1) = ((x0 + 1).min(n - 1), (z0 + 1).min(n - 1));
        let (tx, tz) = (fx - x0 as f32, fz - z0 as f32);
        let s = |x: usize, z: usize| samples[z * n + x];
        let a = s(x0, z0) + (s(x1, z0) - s(x0, z0)) * tx;
        let b = s(x0, z1) + (s(x1, z1) - s(x0, z1)) * tx;
        a + (b - a) * tz
    }

    /// How much this snapshot should be trusted at a world-XZ point: 1 through the
    /// middle, easing to 0 over [`FINE_BLEND_M`] at the boundary, and 0 outside.
    ///
    /// Smoothstepped rather than linear for the reason [`edge_taper`] is: a linear ramp
    /// meets full strength at a crease, and on a blend between two *different
    /// measurements* of the same ground that crease is a visible ridge running round a
    /// square.
    #[must_use]
    fn rim_weight(&self, wx: f32, wz: f32) -> f32 {
        if self.samples.is_empty() || self.half_m <= 0.0 {
            return 0.0;
        }
        let d = (wx - self.centre.x).abs().max((wz - self.centre.y).abs());
        let inner = (self.half_m - FINE_BLEND_M).max(0.0);
        if d <= inner {
            return 1.0;
        }
        if d >= self.half_m {
            return 0.0;
        }
        let t = ((d - inner) / (self.half_m - inner)).clamp(0.0, 1.0);
        1.0 - t * t * (3.0 - 2.0 * t)
    }

    /// Surface normal at a world-XZ point, from the field's own slope (central
    /// differences over one grid step). `+Y` on flat ground and wherever the field
    /// is absent.
    #[must_use]
    pub fn normal_at(&self, wx: f32, wz: f32) -> Vec3 {
        if self.samples.is_empty() {
            return Vec3::Y;
        }
        let d = self.half_m * 2.0 / (self.grid - 1) as f32;
        let dx = self.at(wx + d, wz) - self.at(wx - d, wz);
        let dz = self.at(wx, wz + d) - self.at(wx, wz - d);
        Vec3::new(-dx, 2.0 * d, -dz).normalize()
    }

    /// The ground beside the water: [`at`](Self::at) with every sample in or beside a
    /// registered water body replaced by the dry ground around it, so a street
    /// runs level to the quay's edge instead of sagging into the carve
    /// ([`bank_flags`] says why). Identical to `at` wherever no water is near.
    #[must_use]
    pub fn land_at(&self, wx: f32, wz: f32) -> f32 {
        if self.banks.is_empty() {
            return self.at(wx, wz);
        }
        self.bilinear_by(wx, wz, |i| self.land_value(i))
    }

    /// The normal of the surface [`land_at`](Self::land_at) describes.
    #[must_use]
    pub fn land_normal_at(&self, wx: f32, wz: f32) -> Vec3 {
        if self.banks.is_empty() {
            return self.normal_at(wx, wz);
        }
        let d = self.sample_m();
        if d <= 0.0 {
            return Vec3::Y;
        }
        let dx = self.land_at(wx + d, wz) - self.land_at(wx - d, wz);
        let dz = self.land_at(wx, wz + d) - self.land_at(wx, wz - d);
        Vec3::new(-dx, 2.0 * d, -dz).normalize()
    }

    /// The ground under the water and the basemap: [`at`](Self::at) with the bank
    /// sunk to the canal beside it, so a sheet at the registered level is not
    /// overlapped by the basemap's slope out of the carve — the other half of what
    /// lets the water stay level up to its wall. Never above `at`.
    #[must_use]
    pub fn sunk_at(&self, wx: f32, wz: f32) -> f32 {
        if self.banks.is_empty() {
            return self.at(wx, wz);
        }
        self.bilinear_by(wx, wz, |i| self.sunk_value(i))
    }

    /// The street-side value of sample `i`: itself if it is dry, else the mean of
    /// the dry samples within two spacings — the level the quay stands at. A sample
    /// with none (the middle of a wide canal) keeps its own, which no dry surface
    /// is drawn over anyway.
    fn land_value(&self, i: usize) -> f32 {
        if self.banks[i] == 0 {
            return self.samples[i];
        }
        let n = self.grid;
        let (x, z) = (i % n, i / n);
        let (mut sum, mut count) = (0.0f32, 0u32);
        for zz in z.saturating_sub(2)..=(z + 2).min(n - 1) {
            for xx in x.saturating_sub(2)..=(x + 2).min(n - 1) {
                let j = zz * n + xx;
                if self.banks[j] == 0 {
                    sum += self.samples[j];
                    count += 1;
                }
            }
        }
        if count == 0 {
            self.samples[i]
        } else {
            sum / count as f32
        }
    }

    /// The canal-side value of sample `i`: a bank sample takes the lowest wet sample
    /// beside it, never rising above its own.
    fn sunk_value(&self, i: usize) -> f32 {
        if self.banks[i] != BANK {
            return self.samples[i];
        }
        let n = self.grid;
        let (x, z) = (i % n, i / n);
        let mut low = self.samples[i];
        for zz in z.saturating_sub(1)..=(z + 1).min(n - 1) {
            for xx in x.saturating_sub(1)..=(x + 1).min(n - 1) {
                let j = zz * n + xx;
                if self.banks[j] == WET {
                    low = low.min(self.samples[j]);
                }
            }
        }
        low
    }

    /// [`sample_grid`](Self::sample_grid) over the base grid with each corner's value
    /// supplied by `value(index)`.
    fn bilinear_by(&self, wx: f32, wz: f32, value: impl Fn(usize) -> f32) -> f32 {
        let n = self.grid;
        if self.samples.is_empty() || n < 2 {
            return 0.0;
        }
        let span = self.half_m * 2.0;
        let (wx, wz) = (wx - self.centre.x, wz - self.centre.y);
        let fx = (wx + self.half_m) / span * (n - 1) as f32;
        let fz = (wz + self.half_m) / span * (n - 1) as f32;
        if !(0.0..=(n - 1) as f32).contains(&fx) || !(0.0..=(n - 1) as f32).contains(&fz) {
            return 0.0;
        }
        let (x0, z0) = (fx.floor() as usize, fz.floor() as usize);
        let (x1, z1) = ((x0 + 1).min(n - 1), (z0 + 1).min(n - 1));
        let (tx, tz) = (fx - x0 as f32, fz - z0 as f32);
        let s = |x: usize, z: usize| value(z * n + x);
        let a = s(x0, z0) + (s(x1, z0) - s(x0, z0)) * tx;
        let b = s(x0, z1) + (s(x1, z1) - s(x0, z1)) * tx;
        a + (b - a) * tz
    }

    /// Whether any registered water was carved into this field — without it there
    /// is no bank, and [`land_at`](Self::land_at) and [`sunk_at`](Self::sunk_at)
    /// are [`at`](Self::at).
    #[must_use]
    pub fn has_banks(&self) -> bool {
        !self.banks.is_empty()
    }

    /// This field as the ground beside the water — see [`LandField`].
    #[must_use]
    pub fn land(&self) -> LandField<'_> {
        LandField(self)
    }

    /// Every sample as [`sunk_at`](Self::sunk_at) reads it, for building the
    /// basemap's lower envelopes from. `None` when no water is registered, where it
    /// is the field itself.
    fn sunk_samples(&self) -> Option<Vec<f32>> {
        if self.banks.is_empty() {
            return None;
        }
        Some(
            (0..self.samples.len())
                .map(|i| self.sunk_value(i))
                .collect(),
        )
    }

    /// The median height over a polygon's vertices — what a *level* surface (a
    /// canal, a lake) stands at. Following the field per-vertex would tilt a water
    /// body down the valley it sits in; water is flat, and the whole point of
    /// measuring is not to invent slopes that aren't there.
    #[must_use]
    pub fn level_over(&self, points: impl IntoIterator<Item = [f32; 2]>) -> f32 {
        let mut hs: Vec<f32> = points.into_iter().map(|p| self.at(p[0], p[1])).collect();
        if hs.is_empty() {
            return 0.0;
        }
        hs.sort_by(f32::total_cmp);
        hs[hs.len() / 2]
    }
}

/// Where the drawn roads are, on the coarse field's own grid.
///
/// **A built road is graded and a survey is not.** AHN reads the kerbs, the parked
/// cars, the hedge and the return off a passing lorry; a carriageway draped
/// straight onto that rides every one of them, while the ground beside it — which
/// is refined far more coarsely — cuts the same bumps as a chord. What that looks
/// like is a ragged ridge standing along the edge of every road for its whole
/// length, and it was in this client on every platform until 2026-09-21 because
/// nothing called [`cartopolis_geo::road_grade`] at all.
///
/// It is a mask rather than the road geometry because the geometry is enormous and
/// the bake only ever asks one question of it — "is this sample road?" — and
/// because the answer accumulates: cells stream in one at a time and each marks
/// its own part of one grid that outlives them.
#[derive(Resource, Default)]
pub struct RoadMask {
    marks: Vec<bool>,
    grid: usize,
    /// The anchor generation these marks are in the frame of. Marks from before a
    /// re-anchor describe roads in a frame that has moved.
    gen: Option<u32>,
    /// Bumped whenever a cell adds marks, so the bake knows to run again. Cells
    /// that contain no road at all do not move it — most of a rural box is that,
    /// and a bump per empty cell would re-bake the box for nothing.
    pub revision: u32,
    /// Cells already marked. A cell is re-tessellated whenever the terrain revision
    /// moves, which is exactly what this feeds, so without it the mask would grow a
    /// bump per cell per bake for ever.
    marked: std::collections::HashSet<(i64, i64)>,
}

impl RoadMask {
    /// Mark one cell's road ribbons, in the anchored world-XZ frame.
    ///
    /// `origin` is the cell's north-west corner in world metres — the same origin
    /// its tile-local mesh coordinates are relative to.
    #[cfg(test)]
    pub fn mark_cell(
        &mut self,
        gen: u32,
        grid: usize,
        cell: (i64, i64),
        origin: Vec2,
        meshes: &[crate::systems::vector_tiles::SurfaceMesh],
    ) {
        self.mark_triangles(gen, grid, cell, origin, &road_triangles(meshes));
    }

    /// Mark one cell's road ribbons, given the triangles already picked out by
    /// [`road_triangles`] — which the surface worker does, so that the main thread
    /// neither walks the cell's meshes nor sees the extra triangles the terrain
    /// refinement adds to them afterwards.
    pub fn mark_triangles(
        &mut self,
        gen: u32,
        grid: usize,
        cell: (i64, i64),
        origin: Vec2,
        local: &[[[f32; 2]; 3]],
    ) {
        if grid == 0 {
            return;
        }
        if self.gen != Some(gen) || self.grid != grid {
            self.marks = vec![false; grid * grid];
            self.grid = grid;
            self.gen = Some(gen);
            self.marked.clear();
            // Not a bump: an empty mask is what the next bake would produce anyway,
            // and the bake is already re-running because the anchor moved.
        }
        if !self.marked.insert(cell) {
            return;
        }
        if local.is_empty() {
            return;
        }
        let tris: Vec<[[f32; 2]; 3]> = local
            .iter()
            .map(|t| t.map(|v| [origin.x + v[0], origin.y + v[1]]))
            .collect();
        #[allow(clippy::cast_precision_loss)]
        let step_m = TERRAIN_HALF_M * 2.0 / (grid - 1).max(1) as f32;
        // **Marking only.** The margin is applied once, to the finished mask, on
        // the bake's worker — see `road_grade::dilate_mask`. Growing here grew
        // every earlier cell again, so the mark spread a little further on every
        // cell that landed; and it did it on the main thread, six full passes over
        // a 2,049² grid per cell.
        cartopolis_geo::road_grade::mark_roads(
            &mut self.marks,
            grid,
            (-TERRAIN_HALF_M, -TERRAIN_HALF_M),
            step_m,
            &tris,
        );
        self.revision = self.revision.wrapping_add(1);
    }

    /// The marks for `gen`, or an empty mask when they belong to another frame.
    #[must_use]
    fn for_gen(&self, gen: u32, grid: usize) -> Vec<bool> {
        if self.gen == Some(gen) && self.grid == grid {
            self.marks.clone()
        } else {
            Vec::new()
        }
    }

    /// How many samples are marked — what `--dump-state` reports, and the cheap
    /// "did this do anything?" test.
    ///
    /// **The carriageway, not the ground that ends up graded.** Marking and
    /// widening were one call until the margin was found to compound across
    /// cells; the widening now happens once, inside the bake, against a copy, so
    /// this counts the road surface itself and the graded region is this plus
    /// `DEFAULT_MARGIN_SAMPLES` around it.
    ///
    /// That makes the figure **not comparable to the one recorded when the
    /// metric was introduced** — 1,927,048 of 4.2 M over Groningen — because
    /// that reading was of the compounded margin, which was the bug. Expect a
    /// much smaller number here and do not read the drop as grading having
    /// stopped working.
    #[must_use]
    pub fn marked_samples(&self) -> usize {
        self.marks.iter().filter(|m| **m).count()
    }
}

/// A cell's road-sheet triangles in its own tile-local XZ, for [`RoadMask`] —
/// shared with atlas's grading, which marks the same ones.
pub use cartopolis_geo::terrain_grade::road_triangles;

/// How long the road mask must stop growing before the field is baked again.
///
/// The mask gains a cell at a time as the streamer works through the ring, and a
/// bake per cell would be twenty bakes and twenty retears of the whole city per
/// anchor. Waiting for the burst to finish makes it one, at the cost of the roads
/// being ungraded for that first second — which is the same second the field
/// itself is missing for, and everything downstream already handles that.
const ROAD_SETTLE_S: f32 = 0.75;

/// The same, for the measured water levels.
///
/// **They arrive the same way the road marks do and were not treated the same way.**
/// `WaterLevels` bumps its revision per station as the register answers, and every
/// bump the field has not already baked is a bake — which is a retear of every cell
/// standing on the field. Measured cold over Groningen on 2026-09-21 the revision
/// went 2 → 51 during one fill, and the bake it triggered at +90 s re-tessellated
/// nine surface cells and twenty-five building cells.
///
/// Longer than the road settle because the two bursts are not the same shape: the
/// marks arrive from cells the client is already meshing, a ring's worth inside a
/// second, while the levels arrive from a register over the network and a straggler
/// two seconds behind its neighbours is ordinary. Two seconds of a flat sea, once,
/// against a rebuild of the whole ring.
const WATER_SETTLE_S: f32 = 2.0;

/// Longest the field waits for atlas's coverage manifest before choosing the
/// WCS patch instead. The manifest answers in ~50 ms from a reachable host.
const COVERAGE_WAIT_S: f32 = 5.0;

/// Longest a ready bake waits for the surface ring to finish arriving — see the
/// hold in [`update_terrain_field`].
///
/// 30 s, not the 10 it started at. At 10 the first bake went ahead with part of
/// the ring's bridge decks still unpublished, the rest landed a few seconds later,
/// and the digest moving brought a second bake — every ground and building cell
/// built three times instead of twice. Measured on the Deck (2026-09-24, Grote
/// Markt): 600 m cold 67 → 50 s, 3 km warm 48 → 41 s, one bake per fill. The
/// price is a camera that keeps a ring busy waiting up to 30 s for its hills.
const RING_HOLD_MAX_S: f32 = 30.0;

/// What [`update_terrain_field`] remembers about an anchor whose source is not
/// chosen yet.
#[derive(Default)]
pub struct UndecidedSource {
    /// When this anchor started waiting for the atlas coverage answer.
    coverage_wait: Option<(u32, f32)>,
    /// The anchor whose cells were held while its source was still undecided.
    pending_hold: Option<u32>,
}

/// Keep [`TerrainField`] in step with the map anchor: request the box once, bake it
/// when it lands, and drop it whenever the layer is off or out of coverage.
pub fn update_terrain_field(
    mut field: ResMut<TerrainField>,
    mut height: ResMut<HeightLayer>,
    stream: Res<MapStream>,
    levels: Res<crate::systems::water_levels::WaterLevels>,
    roads: Res<RoadMask>,
    decks: Res<crate::systems::map_geometry::DeckPlates>,
    surfaces: Res<crate::systems::map_geometry::MapSurfaces>,
    clock: FieldClock,
    mut settled: Local<(u32, f32)>,
    mut water_settled: Local<(u32, f32)>,
    mut deck_settled: Local<(u64, f32)>,
    // When a bake that is ready to run started waiting for the surface ring.
    mut hold: Local<Option<f32>>,
    mut tiles: ResMut<crate::systems::terrain_tiles::TerrainTiles>,
    // Which source this anchor's field comes from. Latched per anchor generation,
    // so a coverage manifest landing mid-fetch does not start a second download of
    // the same ground.
    mut source: Local<Option<(u32, FieldSource)>>,
    // What an owed bake is waiting on, logged when it changes: a field that never
    // finishes owing a bake keeps a capture from settling, and nothing else says why.
    mut waiting: Local<&'static str>,
    // When this anchor started waiting for the atlas coverage answer, and which
    // anchor's cells were held while its source was still undecided. One
    // parameter, because Bevy caps a system at sixteen.
    mut undecided: Local<UndecidedSource>,
) {
    let UndecidedSource {
        coverage_wait,
        pending_hold,
    } = &mut *undecided;
    let time = &clock.time;
    // The cells' wait for graded ground ends when it lands (`apply_coarse_terrain`),
    // when the anchor moves on, or at `GRADED_HOLD_S` — hills a little late beat a
    // map that is not drawn.
    if let Some((g, since)) = field.build_hold {
        if g != stream.anchor_generation || time.elapsed_secs() - since > GRADED_HOLD_S {
            field.build_hold = None;
        }
    }
    if field.build_hold.is_none() {
        field.release_waiters();
    }
    let Some(anchor) = stream.geo_anchor() else {
        return;
    };
    let mut wait = |why: &'static str, field: &TerrainField| {
        if *waiting != why {
            *waiting = why;
            tracing::info!(
                why,
                answered_gen = ?field.answered_gen,
                answered_water_rev = ?field.answered_water_rev,
                answered_road_rev = ?field.answered_road_rev,
                answered_deck_rev = ?field.answered_deck_rev,
                "terrain field bake waiting"
            );
        }
    };
    if !HeightLayer::covers(anchor.0, anchor.1) {
        field.clear();
        // The layer can come back on without the anchor moving, so this is not an
        // answer for the current generation — leave `answered_gen` alone and let
        // the bake run when it does.
        // Both halves of the answer, or switching the layer back on would find the
        // water half still satisfied.
        field.answered_gen = None;
        field.answered_centre = None;
        field.answered_water_rev = None;
        field.answered_road_rev = None;
        field.answered_deck_rev = None;
        field.wanted_gen = None;
        field.bake_owed = false;
        *hold = None;
        field.abandon_coarse_job();
        return;
    }
    field.wanted_gen = Some(stream.anchor_generation);
    // **Hold the cells from the moment an anchor lands, not from the moment its
    // source is chosen.** The choice waits on atlas's coverage answer, and building
    // cells were being dispatched in the frames between — on flat ground, torn down
    // and rebuilt when the graded field landed (Deck, 2026-09-26, warm at 600 m:
    // all six LoD2.2 cells built twice). Once per anchor, bounded by the same
    // `GRADED_HOLD_S`, and lifted below the moment the choice is anything but graded.
    if tiles.enabled()
        && *pending_hold != Some(stream.anchor_generation)
        && !matches!(*source, Some((g, _)) if g == stream.anchor_generation)
    {
        *pending_hold = Some(stream.anchor_generation);
        field.build_hold = Some((stream.anchor_generation, time.elapsed_secs()));
    }
    // **The roads have to stop arriving before they are worth baking in.** The mask
    // grows a cell at a time, so this waits for the burst to end rather than baking
    // per cell; `settled` is (the revision last seen, when it was last seen).
    let now = time.elapsed_secs();
    if settled.0 != roads.revision {
        *settled = (roads.revision, now);
    }
    // The water levels get the same treatment, for the same reason — see
    // [`WATER_SETTLE_S`]. The first bake is not held up by this: an empty register is
    // revision 0 and settles immediately, and a level that lands afterwards is what
    // the wait is for.
    if water_settled.0 != levels.revision {
        *water_settled = (levels.revision, now);
    }
    let water_rev = if now - water_settled.1 >= WATER_SETTLE_S {
        levels.revision
    } else {
        field.answered_water_rev.unwrap_or(0)
    };
    // The decks arrive a few at a time as each cell's heights are measured, so
    // they get the same debounce the roads and the water levels get — and for the
    // sharpest version of the same reason, since a bake retears every streamer
    // standing on the ground.
    // **The content, not the counter** — `DeckPlates::grade_digest` says why a
    // counter cannot work here.
    let deck_digest = decks.grade_digest();
    if deck_settled.0 != deck_digest {
        *deck_settled = (deck_digest, now);
    }
    let deck_rev = if now - deck_settled.1 >= ROAD_SETTLE_S {
        deck_digest
    } else {
        field.answered_deck_rev.unwrap_or(0)
    };
    let road_rev = if now - settled.1 >= ROAD_SETTLE_S {
        roads.revision
    } else {
        // Still arriving. Answer with whatever the last bake used, so this is not a
        // reason to bake — and never a reason to *skip* one the anchor or the water
        // has asked for.
        field.answered_road_rev.unwrap_or(0)
    };
    // **Graded ground owes nothing to the roads or the decks.** Atlas graded it
    // against both (`cartopolis_geo::terrain_grade`). It still owes the water: the
    // heights are carved already, but the water surfaces are drawn against the
    // levels the field hands out (`GroundPlacement`), and a field baked before they
    // arrived drew every canal at its bank.
    let graded = matches!(
        *source,
        Some((g, FieldSource::Graded(_))) if g == stream.anchor_generation
    );
    // **Graded ground follows the camera** (step 3 of `level-of-detail-tile-tree.md`).
    // A field fixed on the anchor left the ground flat three kilometres from where a
    // session started; nothing moved it but a re-anchor, and a re-anchor rebuilds
    // the world. It needs no road mask, which is anchor-centred, and a re-bake is a
    // resample — so only over graded ground, and the centre moves by whole lattice
    // steps once the camera is `FOLLOW_M` out.
    let centre_now = field.answered_centre.unwrap_or(Vec2::ZERO);
    let want_centre = match clock.camera_xz() {
        Some(cam) if graded && (cam - centre_now).abs().max_element() > FOLLOW_M => Vec2::new(
            (cam.x / FOLLOW_LATTICE_M).round() * FOLLOW_LATTICE_M,
            (cam.y / FOLLOW_LATTICE_M).round() * FOLLOW_LATTICE_M,
        ),
        _ if graded => centre_now,
        _ => Vec2::ZERO,
    };
    if field.answered_gen == Some(stream.anchor_generation)
        && field.answered_centre == Some(want_centre)
        && field.answered_water_rev == Some(water_rev)
        && (graded
            || (field.answered_road_rev == Some(road_rev)
                && field.answered_deck_rev == Some(deck_rev)))
    {
        *hold = None;
        // An input that moved and then moved back — the deck digest does exactly
        // that while a ring republishes its plates — leaves nothing to bake, and
        // the flag it raised on the way has to come down here or the field reports
        // a bake owed for ever and no capture settles (measured on the Deck,
        // 2026-09-24: every run of this branch sat at `busy="terrain"` for 180 s).
        field.bake_owed = false;
        wait("nothing owed", &field);
        return;
    }
    // Owed from here until it is queued, including while it is held below — a
    // capture must not settle on a field that is about to be replaced.
    field.bake_owed = true;
    // **A bake records what it used, not what asked for it.** The settled revisions
    // above decide *whether* to bake; the data handed to the worker below is always
    // the live mask and the live levels. Recording the settled numbers instead —
    // which is what this did for the roads — means the field reports having baked an
    // older input than it did, and bakes a second time, over identical data, the
    // moment the settle passes. One redundant bake is one redundant retear of every
    // cell in two streamers.
    let baked_water_rev = levels.revision;
    let baked_road_rev = roads.revision;
    let baked_deck_rev = deck_digest;
    // One bake at a time. A re-anchor that lands while one is out would otherwise
    // queue a second over the same worker pool, and the first is already worthless.
    if field.coarse_job.is_some() {
        wait("a bake is running", &field);
        return;
    }

    let grid = crate::quality::budget()
        .terrain_grid
        .clamp(65, TERRAIN_GRID);

    let req = field_request(anchor, TERRAIN_HALF_M, grid);
    let gen = stream.anchor_generation;
    // Atlas tiles where atlas has cut the area (`terrain_tiles` says why only
    // there), at the grid this field reads (`terrain_tiles::tile_grid`).
    let chosen = match *source {
        Some((g, s)) if g == gen => s,
        _ => {
            let eligible = tiles.enabled();
            // **Ask atlas before choosing.** The manifest is one small request that
            // starts at launch, but on a cold start the anchor lands first, and
            // latching then chose the 12 MB WCS patch over tiles atlas had already
            // cut (measured on the Deck, 2026-09-24: every cold run). Bounded, so a
            // host that never answers costs a few seconds and not the field.
            if eligible && !crate::systems::terrain_tiles::coverage_answered() {
                let now = time.elapsed_secs();
                let since = match *coverage_wait {
                    Some((g, t)) if g == gen => t,
                    _ => {
                        *coverage_wait = Some((gen, now));
                        now
                    }
                };
                if now - since < COVERAGE_WAIT_S {
                    wait("the atlas coverage", &field);
                    return;
                }
            }
            let s = if !eligible || !crate::systems::terrain_tiles::covered(anchor.0, anchor.1) {
                FieldSource::Wcs
            } else {
                match crate::systems::terrain_tiles::graded_stamp(&field_tiles(
                    anchor,
                    TERRAIN_HALF_M,
                    Vec2::ZERO,
                )) {
                    Some(stamp) => FieldSource::Graded(stamp),
                    // Raw tiles at the handset's grid too: atlas thins or cuts
                    // them (`terrain_tiles::tile_grid`).
                    None => FieldSource::Tiles,
                }
            };
            *source = Some((gen, s));
            tracing::info!(anchor_gen = gen, source = ?s, "terrain field source");
            // Once per anchor, here where the source is chosen, so an expired hold
            // is not started again. The hold taken while undecided keeps its start
            // time, so the whole wait stays inside `GRADED_HOLD_S`; any other
            // source is ground graded here, which needs the cells built first.
            if matches!(s, FieldSource::Graded(_)) {
                if !field.holds_builds(gen) && *pending_hold != Some(gen) {
                    field.build_hold = Some((gen, time.elapsed_secs()));
                }
            } else if field.holds_builds(gen) {
                field.build_hold = None;
                field.release_waiters();
            }
            s
        }
    };
    // Either source queues its fetch on the first ask and answers "not yet" until
    // it lands; the field simply stays flat for those few frames.
    let stamp = match chosen {
        FieldSource::Graded(s) => Some(s),
        FieldSource::Tiles => Some(0),
        FieldSource::Wcs => None,
    };
    // A following field is larger, at the same spacing: it has to cover the
    // building ring (`lod22::lod22_radius`, ±3.7 km) wherever inside `FOLLOW_M`
    // the camera is, or every move of the field changes the ground under the ring's
    // outer cells and they are all rebuilt (Deck, 2026-09-24: 26 of 26 at 6.4 km).
    //
    // Only once it has moved: the first field of an anchor stays the 6.4 km one,
    // because a cold start pays for every tile under it — ~30 against ~49 — and
    // most sessions never fly far enough to need the rest.
    let (half, n) = if matches!(chosen, FieldSource::Graded(_)) && want_centre != Vec2::ZERO {
        (GRADED_HALF_M, (grid - 1) * 3 / 2 + 1)
    } else {
        (TERRAIN_HALF_M, grid)
    };
    // **Graded only where every tile under *this* field is.** The source is chosen
    // once per anchor, from the first field's tiles; a field that follows the
    // camera is larger and moves, and near the edge of the graded area it reaches
    // tiles atlas has never graded — asked for graded, those answer 404 (a
    // handset capture at 53.21 N 6.53 E, 2026-09-26: eleven of them, each retried
    // before the fallback). So each request asks again, and
    // one that reaches past the graded area takes raw tiles and is graded here.
    let stamp = match stamp {
        Some(s)
            if s > 0
                && crate::systems::terrain_tiles::graded_stamp(&field_tiles(
                    anchor,
                    half,
                    want_centre,
                )) != Some(s) =>
        {
            // Ground graded here needs the cells built first.
            field.build_hold = None;
            Some(0)
        }
        other => other,
    };
    let mosaic = if let Some(stamp) = stamp {
        let want = field_tiles(anchor, half, want_centre);
        match tiles.mosaic(&want, stamp) {
            crate::systems::terrain_tiles::Mosaic::Ready(m) => Some(m),
            crate::systems::terrain_tiles::Mosaic::Waiting => {
                wait("atlas tiles", &field);
                return;
            }
            // A tile atlas would not give: graded ground falls back to the raw
            // tiles and graded here, raw tiles to WCS.
            crate::systems::terrain_tiles::Mosaic::Failed => {
                let next = if stamp > 0 {
                    FieldSource::Tiles
                } else {
                    FieldSource::Wcs
                };
                tracing::warn!(anchor_gen = gen, next = ?next, "terrain tiles failed; falling back");
                *source = Some((gen, next));
                // Ground graded here needs the cells built first.
                field.build_hold = None;
                tiles.release();
                return;
            }
        }
    } else {
        if height
            .patch_ranked(req, crate::systems::height::RANK_FIELD)
            .is_none()
        {
            wait("the WCS patch", &field);
            return;
        }
        None
    };
    // **Not while the surface ring is still arriving.** Everything the bake grades
    // against — the road outlines, the bridge decks — is published by those cells
    // as each one finishes, and the nine of a ring finish over ten seconds or more,
    // so the debounces above end in a gap between two cells rather than at the end
    // of the burst. Every bake retears every cell standing on the ground, so a bake
    // against a partial set is a whole rebuild spent on a field that is replaced a
    // few seconds later. Measured cold over Groningen (2026-09-23): three bakes per
    // fill — the field's arrival, then 8 of 9 cells' roads, then the decks — each a
    // retear of the ring.
    //
    // After the download, not before it: the 12 MB fetch is the long pole and must
    // start at once. And bounded, because a camera that keeps moving keeps a ring
    // busy, and hills a little late beat hills never.
    //
    // **And not while the water levels are still arriving**, for the same reason.
    // Once the height patches came off the device the ring finished in three
    // seconds, the first bake ran while 20 of 50 levels were in, and the rest cost a
    // second bake and a second retear of everything (Deck, 2026-09-24: warm 25 s →
    // 38 s). The debounce above cannot see this: it ends in any two-second gap.
    //
    // **Graded ground is not held for the ring**, which is the point of grading it
    // on atlas: nothing the ring publishes changes it. It is held for the water
    // levels, briefly — see `graded` above — and they come from our own host.
    let pre_graded = stamp.is_some_and(|s| s > 0);
    let held_since = *hold.get_or_insert(now);
    let bound = if pre_graded {
        GRADED_WATER_HOLD_S
    } else {
        RING_HOLD_MAX_S
    };
    if now - held_since < bound {
        if !pre_graded && surfaces.streaming_busy() {
            wait("the surface ring", &field);
            return;
        }
        if levels.loading() {
            wait("the water levels", &field);
            return;
        }
    }
    *hold = None;
    field.bake_owed = false;
    wait("queued", &field);
    let job: Box<dyn GroundSource> = match mosaic {
        // The tiles are shared, not copied; the layer forgets them and a re-bake
        // reads them back from the device cache rather than the network.
        Some(m) => {
            tiles.release();
            tracing::info!(
                anchor_gen = gen,
                measured_tiles = m.len(),
                fetched = tiles.fetched,
                "terrain field from atlas tiles"
            );
            Box::new(m)
        }
        // 17 MB cloned onto the worker, once per anchor. Holding the layer's cache
        // borrowed across a job is not an option, and keeping the patch after the
        // bake would double the field's resident cost — which is also why every
        // re-bake downloads it again, and why tiles are worth having.
        None => {
            let Some(patch) = height.patch_ranked(req, crate::systems::height::RANK_FIELD) else {
                return;
            };
            let job = patch.clone();
            height.drop_patch(&req);
            Box::new(job)
        }
    };

    // **Every bake is a retear of every streamer that stands on the ground**, so
    // which of the three inputs moved is worth naming. The field is the head of that
    // chain: a bake bumps `revision`, and `map_geometry` and `lod22` answer a bump by
    // re-tessellating their whole ring. Three bakes in a cold fill is three rings.
    tracing::info!(
        anchor_gen = gen,
        answered_gen = ?field.answered_gen,
        water_rev,
        answered_water_rev = ?field.answered_water_rev,
        road_rev,
        answered_road_rev = ?field.answered_road_rev,
        deck_rev,
        answered_deck_rev = ?field.answered_deck_rev,
        "terrain field bake queued"
    );
    let water = crate::systems::water_levels::WaterLevelField::build(&levels, anchor);
    // The grading inputs, only for ground that is graded here. Both are built on
    // the main thread at the field's full size — a 4.2 MB copy of the road mask and
    // three deck rasters beside it — and ground atlas has graded reads neither.
    let (road, ramp, deck_lift, deck_top) = if pre_graded {
        (Vec::new(), Vec::new(), Vec::new(), Vec::new())
    } else {
        // Per plate, not per sample — see `DeckPlates::grade_masks`.
        let cartopolis_geo::terrain_grade::DeckMasks { ramp, lift, top } =
            decks.grade_masks(grid, TERRAIN_HALF_M);
        (roads.for_gen(gen, grid), ramp, lift, top)
    };
    let sink = field.coarse_sink.clone();
    let datum = field.gen_datum.filter(|(g, _)| *g == gen).map(|(_, d)| d);
    field.coarse_job = Some(gen);
    // Near: the ground and building cells wait on this field (`holds_builds`), so
    // it may not queue behind the cells it is holding up.
    crate::platform::workers::submit_heavy_ranked(
        "terrain_field",
        crate::platform::workers::NEAR,
        Box::new(move || {
            let started = bevy::platform::time::Instant::now();
            let (baked, approach) = bake(
                job.as_ref(),
                anchor,
                want_centre,
                datum,
                half,
                n,
                // Graded on atlas: no carve, no fill, no grading here.
                (!pre_graded).then_some(Grading {
                    water: &water,
                    road,
                    ramp,
                    deck_lift: &deck_lift,
                    deck_top: &deck_top,
                }),
            );
            let baked_at = bevy::platform::time::Instant::now();
            let banks = if baked.is_some() {
                bank_flags(n, half, want_centre, &water)
            } else {
                Vec::new()
            };
            let banked_at = bevy::platform::time::Instant::now();
            // The snapshot, floors included, is finished here rather than where it
            // is installed: the floors are a min-filter chain over every sample,
            // and on the main thread they were a frozen frame per bake. The
            // envelopes are the basemap's, so they are built from the canal-side
            // values the basemap's finest level reads (`floor_at`) — never higher
            // than the field, so the guarantee `build_floors` states still holds.
            let baked = baked.map(|(samples, datum, carved)| {
                let mut ground = TerrainSnapshot {
                    samples: std::sync::Arc::new(samples),
                    half_m: half,
                    grid: n,
                    centre: want_centre,
                    floors: std::sync::Arc::default(),
                    banks: std::sync::Arc::new(banks),
                };
                let sample_m = half * 2.0 / (n - 1) as f32;
                let floors = match ground.sunk_samples() {
                    Some(sunk) => build_floors(&sunk, n, sample_m),
                    None => build_floors(&ground.samples, n, sample_m),
                };
                ground.floors = std::sync::Arc::new(floors);
                (ground, datum, carved)
            });
            // Every ground and building cell waits on this job, so its time is
            // start-up time: the stages say which part of it to go after.
            tracing::info!(
                grid = n,
                pre_graded,
                bake_ms = baked_at.duration_since(started).as_millis() as u64,
                banks_ms = banked_at.duration_since(baked_at).as_millis() as u64,
                floors_ms = banked_at.elapsed().as_millis() as u64,
                "terrain field bake timed"
            );
            if let Ok(mut g) = sink.lock() {
                *g = Some(CoarseBake {
                    centre: want_centre,
                    half,
                    gen,
                    grid: n,
                    water_rev: baked_water_rev,
                    road_rev: baked_road_rev,
                    deck_rev: baked_deck_rev,
                    water,
                    baked,
                    approach,
                });
            }
        }),
    );
}

/// Install a finished field bake.
///
/// Separate from the system that asks for it, and ordered before it, for the reason
/// [`apply_fine_terrain`] is: a box that lands this frame is installed this frame.
pub fn apply_coarse_terrain(mut field: ResMut<TerrainField>, stream: Res<MapStream>) {
    let Some(done) = field.coarse_sink.lock().ok().and_then(|mut g| g.take()) else {
        return;
    };
    // A result from before a re-anchor measures ground in a frame that has moved.
    if done.gen != stream.anchor_generation {
        field.coarse_job = None;
        return;
    }
    field.coarse_job = None;
    // The ground the cells were waiting for.
    field.build_hold = None;
    // Answered, whether or not there was anything there. Recording it for the no-data
    // case — all sea, over the border — is what stops a box AHN has nothing for from
    // re-baking four million samples every frame for the rest of the session; the
    // field carries a scar from exactly that bug (see `answered_gen`).
    field.answered_gen = Some(done.gen);
    field.answered_water_rev = Some(done.water_rev);
    field.answered_road_rev = Some(done.road_rev);
    field.answered_deck_rev = Some(done.deck_rev);
    field.approach_samples = done.approach;
    field.water = done.water;
    // Before the empty case, which is an answer for this centre too.
    field.answered_centre = Some(done.centre);
    let Some((ground, datum, carved)) = done.baked else {
        field.clear();
        return;
    };

    let lo = ground.samples.iter().copied().fold(f32::MAX, f32::min);
    let hi = ground.samples.iter().copied().fold(f32::MIN, f32::max);
    let sample_m = done.half * 2.0 / (done.grid - 1) as f32;
    field.ground = ground;
    field.gen_datum = Some((done.gen, datum));
    field.carved_coarse = carved;
    field.datum_m = datum;
    field.range_m = (lo, hi);
    field.revision = field.revision.wrapping_add(1);
    tracing::info!(
        datum_m = datum,
        low_m = lo,
        high_m = hi,
        sample_m,
        carved,
        centre_x = done.centre.x,
        centre_z = done.centre.y,
        "terrain field baked"
    );
}

/// The RD box to measure for a world box of `half` metres around the anchor.
///
/// Built from the world box's corners *and* edge midpoints: Mercator-to-RD is not
/// an affine map, so the extreme easting on a north edge is not necessarily at a
/// corner, and a box that merely spans the corners can miss ground the field will
/// ask about.
fn field_request(anchor: (f64, f64, u32), half: f32, grid: usize) -> HeightRequest {
    let h = half as f64;
    let mut min = [f64::MAX, f64::MAX];
    let mut max = [f64::MIN, f64::MIN];
    for wz in [-h, 0.0, h] {
        for wx in [-h, 0.0, h] {
            let (lat, lng) = world_to_lat_lng(anchor, wx, wz);
            let rd = wgs84_to_rd(lat, lng);
            min = [min[0].min(rd[0]), min[1].min(rd[1])];
            max = [max[0].max(rd[0]), max[1].max(rd[1])];
        }
    }
    HeightRequest {
        model: HeightModel::Terrain,
        min: [min[0] - REQUEST_PAD_M, min[1] - REQUEST_PAD_M],
        max: [max[0] + REQUEST_PAD_M, max[1] + REQUEST_PAD_M],
        grid: grid as u32,
    }
    .padded(0.0)
}

/// The atlas tiles under the same world box [`field_request`] measures.
fn field_tiles(anchor: (f64, f64, u32), half: f32, centre: Vec2) -> Vec<(i64, i64)> {
    let h = f64::from(half) + REQUEST_PAD_M;
    let (cx, cz) = (f64::from(centre.x), f64::from(centre.y));
    let (mut south, mut west, mut north, mut east) = (90.0f64, 180.0f64, -90.0f64, -180.0f64);
    for wz in [cz - h, cz + h] {
        for wx in [cx - h, cx + h] {
            let (lat, lng) = world_to_lat_lng(anchor, wx, wz);
            south = south.min(lat);
            north = north.max(lat);
            west = west.min(lng);
            east = east.max(lng);
        }
    }
    crate::systems::terrain_tiles::tiles_under(south, west, north, east)
}

/// Where a bake reads measured ground from: one WCS patch, or atlas tiles
/// (`terrain_tiles`). Everything after the read — carving, filling, grading — is
/// the same for both.
pub trait GroundSource: Send + Sync {
    /// Metres above NAP, or `None` where nothing was measured.
    fn height(&self, lat: f64, lng: f64) -> Option<f32>;

    /// The whole lattice at once: `out[z * lngs.len() + x]` for `(lats[z],
    /// lngs[x])`, NaN where nothing was measured. Rows are independent, so they
    /// are split across threads ([`for_rows`]).
    fn fill(&self, lats: &[f64], lngs: &[f64], out: &mut [f32]) {
        for_rows(out, lngs.len(), |z, row| {
            for (slot, &lng) in row.iter_mut().zip(lngs) {
                *slot = self.height(lats[z], lng).unwrap_or(f32::NAN);
            }
        });
    }
}

impl GroundSource for HeightPatch {
    fn height(&self, lat: f64, lng: f64) -> Option<f32> {
        // `wgs84_to_rd` is a bivariate polynomial and genuinely is per sample;
        // after the separable axes in `bake` it is the whole remaining cost.
        self.at(wgs84_to_rd(lat, lng))
    }
}

impl GroundSource for crate::systems::terrain_tiles::TileMosaic {
    fn height(&self, lat: f64, lng: f64) -> Option<f32> {
        Self::height(self, lat, lng)
    }

    fn fill(&self, lats: &[f64], lngs: &[f64], out: &mut [f32]) {
        let columns = Self::columns(lngs);
        for_rows(out, lngs.len(), |z, row| {
            self.height_row(lats[z], &columns, row)
        });
    }
}

/// Run `each(z, row)` over every `width`-long row of `grid`.
///
/// **Split across a few threads natively**, because the field's per-sample passes
/// are 4.2 million independent answers and every ground and building cell waits
/// for the bake they are part of: on the Deck a pre-graded bake — no grading at
/// all, only resampling — took 4.3–5.1 s on one thread (2026-09-26). Four
/// threads at most, for the reason `road_stitch`'s `PARALLEL_MAX_THREADS` gives:
/// this is already a job on the worker pool. Serial on wasm, which has no
/// scoped threads here.
pub(crate) fn for_rows<T: Send>(
    grid: &mut [T],
    width: usize,
    each: impl Fn(usize, &mut [T]) + Sync,
) {
    if width == 0 {
        return;
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let rows = grid.len() / width;
        let threads = std::thread::available_parallelism()
            .map_or(1, std::num::NonZeroUsize::get)
            .min(4);
        // Below a few hundred rows the threads cost more than the work.
        if threads > 1 && rows >= 256 {
            let band = rows.div_ceil(threads);
            let each = &each;
            std::thread::scope(|s| {
                for (b, chunk) in grid.chunks_mut(band * width).enumerate() {
                    s.spawn(move || {
                        for (i, row) in chunk.chunks_mut(width).enumerate() {
                            each(b * band + i, row);
                        }
                    });
                }
            });
            return;
        }
    }
    for (z, row) in grid.chunks_mut(width).enumerate() {
        each(z, row);
    }
}

/// Where this anchor's field comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldSource {
    /// One PDOK WCS patch, graded here.
    Wcs,
    /// Atlas's raw tiles, graded here.
    Tiles,
    /// Atlas's graded tiles, by the grading run with this stamp: sampled, not
    /// graded again.
    Graded(u64),
}

/// What a bake grades the ground against, when it grades it at all.
struct Grading<'a> {
    water: &'a crate::systems::water_levels::WaterLevelField,
    // **By value, so the widening mutates it in place.** `for_gen` already hands
    // the worker its own copy of the mask.
    road: Vec<bool>,
    // Where the approach roads run, and how high the structure they climb to
    // stands — `map_geometry::DeckPlates::grade_masks`. Separate from `road`
    // because a gradient travels through everything a raster corridor touches,
    // and the street passing *under* a viaduct touches its approach.
    ramp: Vec<bool>,
    deck_lift: &'a [f32],
    // The deck's measured top (NAP) where the survey has one, else NaN — what an
    // approach is graded *to* (`terrain_grade::DeckPlate::top`).
    deck_top: &'a [f32],
}

/// Resample measured ground into the world-XZ grid, fill its holes, subtract the
/// anchor's own ground and taper the edge to zero.
///
/// `None` when the source has no usable sample under the field at all.
fn bake(
    patch: &dyn GroundSource,
    anchor: (f64, f64, u32),
    // The field's centre in the anchored frame (`FOLLOW_M`).
    centre: Vec2,
    // This anchor's datum when an earlier field fixed it; `None` for the first.
    datum: Option<f32>,
    half: f32,
    n: usize,
    // `None` for ground atlas has already graded.
    grading: Option<Grading<'_>>,
) -> (Option<(Vec<f32>, f32, usize)>, (usize, usize)) {
    let started = bevy::platform::time::Instant::now();
    let step = (half * 2.0) as f64 / (n - 1) as f64;
    // Mercator is separable, so the grid's 65,536 coordinate conversions are
    // really `n` longitudes and `n` latitudes (`geo_nav::WorldFrame` says why this
    // is worth spelling out). Naively this ran the anchor's trig, a `sinh` and an
    // `atan` per *sample*, on the main thread, once per map anchor.
    let frame = WorldFrame::new(anchor);
    let axis: Vec<f64> = (0..n).map(|i| -half as f64 + i as f64 * step).collect();
    let lngs: Vec<f64> = axis
        .iter()
        .map(|&wx| frame.lng_at(f64::from(centre.x) + wx))
        .collect();
    let lats: Vec<f64> = axis
        .iter()
        .map(|&wz| frame.lat_at(f64::from(centre.y) + wz))
        .collect();

    let mut raw = vec![f32::NAN; n * n];
    patch.fill(&lats, &lngs, &mut raw);
    let resampled_at = bevy::platform::time::Instant::now();
    if !raw.iter().any(|v| v.is_finite()) {
        return (None, (0, 0));
    }

    // The datum is the ground at the anchor — the field's own centre — so the
    // world's existing `y = 0` keeps meaning "here". Falling back to the median
    // covers an anchor that happens to sit on a bridge (a hole in the DTM).
    // A field that followed the camera keeps the anchor's datum, which the first
    // field fixed.
    let middle = raw[(n / 2) * n + n / 2];
    let datum = if let Some(d) = datum {
        d
    } else if middle.is_finite() {
        middle
    } else {
        // Before the grading below, so there is nothing to report about it.
        let Some(m) = median_finite(&raw) else {
            return (None, (0, 0));
        };
        m
    };

    // Carve, fill and grade: `cartopolis_geo::terrain_grade::grade`, the same code
    // atlas grades its tiles with. The carve is the same one the fine tier gets, and
    // it matters here for a different reason — this is the field the basemap
    // clipmap is displaced by (`map_stream::apply_terrain_to_tiles`), so without it
    // the ground runs flat across a canal the water sheet has just dropped into.
    // Coarse: resampled, so a finite sample inside a ring is not evidence.
    let taper_start = if grading.is_none() {
        GRADED_TAPER_START
    } else {
        TAPER_START
    };
    let (mut carved, mut approach) = (0, (0, 0));
    if let Some(g) = grading {
        let lattice = field_lattice(n, half, centre);
        let road = if g.road.len() == n * n {
            g.road
        } else {
            Vec::new()
        };
        let graded = cartopolis_geo::terrain_grade::grade(
            &mut raw,
            lattice,
            &level_lookup(&g.water.levels_on(&lattice), lattice),
            road,
            g.ramp,
            g.deck_lift,
            g.deck_top,
        );
        carved = graded.carved;
        approach = (graded.abutments, graded.raised);
        tracing::debug!(
            abutments = graded.abutments,
            raised = graded.raised,
            "graded the field along the roads and up to the decks"
        );
    }
    let graded_at = bevy::platform::time::Instant::now();
    let mut samples = vec![0.0f32; n * n];
    for_rows(&mut samples, n, |z, row| {
        for (x, slot) in row.iter_mut().enumerate() {
            let v = raw[z * n + x];
            let h = if v.is_finite() { v - datum } else { 0.0 };
            *slot = h * edge_taper_from(x, z, n, taper_start);
        }
    });
    tracing::info!(
        resample_ms = resampled_at.duration_since(started).as_millis() as u64,
        grade_ms = graded_at.duration_since(resampled_at).as_millis() as u64,
        taper_ms = graded_at.elapsed().as_millis() as u64,
        "terrain field bake stages"
    );
    (Some((samples, datum, carved)), approach)
}

/// World-XZ → RD metres over a small box, as an affine map.
///
/// The coarse bake runs `wgs84_to_rd` per sample and says so — "genuinely is per
/// sample" — which is affordable at 65,536 samples and is not at a million. The
/// polynomial is smooth, and over a box this size it is *linear*: sampled at the centre
/// and differenced 100 m either way, the worst error over a 512 m box is **15 mm**
/// anywhere in the Netherlands (measured 2026-09-11, pinned by
/// `the_affine_rd_map_is_exact_enough_for_half_metre_samples`). Against a half-metre
/// sample spacing that is 3 % of a cell.
#[derive(Clone, Copy)]
struct RdAffine {
    origin: [f64; 2],
    per_x: [f64; 2],
    per_z: [f64; 2],
}

impl RdAffine {
    /// Fit about `centre`, a point in the anchored world-XZ frame.
    fn fit(anchor: (f64, f64, u32), centre: Vec2) -> Self {
        // Wide enough that the difference is not dominated by the polynomial's own
        // rounding, small enough to stay in the linear regime.
        const D: f64 = 100.0;
        let at = |wx: f64, wz: f64| -> [f64; 2] {
            let (lat, lng) = world_to_lat_lng(anchor, wx, wz);
            wgs84_to_rd(lat, lng)
        };
        let (cx, cz) = (f64::from(centre.x), f64::from(centre.y));
        let origin = at(cx, cz);
        let (xp, xm) = (at(cx + D, cz), at(cx - D, cz));
        let (zp, zm) = (at(cx, cz + D), at(cx, cz - D));
        Self {
            origin,
            per_x: [(xp[0] - xm[0]) / (2.0 * D), (xp[1] - xm[1]) / (2.0 * D)],
            per_z: [(zp[0] - zm[0]) / (2.0 * D), (zp[1] - zm[1]) / (2.0 * D)],
        }
    }

    /// `dx`/`dz` are metres from the centre this was fitted about.
    fn at(&self, dx: f64, dz: f64) -> [f64; 2] {
        [
            self.origin[0] + self.per_x[0] * dx + self.per_z[0] * dz,
            self.origin[1] + self.per_x[1] * dx + self.per_z[1] * dz,
        ]
    }
}

/// The RD box to ask AHN for, to cover the fine tier's world square.
///
/// Axis-aligned in RD around a square that is not, so it is a little larger than the
/// square — the two frames differ by the meridian convergence, under a degree in the
/// Netherlands. Corners *and* edge midpoints, for the reason [`field_request`] gives.
fn fine_request(anchor: (f64, f64, u32), centre: Vec2) -> HeightRequest {
    let map = RdAffine::fit(anchor, centre);
    let h = f64::from(FINE_HALF_M);
    let mut min = [f64::MAX, f64::MAX];
    let mut max = [f64::MIN, f64::MIN];
    for dz in [-h, 0.0, h] {
        for dx in [-h, 0.0, h] {
            let rd = map.at(dx, dz);
            min = [min[0].min(rd[0]), min[1].min(rd[1])];
            max = [max[0].max(rd[0]), max[1].max(rd[1])];
        }
    }
    HeightRequest {
        model: HeightModel::Terrain,
        min,
        max,
        grid: FINE_GRID as u32,
    }
}

/// Resample a measured patch into the fine tier's world-XZ grid.
///
/// Two things differ from [`bake`] and both are deliberate. The datum is **passed in**
/// rather than derived: the tier has to be expressed against the same zero as the
/// coarse field it blends into, and a tier that picked its own centre sample would step
/// by that difference at the rim. And there is no [`edge_taper`] — the fade is
/// [`TerrainSnapshot::rim_weight`], applied at lookup against the coarse answer rather
/// than baked toward flat.
///
/// `None` when the box holds no measurement at all.
fn bake_fine(
    patch: &HeightPatch,
    anchor: (f64, f64, u32),
    centre: Vec2,
    datum: f32,
    water: &crate::systems::water_levels::WaterLevelField,
) -> Option<(Vec<f32>, usize, Vec<u8>)> {
    let n = FINE_GRID;
    let map = RdAffine::fit(anchor, centre);
    let step = f64::from(FINE_HALF_M) * 2.0 / (n - 1) as f64;
    let mut raw = vec![f32::NAN; n * n];
    let mut any = false;
    for z in 0..n {
        let dz = -f64::from(FINE_HALF_M) + z as f64 * step;
        for x in 0..n {
            let dx = -f64::from(FINE_HALF_M) + x as f64 * step;
            if let Some(v) = patch.at(map.at(dx, dz)) {
                raw[z * n + x] = v;
                any = true;
            }
        }
    }
    if !any {
        return None;
    }
    // Fine: half-metre returns, so measured ground above the water is kept.
    let carved = carve_water(&mut raw, n, FINE_HALF_M, centre, water, true);
    // Unbounded: the tier is 512 m across, so "as far as it takes" is bounded by the
    // box itself, and a half-metre grid has no hole a capped reach could helpfully
    // leave open.
    fill_holes(&mut raw, n, None);
    let samples: Vec<f32> = raw
        .into_iter()
        .map(|v| if v.is_finite() { v - datum } else { 0.0 })
        .collect();
    let slope = encode_slope(&samples, n, FINE_SAMPLE_M);
    Some((samples, carved, slope))
}

/// Metres of rise per metre of run that a fully-saturated texel of the slope texture
/// stands for. A 12 cm kerb over half a metre is 0.24, so a full ±1 covers anything a
/// street has and quantises to 1/127 ≈ 0.5°. Must equal `SLOPE_RANGE` in
/// `shaders/ground_detail.wgsl`.
pub const DETAIL_SLOPE_RANGE: f32 = 1.0;

/// Texels of the fine grid the *geometry* already carries between its own vertices.
///
/// The detail texture holds a **residual**: the slope the ground has, minus the slope
/// the mesh under it is already drawn with. Without the subtraction every ground
/// gradient would be applied twice — once in the vertices and once in the normal — and
/// a hillside would shade as if it were half again as steep.
///
/// One [`MAX_GROUND_EDGE_M`] of fine samples, rounded odd so the box is centred.
const DETAIL_CARRIED_TEXELS: usize = 25;

/// Encode the fine tier's residual slope as `Rg8Snorm`, `n²` texels.
///
/// Central differences on the tier's own grid, minus a box blur of the same, so what
/// survives is exactly what a mesh with [`MAX_GROUND_EDGE_M`] edges cannot express: the
/// kerb, the camber, the quay lip, the rut. Separable, four passes over the grid, on
/// the worker that baked it.
fn encode_slope(samples: &[f32], n: usize, sample_m: f32) -> Vec<u8> {
    let mut gx = vec![0.0f32; n * n];
    let mut gz = vec![0.0f32; n * n];
    let inv = 1.0 / (2.0 * sample_m);
    for z in 0..n {
        for x in 0..n {
            let (xm, xp) = (x.saturating_sub(1), (x + 1).min(n - 1));
            let (zm, zp) = (z.saturating_sub(1), (z + 1).min(n - 1));
            gx[z * n + x] = (samples[z * n + xp] - samples[z * n + xm]) * inv;
            gz[z * n + x] = (samples[zp * n + x] - samples[zm * n + x]) * inv;
        }
    }
    let mut out = vec![0u8; n * n * 2];
    let r = DETAIL_CARRIED_TEXELS / 2;
    let k = DETAIL_CARRIED_TEXELS as f32;
    let mut lo = vec![0.0f32; n * n];
    // Per-column running sums for the vertical pass — see below for why it is not a
    // column walk.
    let mut col = vec![0.0f32; n];
    for (channel, g) in [&gx, &gz].into_iter().enumerate() {
        // **A box blur is a running sum, and this one was a re-summation.** The window
        // is `DETAIL_CARRIED_TEXELS` = 25 wide, so summing it per texel costs 25 taps
        // where sliding it costs two — and for a 1024² tier, across two channels and
        // two passes, that is ~105 M loads against ~8 M. Clamp-to-edge is what makes
        // the slide exact rather than approximate: the sample entering and the sample
        // leaving are both clamped indices, so the window is the same window the
        // re-summation formed, term for term.
        for z in 0..n {
            let row = &g[z * n..z * n + n];
            let mut sum = 0.0;
            for d in 0..DETAIL_CARRIED_TEXELS {
                sum += row[d.saturating_sub(r).min(n - 1)];
            }
            lo[z * n] = sum / k;
            for x in 1..n {
                sum += row[(x + DETAIL_CARRIED_TEXELS - 1).saturating_sub(r).min(n - 1)];
                sum -= row[(x - 1).saturating_sub(r).min(n - 1)];
                lo[z * n + x] = sum / k;
            }
        }
        // **The vertical pass walks rows, not columns.** Written the obvious way — `x`
        // outer, `z` inner — every one of the 25 taps is a separate cache line, over a
        // 4 MB array, a million times. Holding one accumulator per column and stepping
        // `z` outward makes both the add and the subtract a sequential row read.
        for (x, c) in col.iter_mut().enumerate() {
            let mut sum = 0.0;
            for d in 0..DETAIL_CARRIED_TEXELS {
                sum += lo[d.saturating_sub(r).min(n - 1) * n + x];
            }
            *c = sum;
        }
        for z in 0..n {
            if z > 0 {
                let add = (z + DETAIL_CARRIED_TEXELS - 1).saturating_sub(r).min(n - 1);
                let sub = (z - 1).saturating_sub(r).min(n - 1);
                for (x, c) in col.iter_mut().enumerate() {
                    *c += lo[add * n + x] - lo[sub * n + x];
                }
            }
            for (x, &c) in col.iter().enumerate() {
                let residual = (g[z * n + x] - c / k) / DETAIL_SLOPE_RANGE;
                out[(z * n + x) * 2 + channel] =
                    ((residual.clamp(-1.0, 1.0) * 127.0).round() as i8) as u8;
            }
        }
    }
    out
}

/// Install a finished fine-tier bake.
///
/// Separate from the system that asks for it so the ask is never blocked on the answer,
/// and ordered before it so a box that lands this frame is installed this frame.
pub fn apply_fine_terrain(mut field: ResMut<TerrainField>, stream: Res<MapStream>) {
    let Some(done) = field.fine_sink.lock().ok().and_then(|mut g| g.take()) else {
        return;
    };
    // A result from before a re-anchor measures ground in a frame that has moved.
    // Dropping it silently is right: `clear_fine` has already forgotten the ask, so the
    // next frame requests the box again in the new frame's coordinates.
    if done.gen != stream.anchor_generation {
        return;
    }
    field.fine_job = None;
    // Answered, whether or not there was anything there. Recording it for the no-data
    // case is what stops a lake or a border box from re-baking a million samples every
    // frame for the rest of the session.
    field.fine_wanted = Some(done.centre);
    field.fine_gen = Some(done.gen);
    field.fine_water_rev = Some(done.water_rev);
    let Some((samples, carved)) = done.samples else {
        field.drop_fine_samples();
        return;
    };
    field.fine_slope = done.slope;
    field.carved_fine = carved;
    field.fine = TerrainSnapshot {
        samples: std::sync::Arc::new(samples),
        half_m: FINE_HALF_M,
        grid: FINE_GRID,
        centre: done.centre,
        // No lower envelope: the tier answers point questions, and nothing builds a
        // mesh off it (see `TerrainField::ground_at`).
        floors: std::sync::Arc::default(),
        banks: std::sync::Arc::default(),
    };
    field.fine_revision = field.fine_revision.wrapping_add(1);
    tracing::debug!(
        centre_x = done.centre.x,
        centre_z = done.centre.y,
        sample_m = FINE_SAMPLE_M,
        carved,
        "fine terrain tier baked"
    );
}

/// Keep the half-metre tier under the camera.
///
/// Gated on the coarse field being ready, because the tier borrows that field's datum
/// and fades into its answer — without it there is nothing to be relative *to*.
///
/// The centre is snapped to a [`FINE_RECENTRE_M`] lattice rather than tracked against a
/// drift threshold. That is the whole hysteresis: the wanted box only changes when the
/// camera crosses a lattice line, so a camera jittering on the spot asks for nothing,
/// and a cached box is re-used exactly when you walk back onto it. The camera is never
/// more than ~68 m from the centre, leaving ≥188 m of measured ground in every
/// direction.
pub fn update_fine_terrain(
    mut field: ResMut<TerrainField>,
    mut height: ResMut<HeightLayer>,
    stream: Res<MapStream>,
    globe: Res<crate::systems::globe::GlobeState>,
    levels: Res<crate::systems::water_levels::WaterLevels>,
    camera: Query<(&big_space::prelude::GridCell, &Transform), With<FreeCamera>>,
) {
    if !field.is_ready() {
        field.clear_fine();
        return;
    }
    let Some(anchor) = stream.geo_anchor() else {
        field.clear_fine();
        return;
    };
    let Ok((cell, tf)) = camera.single() else {
        return;
    };
    // Freeze from orbit, like every other streamer. Half-metre ground is not a question
    // anything on screen can express from 60 km up, and the camera up there would drag
    // the box across a continent.
    if globe.altitude_m(cell, tf) as f32 > crate::systems::globe::GLOBE_SHOW_ALT {
        return;
    }
    let cam = Vec2::new(
        cell.x as f32 * WORLD_CELL_EDGE + tf.translation.x,
        cell.z as f32 * WORLD_CELL_EDGE + tf.translation.z,
    );
    // A re-anchor moves the origin these metres are counted from, so the tier describes
    // the wrong ground the instant it happens.
    //
    // **Only a tier or a job from another generation.** This compared against the
    // installed tier alone, which is `None` until the first bake lands — so every
    // frame in between cancelled the job it had just queued and queued another. The
    // patch download rate-limited that by accident, one retry per PDOK round trip;
    // once the patch came off the device the tier queued a bake every frame or two
    // (Deck, 2026-09-24: 27 in five seconds, 99 s of the worker pool, and the
    // basemap waited 132 s in that queue).
    let gen = stream.anchor_generation;
    if field.fine_gen.is_some_and(|g| g != gen) || field.fine_job.is_some_and(|(_, g)| g != gen) {
        field.clear_fine();
    }
    let snap = |v: f32| (v / FINE_RECENTRE_M).round() * FINE_RECENTRE_M;
    let want = Vec2::new(snap(cam.x), snap(cam.y));
    // Nothing to do while this box is the live one and the rings have not moved, and
    // nothing to do while any box is on a worker — a camera crossing lattice lines
    // faster than the bake finishes would otherwise queue one job per frame.
    if (field.fine_wanted == Some(want) && field.fine_water_rev == Some(levels.revision))
        || field.fine_job.is_some()
    {
        return;
    }
    // **Levels still arriving re-centre nothing.** Each one that lands is a new
    // revision, and this box was re-baked for every one: ~5 s of a worker each, 28
    // of them in one warm start on the Deck (2026-09-24, 143 s of the pool's time)
    // once the patch came off the device instead of from PDOK. One bake after the
    // burst takes all of them.
    if field.fine_wanted == Some(want) && levels.loading() {
        return;
    }
    if !HeightLayer::covers(anchor.0, anchor.1) {
        field.clear_fine();
        return;
    }

    let req = fine_request(anchor, want);
    // `patch` queues on the first ask and answers `None` until it lands; the tier keeps
    // the box it already has, which is why walking does not flicker back to 25 m ground
    // while the next one is in flight.
    let Some(patch) = height.patch(req) else {
        return;
    };
    // 4 MB cloned onto the worker, once per re-centre. The alternative is holding the
    // layer's cache borrowed across a job, which it cannot be.
    let job = patch.clone();
    height.drop_patch(&req);

    let datum = field.datum_m;
    let water_rev = levels.revision;
    let water = crate::systems::water_levels::WaterLevelField::build(&levels, anchor);
    let sink = field.fine_sink.clone();
    tracing::debug!(
        want_x = want.x,
        want_z = want.y,
        had = ?field.fine_wanted,
        water_rev,
        had_water_rev = ?field.fine_water_rev,
        loading = levels.loading(),
        "fine terrain bake queued"
    );
    field.fine_job = Some((want, gen));
    crate::platform::workers::submit_ranked(
        "terrain_fine",
        crate::platform::workers::NEAR,
        Box::new(move || {
            let baked = bake_fine(&job, anchor, want, datum, &water);
            let slope = baked.as_ref().map(|(_, _, s)| s.clone());
            let samples = baked.map(|(s, carved, _)| (s, carved));
            if let Ok(mut g) = sink.lock() {
                *g = Some(FineBake {
                    centre: want,
                    gen,
                    water_rev,
                    samples,
                    slope,
                });
            }
        }),
    );
}

/// Strength of the field at a grid position: 1 through the middle, easing to 0 at
/// the boundary so the measured ground meets the flat world outside it seamlessly.
#[cfg(test)]
fn edge_taper(x: usize, z: usize, n: usize) -> f32 {
    edge_taper_from(x, z, n, TAPER_START)
}

/// [`edge_taper`] starting at `start` of the half-extent.
fn edge_taper_from(x: usize, z: usize, n: usize, start: f32) -> f32 {
    let norm = |i: usize| ((i as f32 / (n - 1) as f32) * 2.0 - 1.0).abs();
    let d = norm(x).max(norm(z));
    if d <= start {
        return 1.0;
    }
    let t = ((d - start) / (1.0 - start)).clamp(0.0, 1.0);
    // Smoothstep, so the field flattens without a crease where the taper starts.
    let s = t * t * (3.0 - 2.0 * t);
    1.0 - s
}

/// Replace the ground inside a registered water body with the water's own measured
/// level, minus a bed.
///
/// **This is the whole point of the water layer, and the order it runs in is the
/// substance of it.** A canal is a hole in AHN's ground model — the laser does not come
/// back off water — and until now the next thing to touch that hole was the fill, which
/// closes it with the average of whatever surrounds it. Around a canal that is its two
/// quays, so the ground under the water was *raised to street level* and the water
/// sheet laid on top. Measured over Groningen: a two-to-five-metre error, and one that
/// looked deliberate.
///
/// So the carve goes **before** the fill, and inside a ring it takes the **lower** of
/// what is there and the water.
///
/// It used to write only into no-data, on the rule that a measurement outranks the
/// register. That rule is right at half a metre and wrong at twenty-five, and the
/// difference is what made this invisible for a day: at 0.5 m a Groningen canal is
/// 96.5 % no-data, but the coarse field asks PDOK for 25 m samples and gets the banks
/// *averaged into* the water cells, so 89 % of them carry a value and the carve skipped
/// them. Measured 2026-09-12: 379 of ~3,400 coarse samples over registered water were
/// being written, the basemap never dipped, and the water sheet dropped out of sight
/// under a ground that had not moved.
///
/// Those values are not measurements of a canal bed. AHN is a laser and water does not
/// return one — there is no ground reading under water at any resolution, only
/// interpolation bleeding in from the edges. The register is the better statement about
/// that ground, so inside a surveyed water body it wins.
///
/// `min` rather than an assignment, so the one thing a real measurement can still do is
/// say the bed is *deeper* — a dredged channel keeps its depth.
///
/// The fill still runs afterwards, and still closes the holes this did not fill — the
/// buildings, the bridge decks, the water outside the Netherlands.
fn carve_water(
    grid: &mut [f32],
    n: usize,
    half_m: f32,
    centre: Vec2,
    water: &crate::systems::water_levels::WaterLevelField,
    keep_measured_above: bool,
) -> usize {
    if water.is_empty() {
        return 0;
    }
    // The loop is `cartopolis_geo::terrain_grade::carve`, shared with atlas. What
    // `keep_measured_above` is for, measured, stays here with the tier that uses it.
    //
    // A real measurement standing **above** the water surface is not a bed, and
    // carving it drowns a structure that is really there: a quay path, a jetty, a
    // lock chamber, a bridge abutment. The laser does not come back off water at
    // all, so inside a water ring a return is a return off something solid — and
    // this runs before `fill_holes`, which is what makes "finite" mean "measured"
    // rather than "invented".
    //
    // Measured at the native half metre over the study quay block 2026-09-16,
    // sampling inside the registered water polygons: 78.3 % no-data, **21.6 %
    // measured above the surface** (5,052 m², all of it carved away before this),
    // 0.1 % measured at or below it. That last group is the specular return off the
    // water itself and is the only thing the `min` is for.
    //
    // **Only the fine tier may do this.** The coarse field is resampled at 3.125 m
    // by a service that area-averages, so a sample inside a canal is the banks
    // averaged in rather than a reading off anything — keeping those is what left
    // the basemap flat across a canal for a day, and
    // `the_carve_overwrites_resampled_ground_but_keeps_a_deeper_reading` pins it.
    let lattice = field_lattice(n, half_m, centre);
    cartopolis_geo::terrain_grade::carve(
        grid,
        lattice,
        &level_lookup(&water.levels_on(&lattice), lattice),
        keep_measured_above,
    )
}

/// The carve's `level_at`, answered from [`WaterLevelField::levels_on`] for the
/// lattice it walks. The carve only ever asks at the lattice's own points, which
/// `index_of` rounds back to their sample exactly.
///
/// [`WaterLevelField::levels_on`]: crate::systems::water_levels::WaterLevelField::levels_on
fn level_lookup(
    levels: &[f32],
    lattice: cartopolis_geo::terrain_grade::Lattice,
) -> impl Fn(f32, f32) -> Option<f32> + '_ {
    move |x, z| {
        let (ix, iz) = lattice.index_of([x, z])?;
        let v = *levels.get(iz * lattice.n + ix)?;
        v.is_finite().then_some(v)
    }
}

/// The placement of an `n`² grid spanning `±half_m` around `centre`, for the
/// shared grading code.
fn field_lattice(n: usize, half_m: f32, centre: Vec2) -> cartopolis_geo::terrain_grade::Lattice {
    cartopolis_geo::terrain_grade::Lattice {
        n,
        origin: [centre.x - half_m, centre.y - half_m],
        step: half_m * 2.0 / (n - 1) as f32,
    }
}

/// Close no-data holes out to `reach_cells` from the nearest measurement — see
/// [`cartopolis_geo::ahn::fill_holes`], which is where it lives.
///
/// It is in the shared crate because the harness draws the same ground from the
/// same service and had no filling at all: AHN's bare-earth model is *no data*
/// under every building, so an unfilled city is a field of pits, and a mesh bent
/// onto it crumples along every street. That read as a fault in the map
/// (2026-09-21) and was a fault in the instrument.
use cartopolis_geo::ahn::fill_holes;

/// Longest ground edge (m) left unsplit before displacement.
///
/// This is the resolution of the ground **you are standing on** — a city's paving,
/// roads, verges and squares are vector surfaces, and they cover the basemap tile
/// under them almost everywhere. So it, not [`TERRAIN_SAMPLE_M`], is what decides
/// whether a street has a gradient.
///
/// Four field samples per edge (12.5 m). It was 24 m, "a touch under the field's own
/// 25 m sampling" — at *that* spacing a Dutch street has a median slope under half a
/// degree and shades within ±4 % of flat: correct ground with no gradient in it (see
/// [`TERRAIN_GRID`]).
///
/// **Four and not two, and the reason is a measured upload burst.** Two samples per
/// edge (6.25 m) is what the field can actually carry and is what the finest clipmap
/// quad is, so it looked like the obvious number. Measured over Groningen 2026-09-13,
/// `--height` on against off at the same viewpoint: `upload_peak_asset_kb` went from
/// 8 MB to **37 MB**. That is one ground mesh handed to the driver in a single frame —
/// a `map_geometry` cell is a z14 tile, ~1471 m across, and a fully covered one at
/// 6.25 m is a six-figure triangle count merged into one asset. It is exactly the
/// burst `lod22::MAX_CELL_MESH_BYTES` exists to cut up, and the ground groups have no
/// equivalent: bevy's upload budget writes one *whole* asset per frame, so it cannot
/// spread a single huge one however low the ceiling is set.
///
/// So the geometry stops at 12.5 m, and everything below that belongs to a layer that
/// costs no triangles at all — the sub-metre normal from the half-metre tier (see
/// `docs/notes/terrain-relief.md`). Splitting the surface meshes by byte budget the
/// way `lod22` does would buy the finer half back, and is the thing to do first if
/// this number is ever wanted lower.
///
/// A floor, not the number itself — read it through
/// [`TerrainSnapshot::ground_edge_m`], which never goes below the field the machine
/// actually got.
pub const MAX_GROUND_EDGE_M: f32 = TERRAIN_SAMPLE_M * 4.0;

/// The refinement itself lives in `cartopolis_geo::terrain_refine`, so the
/// **harness runs the same one**. It was here until 2026-09-19, which meant
/// `carto-view` — which deliberately does not depend on this crate — bent whatever
/// triangles earcut emitted and drew metres-wide cracks between surfaces that
/// abut perfectly in the client. An instrument that manufactures the fault it is
/// pointed at is worse than no instrument.
///
/// Re-exported rather than referenced through the new path, so the ~200 call sites
/// and tests below did not have to move with it.
pub use cartopolis_geo::terrain_refine::{
    refine_for_terrain, Crease, GroundField, MIN_GROUND_EDGE_M,
};

/// The measured field, as the shared refinement asks about it. Both methods
/// already existed as inherent ones — this only names them where
/// `cartopolis_geo` can reach them.
impl GroundField for TerrainSnapshot {
    fn min_ground_edge_m(&self) -> f32 {
        TerrainSnapshot::min_ground_edge_m(self)
    }

    fn at(&self, x: f32, z: f32) -> f32 {
        TerrainSnapshot::at(self, x, z)
    }
}

/// The shared crate states these as literals, having no `TERRAIN_SAMPLE_M` to
/// derive them from. This is what keeps the derivation a checked claim rather than
/// two numbers that used to agree.
const _: () = {
    assert!(MIN_GROUND_EDGE_M == TERRAIN_SAMPLE_M * 0.5);
};

/// Lay an existing mesh over the measured ground: refine its long edges, then lift
/// every vertex by the field under it.
///
/// `origin` is the world-XZ point the mesh's local coordinates are relative to. A
/// no-op when the field is absent, so a caller can apply it unconditionally.
///
/// It **adds** to whatever height the mesh already carries rather than assigning
/// one, which is what lets the route line arrive with a bridge deck already baked
/// into its vertices and still follow the ground the bridge stands on. A deck height
/// cannot be a field sampled here: a road on a viaduct and the road beneath it have
/// the same world XZ, so no function of position can tell them apart — see
/// `navigation::route_lift`.
pub fn drape_mesh(mesh: &mut Mesh, origin: Vec2, field: &TerrainSnapshot) {
    use bevy::render::mesh::{Indices, VertexAttributeValues};

    if !field.is_ready() {
        return;
    }

    let Some(VertexAttributeValues::Float32x3(pos)) = mesh.attribute(Mesh::ATTRIBUTE_POSITION)
    else {
        return;
    };
    let mut positions = pos.clone();
    let mut indices: Vec<u32> = match mesh.indices() {
        Some(Indices::U32(i)) => i.clone(),
        Some(Indices::U16(i)) => i.iter().map(|v| u32::from(*v)).collect(),
        None => return,
    };
    let had_normals = mesh.attribute(Mesh::ATTRIBUTE_NORMAL).is_some();
    refine_for_terrain(
        &mut positions,
        &mut indices,
        &mut Vec::new(),
        &mut Vec::new(),
        &mut Vec::new(),
        &mut Vec::new(),
        field.ground_edge_m(),
        Some(Crease {
            field,
            origin: origin.to_array(),
        }),
    );
    for p in positions.iter_mut() {
        p[1] += field.at(origin.x + p[0], origin.y + p[2]);
    }
    if had_normals {
        let normals: Vec<[f32; 3]> = positions
            .iter()
            .map(|p| field.normal_at(origin.x + p[0], origin.y + p[2]).to_array())
            .collect();
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    }
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_indices(Indices::U32(indices));
}

/// Build a ground quad of `size` metres centred on `centre` (world XZ) as a
/// `divisions`×`divisions` grid lifted onto the field.
///
/// Positions are **local to the quad's centre**, so this drops straight into the
/// entity transform the flat tile already had. UVs match the `+Y` face of the
/// `Cuboid` this replaces (u east, v south, 0–1 across the tile) — a tile whose
/// imagery is mirrored against its neighbours is what a mismatch looks like.
///
/// Normals come from the field's own slope rather than from the triangles: the
/// field is what every other consumer stands on, and shading the ground with the
/// gradient of the *same* function is what keeps a lit hillside agreeing with the
/// road running over it.
///
/// Both the height and the normal come through [`TerrainSnapshot::floor_at`] at this
/// grid's own quad size, so a level whose quads are coarser than the field draws a
/// surface that is everywhere at or under the measured ground — which is what allows
/// a distant level to be coarse at all (see [`build_floors`]). At the finest levels
/// the quad is the sample and the floor is the measurement.
/// `lid` answers, for a world XZ, how far the ground there is lifted back over a
/// tunnel's lid and how much of a lid it is (0..1, for the shading) — the same
/// fill the vector ground takes (`tunnels::Lidded`). **Without it the basemap under
/// a lid stayed at the surveyed trench floor**, which is *inside* the tunnel: from a
/// cutting, every mouth was filled by a sloping sheet of map (2026-09-23).
///
/// `openings` are the tunnel openings the sheet has to be missing over, in this
/// grid's own local XZ. Cutting here rather than at the call site is what lets a
/// tile that needs no relief and no curve still get a hole in it: it is the only
/// place a clipmap tile's triangles exist.
pub fn displaced_grid(
    field: &TerrainField,
    globe: &crate::systems::globe::GlobeState,
    centre: Vec2,
    size: f32,
    divisions: usize,
    openings: &[Vec<[f32; 2]>],
    lid: &dyn Fn(f32, f32) -> (f32, f32),
) -> Mesh {
    use bevy::asset::RenderAssetUsages;
    use bevy::render::mesh::{Indices, PrimitiveTopology};

    let n = divisions.max(1) + 1;
    let step = size / divisions.max(1) as f32;
    let half = size * 0.5;
    // The tile's transform already carries the drop at its own centre, so the
    // mesh only holds the *difference* across itself. Two things follow and both
    // are the point: the numbers stay small enough for f32 80 km out, where the
    // absolute drop is 528 m and a vertex offset is centimetres; and every tile
    // evaluates the same global `drop_at`, so neighbours agree exactly on their
    // shared edge and the ring has no cracks in it.
    let centre_drop = globe.drop_at(centre.length());
    let mut positions = Vec::with_capacity(n * n);
    let mut normals = Vec::with_capacity(n * n);
    let mut uvs = Vec::with_capacity(n * n);
    for j in 0..n {
        let lz = -half + j as f32 * step;
        for i in 0..n {
            let lx = -half + i as f32 * step;
            let (wx, wz) = (centre.x + lx, centre.y + lz);
            let curve = globe.drop_at(Vec2::new(wx, wz).length()) - centre_drop;
            let (lift, lidded) = lid(wx, wz);
            positions.push([lx, field.floor_at(wx, wz, step) + lift - curve, lz]);
            // Relief tilts the ground against the surface; curvature tilts the
            // surface itself. Composed by rotating the field's normal into the
            // sphere's frame — outside the field `normal_at` is +Y, so this
            // degrades to the plain sphere normal with no special case.
            let up = globe.surface_normal(wx, wz);
            let n = Quat::from_rotation_arc(Vec3::Y, up)
                * field
                    .floor_normal_at(wx, wz, step)
                    .lerp(Vec3::Y, lidded)
                    .normalize_or(Vec3::Y);
            normals.push(n.to_array());
            uvs.push([(lx + half) / size, (lz + half) / size]);
        }
    }
    let mut indices = Vec::with_capacity(divisions * divisions * 6);
    for j in 0..n - 1 {
        for i in 0..n - 1 {
            let (a, b) = ((j * n + i) as u32, (j * n + i + 1) as u32);
            let (c, d) = (((j + 1) * n + i) as u32, ((j + 1) * n + i + 1) as u32);
            // Wound to face up, matching `Cuboid`'s top face — a downward-wound
            // ground is backface-culled and reads as a hole, silently.
            indices.extend_from_slice(&[b, a, c, c, d, b]);
        }
    }
    // `RENDER_WORLD` only: the relief grid is regenerated from the field whenever
    // the revision moves, so the vertex buffer is never a source of truth anyone
    // reads back — keeping the main-world copy `default()` implies just doubles the
    // resident cost of every clipmap level. `map_stream` spawns the tile in the
    // same frame it adds this mesh, so the auto-`Aabb` pass still sees the
    // positions; a handle stashed for later reuse could not do this (that is why
    // `street_labels`' shared quad keeps `MAIN_WORLD`).
    let (positions, normals, uvs, indices) = if openings.is_empty() {
        (positions, normals, uvs, indices)
    } else {
        let mut sheet = cartopolis_geo::vector_tiles::SurfaceMesh {
            group: cartopolis_geo::vector_tiles::SurfaceGroup::Built,
            positions,
            normals,
            uvs,
            indices,
            colors: Vec::new(),
            rungs: Vec::new(),
        };
        cartopolis_geo::tunnels::cut_sheet(&mut sheet, openings);
        (sheet.positions, sheet.normals, sheet.uvs, sheet.indices)
    };
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
    .with_inserted_indices(Indices::U32(indices))
}

fn median_finite(grid: &[f32]) -> Option<f32> {
    let mut v: Vec<f32> = grid.iter().copied().filter(|s| s.is_finite()).collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(f32::total_cmp);
    Some(v[v.len() / 2])
}

#[cfg(test)]
mod tests {
    use super::*;
    use cartopolis_geo::ahn::FILL_REACH_M;

    /// Every row is visited once, with its own index, whether the grid is big
    /// enough to be split across threads or not — including a last band that is
    /// shorter than the others.
    #[test]
    fn for_rows_visits_every_row_once_with_its_index() {
        for (rows, width) in [(3, 5), (257, 3), (1001, 7), (2049, 2)] {
            let mut grid = vec![usize::MAX; rows * width];
            for_rows(&mut grid, width, |z, row| {
                for (x, slot) in row.iter_mut().enumerate() {
                    assert_eq!(*slot, usize::MAX, "row {z} visited twice");
                    *slot = z * width + x;
                }
            });
            assert!(
                grid.iter().enumerate().all(|(i, &v)| v == i),
                "{rows}x{width}"
            );
        }
    }

    /// A field of `n`² samples centred at `centre`, each the height of a fixed
    /// function at its own world place — what two bakes over the same tiles give.
    fn field_at(centre: Vec2, n: usize, bump: Option<Vec2>) -> TerrainSnapshot {
        let half = TERRAIN_HALF_M;
        let step = half * 2.0 / (n - 1) as f32;
        let mut samples = Vec::with_capacity(n * n);
        for z in 0..n {
            for x in 0..n {
                let (wx, wz) = (
                    centre.x - half + x as f32 * step,
                    centre.y - half + z as f32 * step,
                );
                let mut h = (wx * 0.013).sin() * 3.0 + (wz * 0.007).cos() * 2.0;
                if bump.is_some_and(|b| (b.x - wx).abs() < 60.0 && (b.y - wz).abs() < 60.0) {
                    h += 1.0;
                }
                samples.push(h);
            }
        }
        TerrainSnapshot {
            samples: std::sync::Arc::new(samples),
            half_m: half,
            grid: n,
            centre,
            floors: std::sync::Arc::default(),
            banks: std::sync::Arc::default(),
        }
    }

    #[test]
    fn a_field_that_moved_by_whole_samples_agrees_about_the_ground_it_shares() {
        let (lo, hi) = (Vec2::new(0.0, 0.0), Vec2::new(1_400.0, 1_400.0));
        let a = field_at(Vec2::ZERO, 129, None);
        let b = field_at(Vec2::new(FOLLOW_LATTICE_M, -FOLLOW_LATTICE_M), 129, None);
        assert_eq!(a.digest_over(lo, hi), b.digest_over(lo, hi));
        // …and not about ground that is different.
        let c = field_at(
            Vec2::new(FOLLOW_LATTICE_M, 0.0),
            129,
            Some(Vec2::new(700.0, 700.0)),
        );
        assert_ne!(a.digest_over(lo, hi), c.digest_over(lo, hi));
    }

    /// Build a coarse-shaped snapshot out of a height function, for the refinement
    /// tests below. Small on purpose — 6.4 km at the real spacing is four million
    /// samples and these tests care about one wall.
    /// A canal 20 m wide running north–south through a street at +2 m, carved to
    /// a −3.5 m bed, on a 3.125 m field — the desktop's spacing.
    fn canal() -> (
        TerrainSnapshot,
        crate::systems::water_levels::WaterLevelField,
    ) {
        let water = crate::systems::water_levels::WaterLevelField::build_for_test(
            -3.0,
            &[
                [-10.0, -300.0],
                [10.0, -300.0],
                [10.0, 300.0],
                [-10.0, 300.0],
            ],
        );
        let mut field = snapshot_of(100.0, 3.125, |x, _| if x.abs() < 10.0 { -3.5 } else { 2.0 });
        field.banks = Arc::new(bank_flags(field.grid, field.half_m, field.centre, &water));
        (field, water)
    }

    /// The street reads dry ground right up to the quay, the canal side reads the
    /// bed right up to it, and both are the field itself away from the water.
    #[test]
    fn the_street_is_level_to_the_quay_and_the_canal_is_level_to_its_wall() {
        let (field, _) = canal();
        // Between the last sample in the canal (x = 9.375) and the first on the
        // street (12.5) the carved field is a ramp — the sag this exists to remove.
        assert!(field.at(10.5, 0.0) < 0.0, "the carve reaches past the quay");
        for x in [10.0, 10.5, 12.0, 14.0] {
            assert!(
                (field.land_at(x, 0.0) - 2.0).abs() < 1e-4,
                "street at x = {x}: {}",
                field.land_at(x, 0.0)
            );
        }
        // At the drawn edge the canal side is still the bed, so a sheet at the
        // registered level is clear of the basemap all the way to its wall.
        assert!((field.sunk_at(10.0, 0.0) + 3.5).abs() < 1e-4);
        for x in [-60.0, 40.0, 90.0] {
            assert_eq!(field.land_at(x, 5.0), field.at(x, 5.0));
            assert_eq!(field.sunk_at(x, 5.0), field.at(x, 5.0));
        }
        let mut x = -99.0;
        while x < 99.0 {
            assert!(
                field.sunk_at(x, 1.0) <= field.at(x, 1.0) + 1e-5,
                "sunk above at x = {x}"
            );
            x += 0.7;
        }
    }

    /// With nothing registered, both readings are the field to the bit.
    #[test]
    fn without_water_the_street_and_the_canal_side_are_the_field() {
        let field = snapshot_of(50.0, 3.125, |x, z| (x * 0.1).sin() + z * 0.02);
        assert!(field.banks.is_empty());
        for (x, z) in [(0.3, 1.7), (-20.0, 33.3), (41.0, -12.5)] {
            assert_eq!(field.land_at(x, z), field.at(x, z));
            assert_eq!(field.sunk_at(x, z), field.at(x, z));
        }
    }

    fn snapshot_of(half: f32, sample: f32, height: impl Fn(f32, f32) -> f32) -> TerrainSnapshot {
        let n = (half * 2.0 / sample) as usize + 1;
        let mut samples = Vec::with_capacity(n * n);
        for zi in 0..n {
            for xi in 0..n {
                samples.push(height(
                    -half + xi as f32 * sample,
                    -half + zi as f32 * sample,
                ));
            }
        }
        let floors = build_floors(&samples, n, sample);
        TerrainSnapshot {
            samples: std::sync::Arc::new(samples),
            half_m: half,
            grid: n,
            centre: Vec2::ZERO,
            floors: std::sync::Arc::new(floors),
            banks: std::sync::Arc::default(),
        }
    }

    /// One 12.5 m quad, refined, and how far the drawn surface ends up from the
    /// measured ground under it.
    fn worst_error(field: &TerrainSnapshot, crease: bool) -> (f32, usize) {
        // A single quad straddling the step, in the frame the field is in.
        let mut positions = vec![
            [-6.25, 0.0, -6.25],
            [6.25, 0.0, -6.25],
            [6.25, 0.0, 6.25],
            [-6.25, 0.0, 6.25],
        ];
        let mut indices = vec![0, 1, 2, 0, 2, 3];
        refine_for_terrain(
            &mut positions,
            &mut indices,
            &mut vec![],
            &mut vec![],
            &mut vec![],
            &mut vec![],
            MAX_GROUND_EDGE_M,
            crease.then_some(Crease {
                field,
                origin: [0.0, 0.0],
            }),
        );
        for p in positions.iter_mut() {
            p[1] = field.at(p[0], p[2]);
        }
        // Sample the drawn surface at the centre of every triangle — that is where a
        // flat chord is furthest from a step it straddles — and compare with the
        // ground there.
        let mut worst = 0.0f32;
        for tri in indices.chunks_exact(3) {
            let (a, b, c) = (
                positions[tri[0] as usize],
                positions[tri[1] as usize],
                positions[tri[2] as usize],
            );
            let cx = (a[0] + b[0] + c[0]) / 3.0;
            let cz = (a[2] + b[2] + c[2]) / 3.0;
            let drawn = (a[1] + b[1] + c[1]) / 3.0;
            worst = worst.max((drawn - field.at(cx, cz)).abs());
        }
        (worst, indices.len() / 3)
    }

    /// A square of ground as an `n`×`n` grid of quads, refined and lifted onto the
    /// field. `unshared` is the layout `flat_fill` and `road_stitch` emit: three
    /// fresh vertices per triangle, no sharing anywhere.
    ///
    /// A grid rather than one quad because the thing under test is what happens to
    /// an **interior** edge — the edge two triangles own between them — and a single
    /// quad has exactly one of those.
    fn drawn_grid(
        field: &TerrainSnapshot,
        half: f32,
        n: usize,
        unshared: bool,
    ) -> (Vec<[f32; 3]>, Vec<u32>) {
        let step = half * 2.0 / n as f32;
        let corner = |i: usize, j: usize| [-half + i as f32 * step, 0.0, -half + j as f32 * step];
        let (mut positions, mut indices) = (Vec::new(), Vec::new());
        if unshared {
            for j in 0..n {
                for i in 0..n {
                    let (a, b, c, d) = (
                        corner(i, j),
                        corner(i + 1, j),
                        corner(i + 1, j + 1),
                        corner(i, j + 1),
                    );
                    let base = positions.len() as u32;
                    positions.extend_from_slice(&[a, b, c, a, c, d]);
                    indices.extend((0..6).map(|k| base + k));
                }
            }
        } else {
            for j in 0..=n {
                for i in 0..=n {
                    positions.push(corner(i, j));
                }
            }
            let at = |i: usize, j: usize| (j * (n + 1) + i) as u32;
            for j in 0..n {
                for i in 0..n {
                    indices.extend_from_slice(&[at(i, j), at(i + 1, j), at(i + 1, j + 1)]);
                    indices.extend_from_slice(&[at(i, j), at(i + 1, j + 1), at(i, j + 1)]);
                }
            }
        }
        refine_for_terrain(
            &mut positions,
            &mut indices,
            &mut vec![],
            &mut vec![],
            &mut vec![],
            &mut vec![],
            MAX_GROUND_EDGE_M,
            Some(Crease {
                field,
                origin: [0.0, 0.0],
            }),
        );
        for p in positions.iter_mut() {
            p[1] = field.at(p[0], p[2]);
        }
        (positions, indices)
    }

    /// The height of a triangulated surface at one plan position, or `None` off it.
    fn surface_at(positions: &[[f32; 3]], indices: &[u32], x: f32, z: f32) -> Option<f32> {
        for tri in indices.chunks_exact(3) {
            let (a, b, c) = (
                positions[tri[0] as usize],
                positions[tri[1] as usize],
                positions[tri[2] as usize],
            );
            let det = (b[2] - c[2]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[2] - c[2]);
            if det.abs() < 1e-12 {
                continue;
            }
            let l0 = ((b[2] - c[2]) * (x - c[0]) + (c[0] - b[0]) * (z - c[2])) / det;
            let l1 = ((c[2] - a[2]) * (x - c[0]) + (a[0] - c[0]) * (z - c[2])) / det;
            let l2 = 1.0 - l0 - l1;
            if l0 >= -1e-4 && l1 >= -1e-4 && l2 >= -1e-4 {
                return Some(l0 * a[1] + l1 * b[1] + l2 * c[1]);
            }
        }
        None
    }

    /// **A vertex layout may not change the shape of the ground.**
    ///
    /// `flat_fill` gives the surveyed paving and the crop parcels three fresh
    /// vertices per triangle, and `road_stitch` re-emits every ribbon its junction
    /// cut touched the same way. Before 2026-09-18 the refinement keyed its split
    /// decision on the vertex index pair, so an interior edge of one of those meshes
    /// was two separate edges: tested twice, charged to `CREASE_VERTEX_BUDGET`
    /// twice, and answerable differently for its two owners. A sheet drawn with
    /// unshared vertices therefore came out a different shape from the identical
    /// sheet drawn with shared ones — which is half of why the paving and the
    /// carriageway tore through each other on a slope, and it is invisible to every
    /// test that refines one mesh and measures it against the field.
    ///
    /// **The budget is what makes this visible, so the test has to spend it.** With
    /// room to spare, both owners of a doubly-charged edge reach the same answer and
    /// the two layouts agree; the divergence is the moment `CREASE_VERTEX_BUDGET`
    /// runs out *between* them, after which one owner has a midpoint and the other
    /// does not. An unshared sheet also burns the budget at twice the rate, so it
    /// stops refining sooner and over different ground — the measured "about a
    /// quarter of a dense cell's paving gets no crease refinement, in an arbitrary
    /// spatial order".
    #[test]
    fn a_sheet_draws_the_same_ground_whether_or_not_its_vertices_are_shared() {
        // Ground that creases at every sample, so every edge has sag and the budget
        // is the only thing that can stop the refinement.
        let field = snapshot_of(512.0, TERRAIN_SAMPLE_M, |x, z| {
            ((x / TERRAIN_SAMPLE_M) as i32 % 2) as f32 * 1.5
                + ((z / TERRAIN_SAMPLE_M) as i32 % 2) as f32 * 1.5
        });
        let (half, n) = (256.0f32, 96);
        let (sp, si) = drawn_grid(&field, half, n, false);
        let (up, ui) = drawn_grid(&field, half, n, true);

        let mut worst = 0.0f32;
        let mut compared = 0;
        for i in 0..=120 {
            for j in 0..=120 {
                let x = -half + 0.5 + i as f32 * (half * 2.0 - 1.0) / 120.0;
                let z = -half + 0.5 + j as f32 * (half * 2.0 - 1.0) / 120.0;
                if let (Some(a), Some(b)) = (surface_at(&sp, &si, x, z), surface_at(&up, &ui, x, z))
                {
                    compared += 1;
                    worst = worst.max((a - b).abs());
                }
            }
        }
        assert!(compared > 10_000, "sampled too little of the square");
        assert!(
            worst < 1e-4,
            "the same ground came out {worst} m apart depending on whether the \
             vertices were shared, over {compared} sampled points"
        );
    }

    /// And the refined mesh may not contain a T-junction, whatever the layout.
    ///
    /// A vertex lying in the middle of another triangle's edge is a crack you can
    /// see the sky through once the ground tilts. The per-edge design prevents it by
    /// construction; keying the decision on the index pair quietly reintroduced it
    /// for the unshared meshes, because the two owners of one geometric edge could
    /// be answered differently.
    #[test]
    fn refining_unshared_vertices_leaves_no_t_junction() {
        let field = snapshot_of(512.0, TERRAIN_SAMPLE_M, |x, z| {
            ((x / TERRAIN_SAMPLE_M) as i32 % 2) as f32 * 1.5
                + ((z / TERRAIN_SAMPLE_M) as i32 % 2) as f32 * 1.5
        });
        let (positions, indices) = drawn_grid(&field, 48.0, 16, true);
        for tri in indices.chunks_exact(3) {
            for k in 0..3 {
                let (a, b) = (
                    positions[tri[k] as usize],
                    positions[tri[(k + 1) % 3] as usize],
                );
                for p in &positions {
                    let (dx, dz) = (b[0] - a[0], b[2] - a[2]);
                    let len_sq = dx * dx + dz * dz;
                    if len_sq < 1e-9 {
                        continue;
                    }
                    let t = ((p[0] - a[0]) * dx + (p[2] - a[2]) * dz) / len_sq;
                    if !(0.02..=0.98).contains(&t) {
                        continue;
                    }
                    let off = (p[0] - (a[0] + dx * t)).hypot(p[2] - (a[2] + dz * t));
                    assert!(
                        off > 1e-3,
                        "vertex {p:?} sits inside the edge {a:?}–{b:?}: a T-junction"
                    );
                }
            }
        }
    }

    /// The whole point of the crease rule, as a number.
    ///
    /// A 1.7 m quay wall inside a 12.5 m quad: the long-edge rule has nothing to say
    /// (the quad is already at the limit), so the chord averages straight across the
    /// step and the wall is drawn as a ramp. The crease rule splits it because the
    /// ground under the midpoint is nowhere near the chord.
    #[test]
    fn a_step_inside_one_quad_is_refined_only_by_the_crease_rule() {
        // A wall running north-south at x = 1.2 m, 1.7 m high — the study pond
        // quay wall's measured face, at a spacing the coarse field can carry.
        let field = snapshot_of(
            64.0,
            TERRAIN_SAMPLE_M,
            |x, _z| if x > 1.2 { 1.7 } else { 0.0 },
        );

        let (flat_err, flat_tris) = worst_error(&field, false);
        let (crease_err, crease_tris) = worst_error(&field, true);

        assert!(
            flat_err > 0.5,
            "without the crease rule the step should be badly averaged, got {flat_err} m"
        );
        assert!(
            crease_err < flat_err * 0.5,
            "the crease rule must at least halve the error: {flat_err} m -> {crease_err} m"
        );
        assert!(
            crease_tris > flat_tris,
            "and it pays for it in triangles: {flat_tris} -> {crease_tris}"
        );
    }

    /// And it must cost nothing where there is nothing to resolve, or every flat
    /// Dutch cell — which is most of them — pays for a wall it does not have.
    #[test]
    fn flat_ground_is_not_refined_by_the_crease_rule() {
        let level = snapshot_of(64.0, TERRAIN_SAMPLE_M, |_, _| 0.0);
        assert_eq!(
            worst_error(&level, true).1,
            worst_error(&level, false).1,
            "level ground must produce the same mesh with the crease rule on"
        );
        // A gentle even slope is not a crease either: a chord across a plane is the
        // plane. This is the case that separates "splits where the ground is not
        // flat" from "splits where the ground is not *straight*".
        let ramp = snapshot_of(64.0, TERRAIN_SAMPLE_M, |x, _| x * 0.10);
        assert_eq!(
            worst_error(&ramp, true).1,
            worst_error(&ramp, false).1,
            "an even 10 % slope has no sag and must not be refined"
        );
    }

    /// The bound that keeps a crease-everywhere cell from reintroducing the upload
    /// burst `MAX_GROUND_EDGE_M` exists to avoid.
    #[test]
    fn the_crease_rule_stops_at_its_budget() {
        // Ground that is nothing but steps, so every edge at every level has sag.
        let field = snapshot_of(512.0, TERRAIN_SAMPLE_M, |x, z| {
            ((x / TERRAIN_SAMPLE_M) as i32 % 2) as f32 * 1.5
                + ((z / TERRAIN_SAMPLE_M) as i32 % 2) as f32 * 1.5
        });
        let mut positions = vec![
            [-512.0, 0.0, -512.0],
            [512.0, 0.0, -512.0],
            [512.0, 0.0, 512.0],
            [-512.0, 0.0, 512.0],
        ];
        let mut indices = vec![0, 1, 2, 0, 2, 3];
        let before = positions.len();
        refine_for_terrain(
            &mut positions,
            &mut indices,
            &mut vec![],
            &mut vec![],
            &mut vec![],
            &mut vec![],
            MAX_GROUND_EDGE_M,
            Some(Crease {
                field: &field,
                origin: [0.0, 0.0],
            }),
        );
        // The long-edge rule is unbounded by design (it is bounded by the polygon),
        // so the assertion is on the crease rule's share: it may not add more than
        // its budget on top of whatever the long-edge rule asked for.
        let added = positions.len() - before;
        assert!(
            added <= cartopolis_geo::terrain_refine::CREASE_VERTEX_BUDGET * 2,
            "the crease rule ran away: {added} vertices added"
        );
        // And it is deterministic: the same cell rebuilt must be the same mesh, or
        // the ground twitches every time a cell is retorn.
        let mut again = vec![
            [-512.0, 0.0, -512.0],
            [512.0, 0.0, -512.0],
            [512.0, 0.0, 512.0],
            [-512.0, 0.0, 512.0],
        ];
        let mut again_idx = vec![0, 1, 2, 0, 2, 3];
        refine_for_terrain(
            &mut again,
            &mut again_idx,
            &mut vec![],
            &mut vec![],
            &mut vec![],
            &mut vec![],
            MAX_GROUND_EDGE_M,
            Some(Crease {
                field: &field,
                origin: [0.0, 0.0],
            }),
        );
        assert_eq!(again_idx, indices, "a rebuilt cell must be the same mesh");
    }

    /// The fine bake replaces a million `wgs84_to_rd` evaluations with one affine map
    /// fitted about the box centre. That is only allowed while the error stays far
    /// below the sample spacing it is addressing — so measure it, at four corners of
    /// the country, rather than trusting that a polynomial "is smooth".
    ///
    /// A failure here does not look like a failure: every sample would land a little
    /// off the ground it claims to measure, which reads as a slightly wrong height and
    /// nothing else.
    /// **The harness must resolve the ground as finely as this does.**
    ///
    /// `carto-view` sizes its one AHN request off `cartopolis_geo::ahn::SAMPLE_M`,
    /// and a harness on a coarser raster does not draw a smoother version of the
    /// same city — it draws a different one. Asked for 5.73 m where this bakes
    /// 3.125 m, it aliased the Groningen ring's trench into folds and creases the
    /// client does not have, and those read as a fault in the map (2026-09-21).
    ///
    /// So the two numbers are one number. Change the box or the grid here and this
    /// fails until the shared constant is changed with it.
    #[test]
    fn the_harness_resolves_the_ground_as_finely_as_the_client() {
        assert!(
            (TERRAIN_SAMPLE_M - cartopolis_geo::ahn::SAMPLE_M).abs() < 1e-6,
            "the client bakes {TERRAIN_SAMPLE_M} m and the harness asks for {} m",
            cartopolis_geo::ahn::SAMPLE_M
        );
    }

    #[test]
    fn the_affine_rd_map_is_exact_enough_for_half_metre_samples() {
        // Comfortably under FINE_SAMPLE_M (~0.5 m); measured worst case is ~15 mm.
        const TOLERANCE_M: f64 = 0.1;
        for anchor in [
            (53.2194, 6.5665, 14u32),
            (52.0907, 5.1214, 14),
            (51.4400, 5.4800, 14),
            (53.4400, 6.9000, 14),
        ] {
            let centre = Vec2::new(1_500.0, -900.0);
            let map = RdAffine::fit(anchor, centre);
            let mut worst: f64 = 0.0;
            for dz in [-FINE_HALF_M, -128.0, 0.0, 128.0, FINE_HALF_M] {
                for dx in [-FINE_HALF_M, -128.0, 0.0, 128.0, FINE_HALF_M] {
                    let (lat, lng) = world_to_lat_lng(
                        anchor,
                        f64::from(centre.x) + f64::from(dx),
                        f64::from(centre.y) + f64::from(dz),
                    );
                    let exact = wgs84_to_rd(lat, lng);
                    let approx = map.at(f64::from(dx), f64::from(dz));
                    worst = worst.max((exact[0] - approx[0]).hypot(exact[1] - approx[1]));
                }
            }
            assert!(
                worst < TOLERANCE_M,
                "affine RD map is {worst:.3} m off at {anchor:?}; the fine tier samples \
                 every {FINE_SAMPLE_M} m, so this is no longer a shortcut"
            );
        }
    }

    /// The sliding window must be the same window the re-summation formed.
    ///
    /// `encode_slope`'s box blur went from 25 taps a texel to two, which is a rewrite
    /// of the arithmetic and not just of the loop — clamp-to-edge at both ends, a
    /// running sum that survives 1024 add/subtract steps, and a vertical pass that
    /// walks rows while holding one accumulator per column. Any of those getting the
    /// index by one is a residual that is wrong everywhere and looks like lighting.
    ///
    /// The oracle is the direct form. The output is `Rg8Snorm`, so the comparison is
    /// on the quantised byte: a running sum accumulates float error the re-summation
    /// does not, and what has to hold is that it stays inside a quantisation step
    /// (1/127 of the slope range), not that the two agree bit for bit.
    #[test]
    fn the_running_box_blur_matches_summing_the_window_per_texel() {
        // Not smooth: a residual filter's job is the high-frequency part, so the test
        // field has to have one. Deterministic, and with a plateau, a cliff and a
        // ripple so the clamped edges are exercised from both sides.
        const N: usize = 96;
        let mut samples = vec![0.0f32; N * N];
        for z in 0..N {
            for x in 0..N {
                let (fx, fz) = (x as f32, z as f32);
                let ripple = ((fx * 0.7).sin() + (fz * 1.3).cos()) * 0.35;
                let cliff = if x > N / 2 { 4.0 } else { 0.0 };
                let slope = fz * 0.05;
                samples[z * N + x] = ripple + cliff + slope;
            }
        }
        let got = encode_slope(&samples, N, FINE_SAMPLE_M);

        // The direct form, exactly as it was written before the slide.
        let mut gx = vec![0.0f32; N * N];
        let mut gz = vec![0.0f32; N * N];
        let inv = 1.0 / (2.0 * FINE_SAMPLE_M);
        for z in 0..N {
            for x in 0..N {
                let (xm, xp) = (x.saturating_sub(1), (x + 1).min(N - 1));
                let (zm, zp) = (z.saturating_sub(1), (z + 1).min(N - 1));
                gx[z * N + x] = (samples[z * N + xp] - samples[z * N + xm]) * inv;
                gz[z * N + x] = (samples[zp * N + x] - samples[zm * N + x]) * inv;
            }
        }
        let r = DETAIL_CARRIED_TEXELS / 2;
        let mut want = vec![0u8; N * N * 2];
        let mut lo = vec![0.0f32; N * N];
        for (channel, g) in [&gx, &gz].into_iter().enumerate() {
            for z in 0..N {
                for x in 0..N {
                    let mut sum = 0.0;
                    for d in 0..DETAIL_CARRIED_TEXELS {
                        sum += g[z * N + (x + d).saturating_sub(r).min(N - 1)];
                    }
                    lo[z * N + x] = sum / DETAIL_CARRIED_TEXELS as f32;
                }
            }
            for x in 0..N {
                for z in 0..N {
                    let mut sum = 0.0;
                    for d in 0..DETAIL_CARRIED_TEXELS {
                        sum += lo[(z + d).saturating_sub(r).min(N - 1) * N + x];
                    }
                    let carried = sum / DETAIL_CARRIED_TEXELS as f32;
                    let residual = (g[z * N + x] - carried) / DETAIL_SLOPE_RANGE;
                    want[(z * N + x) * 2 + channel] =
                        ((residual.clamp(-1.0, 1.0) * 127.0).round() as i8) as u8;
                }
            }
        }

        assert_eq!(got.len(), want.len());
        let mut worst = 0i32;
        for (i, (&a, &b)) in got.iter().zip(&want).enumerate() {
            let d = (a as i8 as i32 - b as i8 as i32).abs();
            assert!(
                d <= 1,
                "texel {i} is {d} apart: running {} vs direct {}",
                a as i8,
                b as i8
            );
            worst = worst.max(d);
        }
        assert!(worst <= 1, "worst quantised difference was {worst}");
    }

    /// The tier must agree with the coarse field at its own rim, or walking out of it
    /// is a step rather than a loss of detail.
    #[test]
    fn the_fine_tier_fades_into_the_coarse_answer_at_its_rim() {
        let mut field = ramp_field(0.0);
        let centre = Vec2::new(100.0, -50.0);
        // A tier that disagrees with the (flat, zero) coarse field everywhere.
        field.fine = TerrainSnapshot {
            samples: std::sync::Arc::new(vec![10.0; 4 * 4]),
            half_m: FINE_HALF_M,
            grid: 4,
            centre,
            floors: std::sync::Arc::default(),
            banks: std::sync::Arc::default(),
        };

        let mid = field.ground_at(centre.x, centre.y);
        assert!(
            (mid - 10.0).abs() < 1e-3,
            "the tier should be trusted completely in the middle, got {mid}"
        );
        let out = field.ground_at(centre.x + FINE_HALF_M + 1.0, centre.y);
        assert!(
            out.abs() < 1e-6,
            "outside the box the coarse answer must be returned unchanged, got {out}"
        );
        let rim = field.ground_at(centre.x + FINE_HALF_M, centre.y);
        assert!(
            rim.abs() < 1e-3,
            "the tier must reach zero weight at its boundary, got {rim}"
        );
        // And it has to get there smoothly.
        let mut prev = field.ground_at(centre.x + FINE_HALF_M - FINE_BLEND_M - 1.0, centre.y);
        let mut step = 0.0f32;
        for i in 0..=64 {
            let t = i as f32 / 64.0;
            let x = centre.x + FINE_HALF_M - FINE_BLEND_M - 1.0 + t * (FINE_BLEND_M + 2.0);
            let h = field.ground_at(x, centre.y);
            step = step.max((h - prev).abs());
            prev = h;
        }
        assert!(
            step < 1.0,
            "the blend has a {step} m cliff in it; a smoothstep should spread 10 m over \
             {FINE_BLEND_M} m"
        );
    }

    /// **The property the whole coarse-level scheme rests on.** A clipmap level whose
    /// quads are coarser than the field draws its surface through `floor_at`; if that
    /// surface can rise above the measured ground anywhere, the level pokes through
    /// the detailed one drawn over it and the blurry imagery shows in the middle of
    /// the map. Checked over ground far rougher than the Netherlands has, at every
    /// quad size a relieved tile can ask for, and not only at the sample points —
    /// between them, where linear interpolation is what a mesh actually draws.
    #[test]
    fn a_coarse_quad_never_draws_above_the_measured_ground() {
        let n = 257;
        let half = 400.0f32;
        let sample = half * 2.0 / (n - 1) as f32;
        // Sharp ridges and pits at every scale, so a min filter has something to bite.
        let height = |x: f32, z: f32| {
            (x * 0.31).sin() * 4.0 + (z * 0.17).cos() * 3.0 + (x * 0.03 + z * 0.05).sin() * 12.0
        };
        let mut samples = Vec::with_capacity(n * n);
        for zi in 0..n {
            for xi in 0..n {
                let (x, z) = (-half + xi as f32 * sample, -half + zi as f32 * sample);
                samples.push(height(x, z));
            }
        }
        let floors = build_floors(&samples, n, sample);
        let field = TerrainSnapshot {
            samples: std::sync::Arc::new(samples),
            half_m: half,
            grid: n,
            centre: Vec2::ZERO,
            floors: std::sync::Arc::new(floors),
            banks: std::sync::Arc::default(),
        };

        // Within one rung there is no envelope at all: the finest relieved level has
        // nothing drawn over it and must carry the measurements.
        for quad in [sample, sample * 2.0] {
            let (x, z) = (37.0f32, -61.0f32);
            assert_eq!(
                field.floor_at(x, z, quad),
                field.at(x, z),
                "{quad} m quads should stand on the measurements, not an envelope"
            );
        }

        // From the first quad size that does take an envelope up to the coarsest a
        // relieved tile can ask for.
        for quad in [sample * 2.01, 12.5, 25.0, 50.0, 90.0] {
            // A mesh of this quad size, laid over the middle of the field where a
            // lookup has measurements on every side.
            let mut worst = f32::MIN;
            let mut steps = 0;
            let reach = half * 0.6;
            let mut z = -reach;
            while z <= reach {
                let mut x = -reach;
                while x <= reach {
                    // Where the mesh's own linear interpolation lands, between the
                    // vertices — a quarter, a half and three quarters along a quad.
                    for (fx, fz) in [(0.0, 0.0), (0.25, 0.5), (0.5, 0.5), (0.75, 0.25)] {
                        let (px, pz) = (x + fx * quad, z + fz * quad);
                        let a = field.floor_at(x, z, quad);
                        let b = field.floor_at(x + quad, z, quad);
                        let c = field.floor_at(x, z + quad, quad);
                        let d = field.floor_at(x + quad, z + quad, quad);
                        let top = a.max(b).max(c).max(d);
                        worst = worst.max(top - field.at(px, pz));
                        steps += 1;
                    }
                    x += quad;
                }
                z += quad;
            }
            assert!(steps > 0);
            assert!(
                worst <= 1e-3,
                "{quad} m quads draw {worst} m above the measured ground; a level with \
                 quads this size would poke through the one over it"
            );
        }
    }

    /// The fill's reach is a distance in metres, whatever the sampling is. This is the
    /// test the constant needed and did not have: it was six one-cell dilations
    /// documented as "~150 m", which silently became 37 m the moment the field's
    /// resolution moved.
    #[test]
    fn the_fill_reaches_the_same_distance_at_any_sampling() {
        // The same 40 m hole in the middle of flat ground, sampled two ways.
        for (n, sample_m) in [(64usize, 4.0f32), (128, 2.0)] {
            let hole = (40.0 / sample_m) as usize;
            let lo = n / 2 - hole / 2;
            let mut grid = vec![1.0f32; n * n];
            for z in lo..lo + hole {
                for x in lo..lo + hole {
                    grid[z * n + x] = f32::NAN;
                }
            }
            fill_holes(&mut grid, n, Some(FILL_REACH_M / sample_m));
            assert!(
                grid.iter().all(|v| v.is_finite()),
                "a 40 m hole is well inside {FILL_REACH_M} m of reach, but {sample_m} m \
                 sampling left it open"
            );
            let (lo_v, hi_v) = grid
                .iter()
                .fold((f32::MAX, f32::MIN), |(l, h), v| (l.min(*v), h.max(*v)));
            assert!(
                (lo_v - 1.0).abs() < 1e-3 && (hi_v - 1.0).abs() < 1e-3,
                "filling a hole in flat ground invented relief: {lo_v}..{hi_v}"
            );
        }
    }

    /// The carve has to reach ground AHN *did* return a value for. At 25 m the value
    /// over a canal is the banks averaged in by resampling, not a bed — skipping those
    /// samples is what kept the basemap flat for a day.
    #[test]
    fn the_carve_overwrites_resampled_ground_but_keeps_a_deeper_reading() {
        let n = 8;
        let half = 100.0f32;
        // One ring covering the whole grid, surface at 0 m NAP.
        let field = crate::systems::water_levels::WaterLevelField::build_for_test(
            0.0,
            &[
                [-half * 2.0, -half * 2.0],
                [half * 2.0, -half * 2.0],
                [half * 2.0, half * 2.0],
                [-half * 2.0, half * 2.0],
            ],
        );
        let bed = cartopolis_geo::terrain_grade::WATER_BED_M;

        let mut grid = vec![5.0f32; n * n];
        grid[0] = f32::NAN; // a genuine hole
        grid[1] = -9.0; // a dredged channel: deeper than the surface
        let carved = carve_water(&mut grid, n, half, Vec2::ZERO, &field, false);

        assert!(carved > 0, "nothing was carved at all");
        assert!(
            (grid[0] - (0.0 - bed)).abs() < 1e-5,
            "a hole inside the ring must take the water's own level"
        );
        assert!(
            (grid[1] - -9.0).abs() < 1e-5,
            "a deeper measurement must survive: the bed can be lower than the surface"
        );
        assert!(
            (grid[5] - (0.0 - bed)).abs() < 1e-5,
            "resampled bank height inside the ring must be overwritten, not skipped"
        );
    }

    /// The half-metre tier keeps what it measured standing above the water.
    ///
    /// The mirror of the test above, and the two together are the whole rule: at
    /// 3.125 m a value inside a canal is the banks averaged in by a resampling
    /// service and must be cut through; at 0.5 m it is a laser return off something
    /// solid, because the beam does not come back off water at all.
    ///
    /// What it saves is not hypothetical. Sampling inside the registered water
    /// polygons over the study quay block at the native half metre: 78.3 % no-data,
    /// 21.6 % measured above the surface — 5,052 m² of quay path, jetty and lock —
    /// and 0.1 % measured at or below it.
    #[test]
    fn the_fine_carve_keeps_a_structure_standing_above_the_water() {
        let n = 8;
        let half = 100.0f32;
        let field = crate::systems::water_levels::WaterLevelField::build_for_test(
            0.0,
            &[
                [-half * 2.0, -half * 2.0],
                [half * 2.0, -half * 2.0],
                [half * 2.0, half * 2.0],
                [-half * 2.0, half * 2.0],
            ],
        );
        let bed = cartopolis_geo::terrain_grade::WATER_BED_M;

        let mut grid = vec![f32::NAN; n * n];
        grid[1] = 0.10; // a quay path: a real return, 10 cm above the surface
        grid[2] = -9.0; // a dredged channel: a real return, below the surface
        let carved = carve_water(&mut grid, n, half, Vec2::ZERO, &field, true);

        assert!(carved > 0, "the open water still has to be carved");
        assert!(
            (grid[0] - (0.0 - bed)).abs() < 1e-5,
            "no-data is open water and still takes the bed"
        );
        assert!(
            (grid[1] - 0.10).abs() < 1e-5,
            "a measured surface above the water must survive, got {}",
            grid[1]
        );
        assert!(
            (grid[2] - -9.0).abs() < 1e-5,
            "a deeper measurement must still survive"
        );

        // …and the coarse tier must not do this, or a resampled bank keeps the
        // basemap flat across the canal.
        let mut coarse = vec![f32::NAN; n * n];
        coarse[1] = 0.10;
        carve_water(&mut coarse, n, half, Vec2::ZERO, &field, false);
        assert!(
            (coarse[1] - (0.0 - bed)).abs() < 1e-5,
            "the coarse tier must still cut through a resampled value, got {}",
            coarse[1]
        );
    }

    /// Nothing registered must leave the field byte-identical.
    #[test]
    fn an_empty_water_field_changes_no_sample() {
        let n = 8;
        let mut grid = vec![5.0f32; n * n];
        let before = grid.clone();
        let carved = carve_water(
            &mut grid,
            n,
            100.0,
            Vec2::ZERO,
            &crate::systems::water_levels::WaterLevelField::default(),
            true,
        );
        assert_eq!(carved, 0);
        assert_eq!(grid, before);
    }

    /// Samples per side for the synthetic fields below. Not [`TERRAIN_GRID`]: these
    /// tests are about sampling and blending, and the real grid is four million
    /// samples plus a pyramid over them, built once per test that touches one.
    const RAMP_GRID: usize = 513;

    /// A field whose samples are a known ramp, bypassing the fetch.
    fn ramp_field(per_metre: f32) -> TerrainField {
        let n = RAMP_GRID;
        let half = TERRAIN_HALF_M;
        let step = half * 2.0 / (n - 1) as f32;
        let mut samples = vec![0.0; n * n];
        for z in 0..n {
            for x in 0..n {
                samples[z * n + x] = (-half + x as f32 * step) * per_metre;
            }
        }
        let floors = build_floors(&samples, n, half * 2.0 / (n - 1) as f32);
        TerrainField {
            ground: TerrainSnapshot {
                samples: std::sync::Arc::new(samples),
                half_m: half,
                grid: n,
                centre: Vec2::ZERO,
                floors: std::sync::Arc::new(floors),
                banks: std::sync::Arc::default(),
            },
            ..Default::default()
        }
    }

    /// A flat quad with a vertex at each corner, in the XZ plane — the shape a
    /// route ribbon is made of.
    fn flat_quad(size: f32) -> Mesh {
        use bevy::render::mesh::Indices;
        let mut mesh = Mesh::new(
            bevy::render::mesh::PrimitiveTopology::TriangleList,
            bevy::asset::RenderAssetUsages::default(),
        );
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_POSITION,
            vec![
                [0.0, 0.0, 0.0],
                [size, 0.0, 0.0],
                [size, 0.0, size],
                [0.0, 0.0, size],
            ],
        );
        mesh.insert_indices(Indices::U32(vec![0, 1, 2, 0, 2, 3]));
        mesh
    }

    /// Lift every vertex of a mesh, the way the route ribbon arrives off a bridge
    /// deck.
    fn raise(mesh: &mut Mesh, by: f32) {
        let Some(bevy::render::mesh::VertexAttributeValues::Float32x3(p)) =
            mesh.attribute_mut(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("no positions");
        };
        for v in p.iter_mut() {
            v[1] += by;
        }
    }

    fn heights_of(mesh: &Mesh) -> Vec<f32> {
        let Some(bevy::render::mesh::VertexAttributeValues::Float32x3(p)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("no positions");
        };
        p.iter().map(|v| v[1]).collect()
    }

    /// The drape **adds** to the height a mesh already carries. That is what lets the
    /// route line arrive with a bridge deck baked into its vertices and still follow
    /// the ground the bridge stands on — a drape that assigned would flatten the
    /// viaduct back onto the terrain.
    #[test]
    fn draping_adds_to_the_height_a_mesh_already_has() {
        let field = ramp_field(0.01).snapshot();
        let at = Vec2::ZERO;

        let mut ground = flat_quad(10.0);
        drape_mesh(&mut ground, at, &field);

        let mut raised = flat_quad(10.0);
        raise(&mut raised, 6.0);
        drape_mesh(&mut raised, at, &field);

        let (a, b) = (heights_of(&ground), heights_of(&raised));
        assert_eq!(a.len(), b.len());
        assert!(
            a.iter().any(|y| *y != 0.0),
            "the ramp field should have moved the plain drape at all"
        );
        for (g, r) in a.iter().zip(&b) {
            assert!((r - g - 6.0).abs() < 1e-3, "ground {g}, raised {r}");
        }
    }

    /// An absent field leaves a mesh exactly as it was — including a height it
    /// brought with it. The old wording for this was "a no-op", which is only the
    /// same thing while nothing arrives pre-lifted.
    #[test]
    fn an_absent_field_leaves_a_raised_mesh_raised() {
        let mut mesh = flat_quad(10.0);
        raise(&mut mesh, 6.0);
        drape_mesh(&mut mesh, Vec2::ZERO, &TerrainField::default().snapshot());
        assert!(heights_of(&mesh).iter().all(|y| (*y - 6.0).abs() < 1e-6));
    }

    #[test]
    fn an_absent_field_is_flat_everywhere() {
        let f = TerrainField::default();
        assert!(!f.is_ready());
        assert_eq!(f.at(0.0, 0.0), 0.0);
        assert_eq!(f.at(1_000.0, -2_500.0), 0.0);
        // The point of returning zero rather than an Option: every consumer adds it
        // unconditionally, so "layer off" costs a branch, not a code path.
        assert_eq!(f.at(f32::MAX, f32::MIN), 0.0);
    }

    #[test]
    fn sampling_is_bilinear_and_bounded() {
        let f = ramp_field(0.01); // 1 cm per metre east
        assert!((f.at(0.0, 0.0)).abs() < 1e-3);
        assert!((f.at(100.0, 0.0) - 1.0).abs() < 0.01);
        assert!((f.at(-100.0, 250.0) + 1.0).abs() < 0.01);
        // Between two samples, not snapped to one of them.
        let step = TERRAIN_HALF_M * 2.0 / (RAMP_GRID - 1) as f32;
        let mid = f.at(step * 0.5, 0.0);
        assert!(mid > f.at(0.0, 0.0) && mid < f.at(step, 0.0));
        // Outside the box is flat world, not an extrapolated ramp.
        assert_eq!(f.at(TERRAIN_HALF_M + 1.0, 0.0), 0.0);
        assert_eq!(f.at(0.0, -TERRAIN_HALF_M - 1.0), 0.0);
    }

    #[test]
    fn the_edge_taper_reaches_zero_at_the_boundary() {
        let n = RAMP_GRID;
        assert_eq!(edge_taper(n / 2, n / 2, n), 1.0);
        assert_eq!(edge_taper(0, n / 2, n), 0.0);
        assert_eq!(edge_taper(n - 1, n - 1, n), 0.0);
        // Monotone on the way out from the middle — a taper with a bump in it is a
        // ridge that isn't there.
        let mut last = 1.0;
        for i in n / 2..n {
            let t = edge_taper(i, n / 2, n);
            assert!(t <= last + 1e-6, "taper rose at {i}");
            last = t;
        }
    }

    #[test]
    fn holes_fill_from_their_neighbours_and_big_voids_stay_flat() {
        // 10 m sampling, so the reach is 15 cells and the two holes straddle it.
        let sample_m = 10.0f32;
        let n = 128;
        let mut grid = vec![5.0f32; n * n];
        // A bridge-sized cut-out.
        for z in 10..14 {
            for x in 10..14 {
                grid[z * n + x] = f32::NAN;
            }
        }
        // …and a void far larger than the fill can reach: 600 m across, so its
        // middle is 300 m from the nearest measurement.
        for z in 50..110 {
            for x in 50..110 {
                grid[z * n + x] = f32::NAN;
            }
        }
        fill_holes(&mut grid, n, Some(FILL_REACH_M / sample_m));
        assert!(grid[12 * n + 12].is_finite(), "small hole not filled");
        assert!(
            (grid[12 * n + 12] - 5.0).abs() < 1e-3,
            "fill invented a slope"
        );
        assert!(
            !grid[80 * n + 80].is_finite(),
            "a 600 m void must not be smeared shut"
        );
    }

    /// Signed area doubled — the sign is the winding, and the winding is whether
    /// the triangle is visible at all.
    fn winding(p: &[[f32; 3]], t: &[u32]) -> f32 {
        let (a, b, c) = (p[t[0] as usize], p[t[1] as usize], p[t[2] as usize]);
        (b[0] - a[0]) * (c[2] - a[2]) - (c[0] - a[0]) * (b[2] - a[2])
    }

    #[test]
    fn refinement_splits_long_edges_and_leaves_short_ones_alone() {
        // Two triangles sharing the diagonal of a 200 m square — earcut's answer
        // for a field, and the shape that cannot follow a hill.
        let mut positions = vec![
            [0.0, 0.0, 0.0],
            [200.0, 0.0, 0.0],
            [200.0, 0.0, 200.0],
            [0.0, 0.0, 200.0],
        ];
        let mut indices = vec![0, 1, 2, 0, 2, 3];
        let before = winding(&positions, &indices[0..3]).signum();
        let (mut uvs, mut normals, mut colors) = (vec![], vec![], vec![]);
        refine_for_terrain(
            &mut positions,
            &mut indices,
            &mut uvs,
            &mut normals,
            &mut colors,
            &mut vec![],
            24.0,
            None,
        );
        // Every edge now short enough for the field to be expressed on it…
        for tri in indices.chunks_exact(3) {
            for (i, j) in [(0, 1), (1, 2), (2, 0)] {
                let (a, b) = (positions[tri[i] as usize], positions[tri[j] as usize]);
                let len = ((a[0] - b[0]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
                assert!(len <= 24.0 + 1e-3, "edge of {len} m survived refinement");
            }
            // …and still facing the way it was.
            assert_eq!(
                winding(&positions, tri).signum(),
                before,
                "a sub-triangle flipped"
            );
        }

        // Already-fine geometry is returned untouched: refinement is a cost paid
        // only where the ground is coarse.
        let mut small = vec![[0.0, 0.0, 0.0], [5.0, 0.0, 0.0], [0.0, 0.0, 5.0]];
        let mut small_idx = vec![0, 1, 2];
        refine_for_terrain(
            &mut small,
            &mut small_idx,
            &mut vec![],
            &mut vec![],
            &mut vec![],
            &mut vec![],
            24.0,
            None,
        );
        assert_eq!(small.len(), 3);
        assert_eq!(small_idx, vec![0, 1, 2]);
    }

    #[test]
    fn refinement_shares_midpoints_so_a_slope_has_no_cracks() {
        // Two triangles across one long shared edge. If each invented its own
        // midpoint, displacement would pull them apart along that seam.
        let mut positions = vec![
            [0.0, 0.0, 0.0],
            [100.0, 0.0, 0.0],
            [0.0, 0.0, 100.0],
            [100.0, 0.0, 100.0],
        ];
        let mut indices = vec![0, 1, 2, 1, 3, 2];
        refine_for_terrain(
            &mut positions,
            &mut indices,
            &mut vec![],
            &mut vec![],
            &mut vec![],
            &mut vec![],
            24.0,
            None,
        );
        // Every position is used by some triangle, and no position is duplicated:
        // both are what "the seam is stitched" means in index terms.
        let used: std::collections::HashSet<u32> = indices.iter().copied().collect();
        assert_eq!(
            used.len(),
            positions.len(),
            "orphaned or duplicated vertices"
        );
        let mut seen = std::collections::HashSet::new();
        for p in &positions {
            let key = (
                (p[0] * 100.0).round() as i64,
                (p[1] * 100.0).round() as i64,
                (p[2] * 100.0).round() as i64,
            );
            assert!(
                seen.insert(key),
                "duplicate vertex at {p:?} — that is a crack"
            );
        }
    }

    #[test]
    fn refinement_interpolates_the_attributes_it_carries() {
        let mut positions = vec![[0.0, 0.0, 0.0], [100.0, 0.0, 0.0], [0.0, 0.0, 100.0]];
        let mut indices = vec![0, 1, 2];
        let mut uvs = vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]];
        let mut normals = vec![[0.0, 1.0, 0.0]; 3];
        let mut colors = vec![
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 1.0, 0.0, 1.0],
            [1.0, 0.0, 0.0, 1.0],
        ];
        refine_for_terrain(
            &mut positions,
            &mut indices,
            &mut uvs,
            &mut normals,
            &mut colors,
            &mut vec![],
            24.0,
            None,
        );
        // A road's markings register with its geometry through UV0; dropping the
        // attribute arrays at refinement would strip the markings off every long
        // straight in the city.
        assert_eq!(uvs.len(), positions.len());
        assert_eq!(normals.len(), positions.len());
        assert_eq!(colors.len(), positions.len());
        for n in &normals {
            assert!((Vec3::from(*n).length() - 1.0).abs() < 1e-4);
        }
        for c in &colors {
            assert!(c.iter().all(|v| (0.0..=1.0).contains(v)));
        }
    }

    #[test]
    fn water_sits_level_over_the_polygon_it_covers() {
        let f = ramp_field(0.01);
        // A canal running up the slope: per-vertex would tilt it, the median holds
        // it level at the middle.
        let level = f.level_over([[-100.0, 0.0], [0.0, 0.0], [100.0, 0.0]]);
        assert!((level - f.at(0.0, 0.0)).abs() < 0.01);
        assert_eq!(TerrainField::default().level_over([[0.0, 0.0]]), 0.0);
    }

    /// **The embankment is built, and it climbs at the design grade.**
    ///
    /// The wiring test for the thing that existed for a day and was reachable
    /// only from `carto-view` (`bridge-heights.md`). A flat box, a deck standing
    /// six metres over it, and an approach running west out of the abutment: the
    /// ground along that approach has to rise to meet the deck at no more than
    /// `MAX_APPROACH_GRADE`, and the ground either side of it must not move at
    /// all.
    #[test]
    fn an_approach_climbs_to_its_deck_at_the_design_grade() {
        let anchor = (53.2194, 6.5665, 14);
        let centre = wgs84_to_rd(anchor.0, anchor.1);
        let (w, h) = (64, 64);
        let half_rd = 4_000.0f64;
        // Dead flat at 20 m, so anything the output carries is the grading.
        let patch = HeightPatch {
            min: [centre[0] - half_rd, centre[1] - half_rd],
            max: [centre[0] + half_rd, centre[1] + half_rd],
            w,
            h,
            samples: vec![20.0f32; w * h],
        };
        let n = 129;
        let step = TERRAIN_HALF_M * 2.0 / (n - 1) as f32;
        let mid = n / 2;

        // The structure: a 3-sample block at the middle, six metres up. The
        // approach: the row west of it, out to the rim.
        let mut deck_lift = vec![f32::NAN; n * n];
        let mut ramp = vec![false; n * n];
        let mut road = vec![false; n * n];
        for dz in -1i32..=1 {
            let z = (mid as i32 + dz) as usize;
            for dx in 0..3 {
                deck_lift[z * n + mid + dx] = 6.0;
            }
            for x in 1..mid {
                ramp[z * n + x] = true;
                road[z * n + x] = true;
            }
        }

        let (baked, approach) = bake(
            &patch,
            anchor,
            Vec2::ZERO,
            None,
            TERRAIN_HALF_M,
            n,
            Some(Grading {
                water: &crate::systems::water_levels::WaterLevelField::default(),
                road,
                ramp,
                deck_lift: &deck_lift,
                // No measured top: the clearance path, which is what this pins.
                deck_top: &[],
            }),
        );
        let (samples, _datum, _carved) = baked.expect("patch covers the field");

        assert!(
            approach.0 > 0,
            "no abutment sample was pinned, so no embankment was built at all — \
             which is exactly the state this feature was in while the grading \
             sat wired into carto-view alone"
        );
        assert!(approach.1 > 0, "the embankment raised nothing");

        // At the abutment the ground has climbed most of the way to the deck…
        let at = |x: usize| samples[mid * n + x];
        let abutment = at(mid - 1);
        assert!(
            abutment > 4.0,
            "the ground at the abutment reached {abutment:.2} m of the deck's 6"
        );
        // …and it got there no faster than the design grade allows.
        for x in 2..mid {
            let rise = at(x) - at(x - 1);
            assert!(
                rise <= cartopolis_geo::road_grade::MAX_APPROACH_GRADE * step + 1e-3,
                "the embankment climbed {rise:.3} m over {step:.2} m at sample {x}, \
                 steeper than the {:.0} % ceiling",
                cartopolis_geo::road_grade::MAX_APPROACH_GRADE * 100.0
            );
        }
        // And the ground away from the corridor is untouched, or the ramp has
        // travelled through everything the mask happens to connect.
        let away = samples[(mid + 20) * n + (mid - 20)];
        assert!(
            away.abs() < 0.05,
            "ground 20 samples off the approach moved to {away:.2} m"
        );
    }

    #[test]
    fn baking_is_relative_to_the_anchor_and_flat_at_the_rim() {
        // A patch that rises 1 m per 100 m east, over a box around Groningen.
        let anchor = (53.2194, 6.5665, 14);
        let centre = wgs84_to_rd(anchor.0, anchor.1);
        let (w, h) = (64, 64);
        let (half_rd, mut samples) = (4_000.0f64, Vec::with_capacity(w * h));
        for _row in 0..h {
            for col in 0..w {
                let east = -half_rd + col as f64 / (w - 1) as f64 * half_rd * 2.0;
                samples.push(20.0 + (east / 100.0) as f32);
            }
        }
        let patch = HeightPatch {
            min: [centre[0] - half_rd, centre[1] - half_rd],
            max: [centre[0] + half_rd, centre[1] + half_rd],
            w,
            h,
            samples,
        };
        // No registered water: this test is about the ramp the patch carries, and an
        // empty field is also the everywhere-outside-the-Netherlands case.
        // A test grid, not `TERRAIN_GRID`: the bake runs a `wgs84_to_rd` polynomial
        // per sample and the real field is four million of them.
        let n = 129;
        let (baked, approach) = bake(
            &patch,
            anchor,
            Vec2::ZERO,
            None,
            TERRAIN_HALF_M,
            n,
            Some(Grading {
                water: &crate::systems::water_levels::WaterLevelField::default(),
                // No roads either: this test is about the ramp the patch carries,
                // and an empty mask is what a box with no streamed cell in it yet
                // has.
                road: Vec::new(),
                // …and no decks, for the same reason.
                ramp: Vec::new(),
                deck_lift: &[],
                deck_top: &[],
            }),
        );
        assert_eq!(
            approach,
            (0, 0),
            "a box with no viaduct in it must build no embankment"
        );
        let (baked, datum, carved) = baked.expect("patch covers the field");
        assert_eq!(carved, 0, "an empty water field must carve nothing");
        // The datum is the ground at the anchor, and the field is zero there. The
        // tolerance is the synthetic patch's own 125 m sample spacing: `at` takes
        // the nearest measurement, deliberately, so the centre reads the cell it
        // lands in rather than an interpolation of four.
        assert!(
            (datum - 20.0).abs() < 1.0,
            "datum {datum} should be ~20 m NAP"
        );
        assert!(baked[(n / 2) * n + n / 2].abs() < 0.5);
        // A kilometre east is ~10 m up, relative — not 30 m absolute.
        let one_km =
            ((1_000.0 + TERRAIN_HALF_M) / (TERRAIN_HALF_M * 2.0) * (n - 1) as f32) as usize;
        let east = baked[(n / 2) * n + one_km];
        assert!((east - 10.0).abs() < 1.5, "1 km east read {east} m");
        // Every edge sample is flattened, so the field meets the flat world outside.
        for i in 0..n {
            assert_eq!(baked[i], 0.0, "north rim");
            assert_eq!(baked[(n - 1) * n + i], 0.0, "south rim");
            assert_eq!(baked[i * n], 0.0, "west rim");
            assert_eq!(baked[i * n + n - 1], 0.0, "east rim");
        }
    }

    /// **A road cell marks the field it will be drawn on.**
    ///
    /// The mask is the whole of what makes the bake able to grade, and nothing in a
    /// picture distinguishes "the grading ran and this ground is genuinely rough"
    /// from "the mask was empty, so it did nothing" — which is what the client did
    /// on every platform until 2026-09-21. So this asserts the two facts the bake
    /// depends on: a cell with carriageways in it marks samples, and it marks them
    /// where the road actually is.
    #[test]
    fn a_cell_of_roads_marks_the_field_under_them() {
        use crate::systems::vector_tiles::{SurfaceGroup, SurfaceMesh};

        let grid = 129;
        let step = TERRAIN_HALF_M * 2.0 / (grid - 1) as f32;
        // A 40 m square of carriageway at the field's centre, given as a cell whose
        // own origin is the anchor — so tile-local and world metres coincide and the
        // expected samples are arithmetic rather than a second implementation.
        let quad = |half: f32| SurfaceMesh {
            group: SurfaceGroup::Road(cartopolis_geo::road_texture::RoadClass::Carriageway),
            positions: vec![
                [-half, 0.0, -half],
                [half, 0.0, -half],
                [half, 0.0, half],
                [-half, 0.0, half],
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
            uvs: Vec::new(),
            normals: Vec::new(),
            colors: Vec::new(),
            rungs: Vec::new(),
        };

        let mut mask = RoadMask::default();
        assert_eq!(mask.marked_samples(), 0, "nothing is marked before a cell");
        mask.mark_cell(7, grid, (0, 0), Vec2::ZERO, &[quad(20.0)]);

        let marked = mask.marked_samples();
        assert!(marked > 0, "a cell of carriageway marked nothing");
        // The centre is road; a point well outside the square *and* outside the
        // margin is not. The margin is in samples, so this is stated in samples.
        let at = |x: i32, z: i32| {
            let c = (grid / 2) as i32;
            mask.marks[((c + z) as usize) * grid + (c + x) as usize]
        };
        assert!(at(0, 0), "the middle of the carriageway is not marked");
        let clear = (20.0 / step).ceil() as i32
            + cartopolis_geo::road_grade::DEFAULT_MARGIN_SAMPLES as i32
            + 2;
        assert!(!at(clear, 0), "ground {clear} samples away was marked");

        // **The same cell twice must not count twice.** Every cell is re-tessellated
        // whenever the terrain revision moves, and the terrain revision is what this
        // mask drives — so a bump per re-tessellation is a bake that never settles.
        let rev = mask.revision;
        mask.mark_cell(7, grid, (0, 0), Vec2::ZERO, &[quad(20.0)]);
        assert_eq!(mask.revision, rev, "the same cell was marked twice");

        // A re-anchor is a different frame, so the marks go.
        mask.mark_cell(8, grid, (0, 0), Vec2::ZERO, &[]);
        assert_eq!(
            mask.marked_samples(),
            0,
            "marks from before a re-anchor survived it"
        );
    }

    /// A tunnel's carriageway is metres below the ground and must not drag the
    /// ground down to meet it — the mask is the surface network, not every road.
    #[test]
    fn a_tunnel_deck_does_not_grade_the_ground_over_it() {
        use crate::systems::vector_tiles::{SurfaceGroup, SurfaceMesh};

        let mut mask = RoadMask::default();
        mask.mark_cell(
            1,
            129,
            (0, 0),
            Vec2::ZERO,
            &[SurfaceMesh {
                group: SurfaceGroup::TunnelRoad(
                    cartopolis_geo::road_texture::RoadClass::Carriageway,
                ),
                positions: vec![
                    [-20.0, -7.0, -20.0],
                    [20.0, -7.0, -20.0],
                    [20.0, -7.0, 20.0],
                ],
                indices: vec![0, 1, 2],
                uvs: Vec::new(),
                normals: Vec::new(),
                colors: Vec::new(),
                rungs: Vec::new(),
            }],
        );
        assert_eq!(
            mask.marked_samples(),
            0,
            "a bore graded the ground above itself"
        );
    }
}
