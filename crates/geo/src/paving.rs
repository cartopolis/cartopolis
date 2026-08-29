//! The ground beside the carriageway, as it was surveyed: pavements, cycle
//! paths, parking bays, driveways, verges and traffic islands.
//!
//! No Bevy: rings in, [`SurfaceMesh`]es out. The extraction is
//! `tools/bgt_objects.py` and the streaming is `systems::surveyed`.
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

use crate::vector_tiles::{palette, SurfaceGroup, SurfaceMesh, INDEX_BYTES, VERTEX_BYTES};

/// A surveyed paved surface, in tile-local metres.
#[derive(Debug, Clone)]
pub struct PavedArea {
    pub kind: PavingKind,
    /// Outer ring first, holes after. Open rings — no repeated closing vertex.
    pub rings: Vec<Vec<[f32; 2]>>,
}

/// The paved forms. Mirrors `SURFACE_KINDS` in `tools/bgt_objects.py` —
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
}

/// What one triangle costs to upload, in the units the frame budget is in.
const TRI_BYTES: usize = 3 * VERTEX_BYTES + 3 * INDEX_BYTES;

/// Triangulate a cell's paving into merged meshes, each no larger than
/// `max_bytes`.
///
/// One group for all of it, like the furniture and the canopies: the kind rides
/// in the vertex colour.
#[must_use]
pub fn build_paving_meshes(areas: &[PavedArea], max_bytes: Option<usize>) -> Vec<SurfaceMesh> {
    if areas.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut buf = Buf::default();
    for area in areas {
        let Some(outer) = area.rings.first() else {
            continue;
        };
        if outer.len() < 3 {
            continue;
        }
        // earcut takes one flat coordinate list with the holes appended and their
        // start indices given separately.
        let mut flat: Vec<f64> = Vec::with_capacity(outer.len() * 2);
        for p in outer {
            flat.push(f64::from(p[0]));
            flat.push(f64::from(p[1]));
        }
        let mut holes = Vec::new();
        for ring in &area.rings[1..] {
            if ring.len() < 3 {
                continue;
            }
            holes.push(flat.len() / 2);
            for p in ring {
                flat.push(f64::from(p[0]));
                flat.push(f64::from(p[1]));
            }
        }
        let Ok(tris) = earcutr::earcut(&flat, &holes, 2) else {
            continue;
        };
        if let Some(max) = max_bytes {
            let step = tris.len() / 3 * TRI_BYTES;
            if buf.bytes() + step > max && !buf.positions.is_empty() {
                out.push(std::mem::take(&mut buf).finish());
            }
        }
        let tint = srgb_to_linear(area.kind.rgb());
        // Unshared vertices per triangle. A shared-vertex mesh would be smaller,
        // but every surface here is flat and faces up, so there is no shading to
        // gain from sharing and the winding fix below needs to own its corners.
        for t in tris.chunks_exact(3) {
            let p = |i: usize| -> [f32; 3] { [flat[i * 2] as f32, 0.0, flat[i * 2 + 1] as f32] };
            let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
            // Earcut keeps the input ring's orientation, and the survey does not
            // promise one. A ground surface always faces up, so wind for +Y —
            // the same rule `tessellate_polygon` follows, and the same failure if
            // it is skipped: half the paving in the city is invisible from above
            // and visible from below.
            let cross = (b[0] - a[0]) * (c[2] - a[2]) - (b[2] - a[2]) * (c[0] - a[0]);
            let base = buf.positions.len() as u32;
            if cross < 0.0 {
                buf.push(a, tint);
                buf.push(b, tint);
                buf.push(c, tint);
            } else {
                buf.push(a, tint);
                buf.push(c, tint);
                buf.push(b, tint);
            }
            buf.indices.extend([base, base + 1, base + 2]);
        }
    }
    if !buf.positions.is_empty() {
        out.push(buf.finish());
    }
    out
}

#[derive(Default)]
struct Buf {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    colors: Vec<[f32; 4]>,
    indices: Vec<u32>,
}

impl Buf {
    fn push(&mut self, p: [f32; 3], tint: [f32; 4]) {
        self.positions.push(p);
        self.normals.push([0.0, 1.0, 0.0]);
        self.colors.push(tint);
    }

    fn bytes(&self) -> usize {
        self.positions.len() * VERTEX_BYTES + self.indices.len() * INDEX_BYTES
    }

    fn finish(self) -> SurfaceMesh {
        SurfaceMesh {
            group: SurfaceGroup::Paving,
            positions: self.positions,
            indices: self.indices,
            uvs: Vec::new(),
            normals: self.normals,
            colors: self.colors,
            rungs: Vec::new(),
        }
    }
}

/// One sRGB colour as a linear vertex tint.
///
/// Linear because that is where the shader multiplies it into the group's white
/// base colour — the same exit every other per-vertex palette here takes.
fn srgb_to_linear(rgb: u32) -> [f32; 4] {
    let c = |shift: u32| {
        let v = f32::from((rgb >> shift & 0xFF) as u8) / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    [c(16), c(8), c(0), 1.0]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(kind: PavingKind, size: f32) -> PavedArea {
        PavedArea {
            kind,
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
}
