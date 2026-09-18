//! The ground beside the carriageway, as it was surveyed: pavements, cycle
//! paths, parking bays, driveways, verges and traffic islands.
//!
//! No Bevy: rings in, [`SurfaceMesh`]es out. The extraction is
//! cartopy's `bgt.ts` and the streaming is `systems::surveyed`.
//!
//! # Why this does not draw the road
//!
//! The obvious version of "use the BGT's road surfaces" replaces
//! [`tessellate_roads`](crate::vector_tiles::tessellate_roads) with the survey's
//! own polygons. It was rejected, and not on effort.
//!
//! What the OSM path draws is not just a shape. Each ribbon carries a road
//! *texture* — lane dashes, edge lines, wheel tracks, ballast and sleepers — a
//! kerb casing, and a rung on the per-vertex depth ladder that keeps a slip road
//! under its motorway and a level crossing under its rails. **The BGT cannot
//! supply any of that.** Its own road-furniture collections
//! (`weginrichtingselement_lijn` / `_vlak`) come back `niet-bgt` on every feature
//! over Groningen, so there are no markings in it to draw. Swapping the ribbons
//! for polygons would trade a marked, ordered road network for an unmarked flat
//! one, and call it an upgrade because the outline was more accurate.
//!
//! So the two sources are split by *what they are good at*, and the split is
//! clean because they then describe different ground:
//!
//! * **OSM keeps the vehicle network** — carriageways, railways — as textured
//!   ribbons, unchanged, everywhere in the world.
//! * **The BGT supplies everything beside it**, which OSM draws as a thin
//!   constant-width line or not at all. A pavement is a shape, not a width, and
//!   its edge against the tarmac *is* the kerb line.
//!
//! The one place they would have overlapped is the footway and cycle network,
//! which OSM does draw as `RoadClass::Path` ribbons. Over a covered cell those
//! ribbons are suppressed and these polygons stand in their place, so no ground
//! is ever drawn twice.
//!
//! # Depth
//!
//! Paving takes its own rung, above the water and land fills and below the road
//! band. That ordering is what makes an overlap harmless rather than a z-fight:
//! where a nominal-width OSM carriageway laps over a surveyed pavement, the
//! tarmac wins, which is also what it looks like from the pavement.

use crate::flat_fill::{build_flat_fills, FlatFill};
use crate::road_stitch::{stitch, Ribbon};
use crate::vector_tiles::{palette, SurfaceGroup, SurfaceMesh};

/// A surveyed paved surface, in tile-local metres.
#[derive(Debug, Clone)]
pub struct PavedArea {
    pub kind: PavingKind,
    /// What the survey says it is *made of*, where it says anything.
    /// `None` reproduces the kind's own tone exactly — see [`PavingMaterial`].
    pub material: Option<PavingMaterial>,
    /// Outer ring first, holes after. Open rings — no repeated closing vertex.
    pub rings: Vec<Vec<[f32; 2]>>,
}

/// The paved forms. Mirrors `SURFACE_KINDS` in cartopy's `bgt.ts` —
/// **append, never renumber**.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PavingKind {
    /// Pavement and pedestrianised street.
    Footway,
    /// Cycle path. Its own tone because in this country it genuinely is its own
    /// colour — Dutch cycle paths are laid in red asphalt or red brick, and a
    /// grey one reads as a service road.
    Cycleway,
    /// Parking bay.
    Parking,
    /// The splay of a driveway across a pavement.
    Drive,
    /// Verge: the planted strip between the pavement and the road.
    Verge,
    /// Traffic island.
    Island,
    /// Paving with no function of its own — the transitions between two others,
    /// an apron at a junction. Neutral on purpose; it is the form for "this is
    /// hard ground and the survey does not say what for".
    Paved,
    /// A flight of steps in a footway. Drawn flat like the rest — the survey
    /// gives the outline, not the treads.
    Steps,
}

impl PavingKind {
    /// sRGB tone, carried per vertex.
    ///
    /// One group, one material, the variety in the vertex colours — the contract
    /// `Canopy`, `Trunk` and `Furniture` already follow, and the reason a city's
    /// worth of paving is a handful of draw calls.
    ///
    /// The tones sit in the same pale cartographic family as the rest of the map
    /// (see `scatter.rs` on why nothing here is saturated). They are separated
    /// enough to read as different materials from a first-floor window and not
    /// enough to stripe the street.
    #[must_use]
    pub fn rgb(self) -> u32 {
        match self {
            // Brick and concrete tile: warmer and lighter than tarmac.
            Self::Footway => 0xC8_C2_B8,
            Self::Cycleway => 0xC0_9A_8E,
            // A bay is the same tarmac as the road, a shade lighter so the bays
            // read as a band along the kerb rather than as more carriageway.
            Self::Parking => 0xB4_B2_AE,
            Self::Drive => 0xC2_BD_B4,
            // The one green here, and it is the grass the map already uses, so a
            // verge is the same substance as the lawn it runs into.
            Self::Verge => palette::GRASS,
            Self::Island => 0xC5_C1_B6,
            Self::Paved => 0xBE_BB_B4,
            Self::Steps => 0xBC_B7_AE,
        }
    }

    /// Whether this form keeps its own colour even when the survey names a
    /// material.
    ///
    /// True for exactly one form, and the reason is that the register describes
    /// *construction* and never colour. A Dutch cycle path is dyed red whether it
    /// was laid in asphalt or in brick, and `asfalt` is the commonest thing the
    /// survey says about one — so letting the material speak here would turn the
    /// most recognisable surface in the country grey along every street. The
    /// price is the occasional genuinely grey cycle path drawn red, which is the
    /// cheaper of the two errors by a wide margin.
    #[must_use]
    pub fn pins_its_own_tone(self) -> bool {
        matches!(self, Self::Cycleway)
    }
}

/// What a surveyed surface is made of. Mirrors `SURFACE_MATERIALS` in
/// cartopy's `bgt.ts` — **append, never renumber**.
///
/// # Why this is a second axis and not more kinds
///
/// [`PavingKind`] answers *what the ground is for* — pavement, parking bay,
/// verge. This answers *what it is made of*, and the two are independent: a brick
/// pavement and a brick parking bay are the same colour, and the survey records
/// them in a different field for that reason. Colour is a property of the
/// substance, so where the register names one it decides the tone and the kind's
/// own tone stands down.
///
/// **The register is sometimes silent, and silence has to cost nothing.**
/// Measured on a square kilometre of central Groningen, 2026-08-29: 2,813 of the
/// 3,393 surfaces this layer draws carry a material, i.e. 83%, and the rest carry
/// none. (The rate is that high because the surfaces here are the *detailed*
/// ones — pavements, cycle paths, parking bays. Across `wegdeel` as a whole,
/// carriageways included, the fine field is filled on about a third, and the
/// carriageways are the part this layer deliberately leaves to OpenStreetMap.)
///
/// So one surface in six still arrives with nothing said about it, `None`
/// reproduces the old look exactly, and this stays safe to ship outside the
/// Netherlands where nothing supplies it at all — the same contract
/// `building_facts`' alpha channel is built on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PavingMaterial {
    /// `asfalt`. The darkest tone here: a continuous black-top surface.
    Asphalt,
    /// `cementbeton`, `beton element` — poured or precast concrete.
    Concrete,
    /// `gebakken klinkers`: the fired clay brick a Dutch street is paved in.
    /// Warm and red-brown, and the reason this whole axis is worth having.
    Brick,
    /// `betonstraatstenen`: concrete block pavers. Brick's shape, concrete's
    /// colour — which is exactly the distinction the map could not draw before.
    ConcretePaver,
    /// `tegels`: the pale square slabs a pavement is laid in.
    Tiles,
    /// `sierbestrating`, `natuursteen` — decorative and natural stone.
    Stone,
    /// `grasklinkers`: the open concrete grid that grass grows through, used for
    /// overflow parking and fire lanes. Half its area is lawn, so it is toned
    /// between the two rather than as either.
    GrassPaver,
    /// `gras- en kruidachtigen`, and the coarse `groenvoorziening`.
    Grass,
    /// `bosplantsoen`, `heesters`, `planten`, `boomschors` — a planted bed
    /// rather than a lawn. Takes the canopy tone, so a shrub bed is recognisably
    /// the same substance as the crowns above it (the rule the surveyed hedges
    /// already follow).
    Planting,
    /// `grind`, `split`, `schelpen`, `puin` — loose stone, gravel and shell.
    Gravel,
    /// `zand`.
    Sand,
    /// The coarse `onverhard`: ground that was surveyed and found to be bare.
    Unpaved,
}

impl PavingMaterial {
    /// sRGB tone, carried per vertex exactly as [`PavingKind::rgb`] is.
    ///
    /// Same pale cartographic family as the rest of the map — these separate the
    /// substances at a first-floor window without striping the street.
    #[must_use]
    pub fn rgb(self) -> u32 {
        match self {
            Self::Asphalt => 0xAE_AC_A8,
            Self::Concrete => 0xC4_C2_BC,
            Self::Brick => 0xC7_A3_92,
            Self::ConcretePaver => 0xBF_BC_B4,
            Self::Tiles => 0xC9_C6_BF,
            Self::Stone => 0xBD_B6_AA,
            // Half lawn, half concrete grid, and toned as the mixture it is.
            Self::GrassPaver => 0xB8_C0_A9,
            Self::Grass => palette::GRASS,
            Self::Planting => palette::CANOPY,
            Self::Gravel => 0xC6_BE_AC,
            Self::Sand => palette::SAND,
            Self::Unpaved => 0xB9_AC_98,
        }
    }
}

/// The tone one surface is drawn in: its material where the survey names one and
/// the form does not pin its own colour, otherwise the form's.
#[must_use]
pub fn area_rgb(kind: PavingKind, material: Option<PavingMaterial>) -> u32 {
    match material {
        Some(m) if !kind.pins_its_own_tone() => m.rgb(),
        _ => kind.rgb(),
    }
}

/// Triangulate a cell's paving into merged meshes, each no larger than
/// `max_bytes`.
///
/// One group for all of it, like the furniture and the canopies: the kind rides
/// in the vertex colour. The triangulation itself is [`crate::flat_fill`], shared
/// with the crop parcels — the winding rule and the byte cap are the same problem
/// twice, and were not kept in two copies.
#[must_use]
pub fn build_paving_meshes(areas: &[PavedArea], max_bytes: Option<usize>) -> Vec<SurfaceMesh> {
    let fills: Vec<FlatFill<'_>> = areas
        .iter()
        .map(|a| FlatFill {
            rings: &a.rings,
            rgb: area_rgb(a.kind, a.material),
        })
        .collect();
    build_flat_fills(&fills, SurfaceGroup::Paving, max_bytes)
}

/// Take the carriageways out of the paving, so no ground is covered twice.
///
/// **This is the one deliberate overlap the ground stack kept, and relief is what
/// made it untenable.** A surveyed pavement and a nominal-width OpenStreetMap
/// ribbon do not agree to the centimetre about where the kerb is, so they lap over
/// each other here and there, and `GroundLayer::Paving` sits exactly **1.0 mm**
/// under `GroundLayer::Road` to make that overlap read as tarmac meeting a kerb.
/// On flat ground it does. Once the ground is the measured one, each sheet is bent
/// onto it against its own triangles and the two disagree about where the ground
/// *is* by decimetres — orders of magnitude past a millimetre and past any depth
/// bias — so the overlap stopped being a hairline at a kerb and became paving
/// standing through the middle of the carriageway. That is the white polygons over
/// the lane markings in the phone captures of Weg der Verenigde Naties.
///
/// A millimetre is not the thing to increase. This codebase has settled the same
/// shape of problem three times by ordering it and once by removing it, and only
/// the fourth held: `road_stitch` cuts the ribbons into a partition, and
/// `tessellate_surfaces` now cuts the ground fills the same way. Paving was in
/// neither pass — it is built after `tessellate_surfaces` has finished cutting and
/// appended untouched — so this puts it in one.
///
/// Every road wins over every pavement, which is not a judgement about which is
/// more important but about which one can be moved: an OSM ribbon's width is a
/// nominal number attached to a centre line, and the survey's polygon is a
/// measurement, but the ribbon is the piece carrying the lane markings, the kerb
/// casing and the depth rung, and a hole cut in it has nowhere to get those back
/// from. The pavement loses the strip it was never really entitled to.
///
/// The roads are passed in as blockers and **handed back untouched** — they are
/// already a partition among themselves, and re-running their own cut here would
/// re-decide the bevel self-overlap the module note deliberately leaves alone.
///
/// `surveyed_paving_supersedes` already suppresses the ribbon entirely for the
/// footways, cycle paths and pedestrian streets, so those never reach this. What
/// this removes is the vehicle carriageway laid over a surveyed verge, traffic
/// island, parking bay or junction apron — which on a dual carriageway is tens of
/// metres of it.
pub fn cut_paving_under_roads(paving: &mut Vec<SurfaceMesh>, roads: &[SurfaceMesh]) {
    if paving.is_empty() || roads.is_empty() {
        return;
    }
    let mut ribbons: Vec<Ribbon> = Vec::with_capacity(paving.len() + roads.len());
    // Paving first and lowest, so `stitch` only ever cuts these. `priority` is
    // strictly ordered by construction here: the index in the list breaks every
    // tie, and a tie that is not broken cuts *both* sides out and opens a hole.
    for (i, m) in paving.iter().enumerate() {
        ribbons.push(Ribbon {
            group: m.group,
            rung: 0.0,
            priority: (0, 0, i, 0, 0),
            bridge: false,
            positions: m.positions.clone(),
            uvs: m.uvs.clone(),
            colors: m.colors.clone(),
            indices: m.indices.clone(),
        });
    }
    for (i, m) in roads.iter().enumerate() {
        ribbons.push(Ribbon {
            group: m.group,
            rung: 0.0,
            priority: (1, 0, i, 0, 0),
            bridge: false,
            positions: m.positions.clone(),
            uvs: m.uvs.clone(),
            colors: m.colors.clone(),
            indices: m.indices.clone(),
        });
    }
    stitch(&mut ribbons);
    for (m, r) in paving.iter_mut().zip(ribbons.into_iter()) {
        // `stitch` carries only the two optional attribute sets a `Ribbon` has, so
        // anything else the mesh held per vertex no longer lines up with the
        // positions it came back with. A flat fill's normals are every one of them
        // straight up (`flat_fill::Buf::push`), so restating them at the new length
        // is exact rather than an approximation — and the assertion is what makes
        // that a checked claim instead of an assumption about another module.
        debug_assert!(
            m.normals.iter().all(|n| *n == [0.0, 1.0, 0.0]),
            "paving grew a normal the cut cannot restate"
        );
        debug_assert!(
            m.rungs.is_empty(),
            "paving grew a depth rung the cut does not carry"
        );
        let had_normals = !m.normals.is_empty();
        m.positions = r.positions;
        m.uvs = r.uvs;
        m.colors = r.colors;
        m.indices = r.indices;
        m.normals.clear();
        if had_normals {
            m.normals.resize(m.positions.len(), [0.0, 1.0, 0.0]);
        }
    }
    // A pavement wholly under a carriageway leaves nothing behind. An empty mesh is
    // a material, an entity and a draw call for no triangles, so it is dropped here
    // rather than travelling to the client to be skipped there.
    paving.retain(|m| !m.indices.is_empty());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flat_fill::TRI_BYTES;

    fn square(kind: PavingKind, size: f32) -> PavedArea {
        PavedArea {
            kind,
            material: None,
            rings: vec![vec![[0.0, 0.0], [size, 0.0], [size, size], [0.0, size]]],
        }
    }

    #[test]
    fn a_square_becomes_two_triangles_facing_up() {
        let meshes = build_paving_meshes(&[square(PavingKind::Footway, 4.0)], None);
        assert_eq!(meshes.len(), 1);
        assert_eq!(meshes[0].indices.len(), 6);
        assert!(meshes[0].normals.iter().all(|n| n[1] > 0.9));
    }

    /// Winding is not a detail here: the survey does not promise an orientation,
    /// and a ring that comes in the other way round must still face the sky.
    #[test]
    fn a_reversed_ring_still_faces_up() {
        let mut area = square(PavingKind::Parking, 4.0);
        area.rings[0].reverse();
        let meshes = build_paving_meshes(&[area], None);
        assert_eq!(meshes[0].indices.len(), 6);
        for t in meshes[0].indices.chunks_exact(3) {
            let p = |i: u32| meshes[0].positions[i as usize];
            let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
            let cross = (b[0] - a[0]) * (c[2] - a[2]) - (b[2] - a[2]) * (c[0] - a[0]);
            assert!(cross < 0.0, "triangle wound face-down: {a:?} {b:?} {c:?}");
        }
    }

    #[test]
    fn a_hole_is_cut_out() {
        let mut area = square(PavingKind::Paved, 10.0);
        area.rings
            .push(vec![[3.0, 3.0], [7.0, 3.0], [7.0, 7.0], [3.0, 7.0]]);
        let meshes = build_paving_meshes(&[area], None);
        // A square with a square hole triangulates to eight triangles, not two.
        assert!(
            meshes[0].indices.len() > 6,
            "expected the hole to add triangles, got {}",
            meshes[0].indices.len() / 3
        );
    }

    #[test]
    fn a_degenerate_ring_is_skipped_rather_than_drawn() {
        let area = PavedArea {
            kind: PavingKind::Footway,
            material: None,
            rings: vec![vec![[0.0, 0.0], [1.0, 0.0]]],
        };
        assert!(build_paving_meshes(&[area], None).is_empty());
        assert!(build_paving_meshes(&[], None).is_empty());
    }

    /// The byte cap is what keeps one cell's paving off the single-asset upload
    /// cliff, so it has to actually cut.
    #[test]
    fn the_byte_cap_cuts_a_cell_into_pieces() {
        let areas: Vec<PavedArea> = (0..64).map(|_| square(PavingKind::Footway, 4.0)).collect();
        let whole = build_paving_meshes(&areas, None);
        let split = build_paving_meshes(&areas, Some(TRI_BYTES * 8));
        assert_eq!(whole.len(), 1);
        assert!(split.len() > 1, "the cap should have cut it up");
        let tris = |m: &[SurfaceMesh]| m.iter().map(|x| x.indices.len()).sum::<usize>();
        assert_eq!(
            tris(&whole),
            tris(&split),
            "cutting must not lose triangles"
        );
    }

    /// Every kind must have a tone of its own, or the split into kinds buys
    /// nothing on screen.
    #[test]
    fn every_kind_has_its_own_tone() {
        use PavingKind::*;
        let kinds = [
            Footway, Cycleway, Parking, Drive, Verge, Island, Paved, Steps,
        ];
        for (i, a) in kinds.iter().enumerate() {
            for b in &kinds[i + 1..] {
                assert_ne!(a.rgb(), b.rgb(), "{a:?} and {b:?} share a tone");
            }
        }
    }

    /// Same rule one axis over: two materials that draw the same are two
    /// materials the wire is carrying for nothing.
    #[test]
    fn every_material_has_its_own_tone() {
        use PavingMaterial::*;
        let mats = [
            Asphalt,
            Concrete,
            Brick,
            ConcretePaver,
            Tiles,
            Stone,
            GrassPaver,
            Grass,
            Planting,
            Gravel,
            Sand,
            Unpaved,
        ];
        for (i, a) in mats.iter().enumerate() {
            for b in &mats[i + 1..] {
                assert_ne!(a.rgb(), b.rgb(), "{a:?} and {b:?} share a tone");
            }
        }
    }

    /// The whole safety argument for shipping this outside the Netherlands: a
    /// surface the register says nothing about must draw exactly as it did before
    /// the material axis existed.
    #[test]
    fn no_material_reproduces_the_form_tone() {
        for kind in [
            PavingKind::Footway,
            PavingKind::Cycleway,
            PavingKind::Parking,
            PavingKind::Drive,
            PavingKind::Verge,
            PavingKind::Island,
            PavingKind::Paved,
            PavingKind::Steps,
        ] {
            assert_eq!(area_rgb(kind, None), kind.rgb(), "{kind:?} drifted");
        }
    }

    /// And when it does say something, it has to reach the vertex — a material
    /// that parsed and then changed no pixel is a wire field for nothing.
    #[test]
    fn a_named_material_decides_the_tone() {
        let bare = build_paving_meshes(&[square(PavingKind::Footway, 4.0)], None);
        let mut brick = square(PavingKind::Footway, 4.0);
        brick.material = Some(PavingMaterial::Brick);
        let laid = build_paving_meshes(&[brick], None);
        assert_ne!(bare[0].colors[0], laid[0].colors[0]);
        assert_eq!(
            area_rgb(PavingKind::Footway, Some(PavingMaterial::Brick)),
            PavingMaterial::Brick.rgb()
        );
    }

    /// The one documented exception. A cycle path stays dyed whatever the survey
    /// says it was laid in, and if that ever stops being true it should stop here
    /// rather than silently along every street.
    #[test]
    fn a_cycle_path_keeps_its_colour_whatever_it_is_made_of() {
        for m in [
            PavingMaterial::Asphalt,
            PavingMaterial::Brick,
            PavingMaterial::Concrete,
        ] {
            assert_eq!(
                area_rgb(PavingKind::Cycleway, Some(m)),
                PavingKind::Cycleway.rgb(),
                "{m:?} overrode the cycle path"
            );
        }
    }

    /// A road-shaped mesh to cut against: one quad, `w` metres wide, running the
    /// length of the square the test paves.
    fn road_quad(w: f32, len: f32) -> SurfaceMesh {
        SurfaceMesh {
            group: SurfaceGroup::Road(crate::road_texture::RoadClass::Minor),
            positions: vec![
                [0.0, 0.0, 0.0],
                [w, 0.0, 0.0],
                [w, 0.0, len],
                [0.0, 0.0, len],
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
            uvs: Vec::new(),
            normals: Vec::new(),
            colors: Vec::new(),
            rungs: Vec::new(),
        }
    }

    /// How many of an `n × n` grid over `[0, size]²` a mesh covers, sampled at
    /// texel centres.
    fn covered(meshes: &[SurfaceMesh], size: f32, n: usize) -> Vec<bool> {
        let mut grid = vec![false; n * n];
        for m in meshes {
            for t in m.indices.chunks_exact(3) {
                let p: Vec<[f32; 2]> = t
                    .iter()
                    .map(|&i| {
                        let v = m.positions[i as usize];
                        [v[0] / size * n as f32, v[2] / size * n as f32]
                    })
                    .collect();
                let (a, b, c) = (p[0], p[1], p[2]);
                let side = |u: [f32; 2], v: [f32; 2], q: [f32; 2]| {
                    (v[0] - u[0]) * (q[1] - u[1]) - (v[1] - u[1]) * (q[0] - u[0])
                };
                let lo = |f: fn(&[f32; 2]) -> f32| {
                    (f(&a).min(f(&b)).min(f(&c)).floor().max(0.0) as usize).min(n - 1)
                };
                let hi = |f: fn(&[f32; 2]) -> f32| {
                    (f(&a).max(f(&b)).max(f(&c)).ceil().max(0.0) as usize).min(n - 1)
                };
                for y in lo(|q| q[1])..=hi(|q| q[1]) {
                    for x in lo(|q| q[0])..=hi(|q| q[0]) {
                        let q = [x as f32 + 0.5, y as f32 + 0.5];
                        let (s0, s1, s2) = (side(a, b, q), side(b, c, q), side(c, a, q));
                        if (s0 >= 0.0 && s1 >= 0.0 && s2 >= 0.0)
                            || (s0 <= 0.0 && s1 <= 0.0 && s2 <= 0.0)
                        {
                            grid[y * n + x] = true;
                        }
                    }
                }
            }
        }
        grid
    }

    /// **No ground is paved and surfaced at once.**
    ///
    /// The surveyed pavement and the nominal-width carriageway overlapped on
    /// purpose, held apart by the 1.0 mm between `GroundLayer::Paving` and
    /// `GroundLayer::Road`. That is a millimetre against the decimetres two sheets
    /// disagree by once each is bent onto the measured ground, so the overlap has
    /// to go rather than be ordered more carefully.
    #[test]
    fn a_carriageway_is_taken_out_of_the_paving_under_it() {
        const SIZE: f32 = 40.0;
        const N: usize = 256;
        let roads = vec![road_quad(12.0, SIZE)];
        let mut paving = build_paving_meshes(&[square(PavingKind::Footway, SIZE)], None);
        assert!(!paving.is_empty(), "the fixture paved nothing");

        let before = covered(&paving, SIZE, N);
        let road = covered(&roads, SIZE, N);
        let shared_before = before.iter().zip(&road).filter(|(p, r)| **p && **r).count();
        assert!(
            shared_before > 10_000,
            "the fixture does not overlap, so it asserts nothing: {shared_before} texels"
        );

        cut_paving_under_roads(&mut paving, &roads);
        let after = covered(&paving, SIZE, N);
        let shared_after = after.iter().zip(&road).filter(|(p, r)| **p && **r).count();

        // Not zero: the cut runs on a millimetre lattice and this samples texel
        // centres, so a sliver survives along the seam. Well under a percent is the
        // same residual `no_two_groups_share_ground` allows, and is the difference
        // between a seam and a layer.
        let pct = 100.0 * shared_after as f32 / shared_before as f32;
        assert!(
            pct < 1.0,
            "{shared_after} of {shared_before} shared texels survived the cut ({pct:.1} %)"
        );

        // And the pavement that was never under the road is untouched — a cut that
        // took the whole sheet would also pass the assertion above.
        let kept = after.iter().zip(&road).filter(|(p, r)| **p && !**r).count();
        let kept_before = before
            .iter()
            .zip(&road)
            .filter(|(p, r)| **p && !**r)
            .count();
        assert!(
            kept as f32 > kept_before as f32 * 0.99,
            "the cut took pavement the road never covered: {kept_before} -> {kept}"
        );
    }

    /// The roads are blockers, not subjects. They are already a partition among
    /// themselves and `road_stitch` deliberately leaves a ribbon's own bevel
    /// self-overlap alone, so re-deciding it here would be a second answer to a
    /// settled question.
    #[test]
    fn cutting_the_paving_leaves_the_roads_exactly_as_they_were() {
        let roads = vec![road_quad(12.0, 40.0)];
        let mut paving = build_paving_meshes(&[square(PavingKind::Footway, 40.0)], None);
        let before = roads[0].positions.clone();
        cut_paving_under_roads(&mut paving, &roads);
        assert_eq!(roads[0].positions, before);
    }
}
