//! Where two surveyed surfaces meet: kerbs along the carriageway, and worn
//! ground where paving meets green.
//!
//! The BGT is a partition — every patch of ground belongs to exactly one surface,
//! and the line between a pavement and the carriageway beside it is surveyed. The
//! paving layer drew both sides of that line as flat colour meeting flat colour,
//! which is the "everything is square" look of a city builder. Nothing here is
//! invented: a band is only ever laid along a boundary the survey drew, on the
//! side of it the survey says is pavement.
//!
//! * **Kerb.** Where a raised surface (pavement, island, verge) borders a
//!   carriageway or a parking bay, a [`KERB_M`] band of kerbstone is cut out of
//!   the raised side. Not along a `woonerf`, which is level by design, and not
//!   along a driveway, where the kerb is dropped.
//! * **Worn edge.** Where hard paving borders surveyed green (a verge, or a
//!   surface whose material is grass or planting), a [`WORN_M`] band is cut out
//!   of the paving and shaded from soil at the line to the paving's own tone
//!   inward, per vertex.
//!
//! # Cut, never laid over
//!
//! A band is *removed* from its surface and drawn as a surface of its own, so the
//! ground is still a partition and nothing new is left for the depth buffer to
//! arbitrate (`docs/notes/the-ground-stack.md`). The band and what is left of the
//! surface come out of the same integer overlay inputs on a millimetre lattice, so
//! the seam between them is the same points twice and cannot open a crack. The
//! neighbouring surface is not touched at all.
//!
//! # Which side is outside
//!
//! Each boundary piece is probed: step [`PROBE_M`] off it both ways and ask which
//! step lands inside the surface. Deriving it from the ring's winding is the tidy
//! answer and is wrong in a frame where +z runs south — the doors in
//! `systems::interiors` were drawn inside their own walls that way.

use i_overlay::core::fill_rule::FillRule;
use i_overlay::core::overlay::{Overlay, ShapeType};
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::i_float::int::point::IntPoint;
use i_overlay::mesh::stroke::offset::StrokeOffset;
use i_overlay::mesh::style::{LineCap, LineJoin, StrokeStyle};

use crate::paving::{PavedArea, PavingKind, PavingMaterial};

/// Width of a kerbstone's top face, metres. Dutch kerbs are 10–18 cm across.
pub const KERB_M: f32 = 0.15;
/// How far worn ground reaches into the paving from a green edge, metres.
pub const WORN_M: f32 = 0.35;
/// Kerbstone: pale concrete, lighter than the tile beside it.
pub const KERB_RGB: u32 = 0xBC_B8_AF;
/// Soil and moss at the very edge of the paving.
pub const WORN_RGB: u32 = 0x6E_66_4F;

/// How far off a boundary the inside/outside probe steps. Larger than the 10 cm
/// the extractor simplifies to, so two neighbours that disagree by that much
/// still find each other; smaller than the narrowest pavement worth a kerb.
const PROBE_M: f32 = 0.3;
/// A boundary is classified in pieces no longer than this, so a long pavement
/// edge that borders the road for half its length gets a kerb for that half.
const PIECE_M: f32 = 1.0;
/// Lattice for the integer overlay: millimetres.
const LATTICE: f32 = 1000.0;
/// Bucket size of the lookup grid over the cell's surfaces, metres.
const BUCKET_M: f32 = 25.0;

/// What a surface becomes once its edges are worked out.
#[derive(Debug, Clone)]
pub struct Piece {
    pub area: PavedArea,
    pub edge: Option<EdgeKind>,
    /// For a worn band: the boundary runs it is shaded from, in the cell frame.
    pub shade_from: Vec<Vec<[f32; 2]>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    Kerb,
    Worn,
}

fn is_green(a: &PavedArea) -> bool {
    a.kind == PavingKind::Verge
        || matches!(
            a.material,
            Some(PavingMaterial::Grass | PavingMaterial::Planting)
        )
}

/// Stands above the carriageway on a kerb.
fn is_raised(a: &PavedArea) -> bool {
    matches!(
        a.kind,
        PavingKind::Footway | PavingKind::Island | PavingKind::Verge | PavingKind::Steps
    )
}

/// What a kerb faces.
fn is_carriageway(a: &PavedArea) -> bool {
    matches!(
        a.kind,
        PavingKind::Street | PavingKind::Road | PavingKind::Parking
    )
}

/// Hard ground that wears at a green edge.
fn is_hard(a: &PavedArea) -> bool {
    !is_green(a)
        && !matches!(a.material, Some(PavingMaterial::GrassPaver))
        && matches!(
            a.kind,
            PavingKind::Footway
                | PavingKind::Cycleway
                | PavingKind::Parking
                | PavingKind::Drive
                | PavingKind::Island
                | PavingKind::Paved
                | PavingKind::Steps
                | PavingKind::Yard
                | PavingKind::Garden
        )
}

/// Split each surface into the bands along its surveyed edges and the rest.
///
/// A surface with no qualifying edge comes back unchanged and alone.
#[must_use]
pub fn refine(areas: &[PavedArea]) -> Vec<Piece> {
    let grid = Grid::build(areas);
    let mut out = Vec::with_capacity(areas.len());
    for (i, area) in areas.iter().enumerate() {
        let (kerbs, worn) = runs(i, area, areas, &grid);
        let mut rest = vec![area.rings.clone()];
        if !kerbs.is_empty() {
            let (band, left) = split(&rest, &kerbs, KERB_M);
            for rings in band {
                out.push(Piece {
                    area: PavedArea {
                        kind: area.kind,
                        material: Some(PavingMaterial::Concrete),
                        rings,
                        measured: Some(KERB_RGB),
                    },
                    edge: Some(EdgeKind::Kerb),
                    shade_from: Vec::new(),
                });
            }
            rest = left;
        }
        if !worn.is_empty() {
            let (band, left) = split(&rest, &worn, WORN_M);
            for rings in band {
                out.push(Piece {
                    area: PavedArea {
                        rings,
                        ..area.clone()
                    },
                    edge: Some(EdgeKind::Worn),
                    shade_from: worn.clone(),
                });
            }
            rest = left;
        }
        for rings in rest {
            out.push(Piece {
                area: PavedArea {
                    rings,
                    ..area.clone()
                },
                edge: None,
                shade_from: Vec::new(),
            });
        }
    }
    out
}

/// The kerb and worn runs along one surface's boundary, as open polylines.
fn runs(
    me: usize,
    area: &PavedArea,
    all: &[PavedArea],
    grid: &Grid,
) -> (Vec<Vec<[f32; 2]>>, Vec<Vec<[f32; 2]>>) {
    let wants_kerb = is_raised(area);
    let wants_worn = is_hard(area);
    let (mut kerbs, mut worn) = (Vec::new(), Vec::new());
    if !wants_kerb && !wants_worn {
        return (kerbs, worn);
    }
    for ring in &area.rings {
        if ring.len() < 3 {
            continue;
        }
        // Every piece of the ring in order, with what lies beyond it.
        let mut pieces: Vec<([f32; 2], [f32; 2], Option<EdgeKind>)> = Vec::new();
        for k in 0..ring.len() {
            let (a, b) = (ring[k], ring[(k + 1) % ring.len()]);
            let len = dist(a, b);
            if len < 1e-3 {
                continue;
            }
            let n = (len / PIECE_M).ceil().max(1.0) as usize;
            let normal = [-(b[1] - a[1]) / len, (b[0] - a[0]) / len];
            for s in 0..n {
                let p = lerp(a, b, s as f32 / n as f32);
                let q = lerp(a, b, (s + 1) as f32 / n as f32);
                let m = lerp(p, q, 0.5);
                let class = outside(area, m, normal)
                    .and_then(|o| grid.neighbour(all, me, o))
                    .and_then(|nb| {
                        if wants_kerb && is_carriageway(nb) {
                            Some(EdgeKind::Kerb)
                        } else if wants_worn && is_green(nb) {
                            Some(EdgeKind::Worn)
                        } else {
                            None
                        }
                    });
                pieces.push((p, q, class));
            }
        }
        // Rotate so the walk starts at a change of class, so no run is split
        // across the ring's arbitrary first vertex.
        if let Some(start) = (0..pieces.len())
            .find(|&i| pieces[i].2 != pieces[(i + pieces.len() - 1) % pieces.len()].2)
        {
            pieces.rotate_left(start);
        }
        let mut current: Option<(EdgeKind, Vec<[f32; 2]>)> = None;
        let mut flush = |c: Option<(EdgeKind, Vec<[f32; 2]>)>| {
            if let Some((kind, line)) = c {
                match kind {
                    EdgeKind::Kerb => kerbs.push(line),
                    EdgeKind::Worn => worn.push(line),
                }
            }
        };
        for (p, q, class) in pieces {
            match (&mut current, class) {
                (Some((k, line)), Some(c)) if *k == c => line.push(q),
                (_, Some(c)) => flush(current.replace((c, vec![p, q]))),
                (_, None) => flush(current.take()),
            }
        }
        flush(current);
    }
    (kerbs, worn)
}

/// The point just beyond the boundary at `m`, on whichever side is not inside
/// the surface — or `None` where both or neither are (a sliver too thin to say).
fn outside(area: &PavedArea, m: [f32; 2], normal: [f32; 2]) -> Option<[f32; 2]> {
    let a = [m[0] + normal[0] * PROBE_M, m[1] + normal[1] * PROBE_M];
    let b = [m[0] - normal[0] * PROBE_M, m[1] - normal[1] * PROBE_M];
    match (contains(&area.rings, a), contains(&area.rings, b)) {
        (false, true) => Some(a),
        (true, false) => Some(b),
        _ => None,
    }
}

/// `rings` split into the part within `width` of `lines` and the part beyond,
/// each as polygons (outer ring first). Both halves come from the same integer
/// inputs, so their shared edge is the same points twice.
fn split(
    rings: &[Vec<Vec<[f32; 2]>>],
    lines: &[Vec<[f32; 2]>],
    width: f32,
) -> (Vec<Vec<Vec<[f32; 2]>>>, Vec<Vec<Vec<[f32; 2]>>>) {
    let paths: Vec<Vec<[f64; 2]>> = lines
        .iter()
        .map(|l| {
            l.iter()
                .map(|p| [f64::from(p[0]), f64::from(p[1])])
                .collect()
        })
        .collect();
    // Twice the band: the stroke is centred on the line, and only the half that
    // lies inside the surface survives the intersection.
    let style = StrokeStyle::new(f64::from(width) * 2.0)
        .start_cap(LineCap::Butt)
        .end_cap(LineCap::Butt)
        .line_join(LineJoin::Bevel);
    let stroke = paths.stroke(style, false);
    let clip: Vec<Vec<IntPoint<i32>>> = stroke
        .iter()
        .flatten()
        .map(|c| {
            c.iter()
                .map(|p| to_int([p[0] as f32, p[1] as f32]))
                .collect()
        })
        .collect();
    let subject: Vec<Vec<IntPoint<i32>>> = rings
        .iter()
        .flatten()
        .map(|r| r.iter().map(|&p| to_int(p)).collect())
        .collect();
    let run = |rule: OverlayRule| -> Vec<Vec<Vec<[f32; 2]>>> {
        let mut overlay: Overlay<i32> = Overlay::new(64);
        for c in &subject {
            overlay.add_contour(c, ShapeType::Subject);
        }
        for c in &clip {
            overlay.add_contour(c, ShapeType::Clip);
        }
        overlay
            .overlay(rule, FillRule::NonZero)
            .into_iter()
            .map(|shape| {
                shape
                    .into_iter()
                    .map(|c| c.into_iter().map(from_int).collect::<Vec<_>>())
                    .filter(|c: &Vec<[f32; 2]>| c.len() >= 3)
                    .collect::<Vec<_>>()
            })
            .filter(|s: &Vec<Vec<[f32; 2]>>| !s.is_empty())
            .collect()
    };
    (run(OverlayRule::Intersect), run(OverlayRule::Difference))
}

/// The kerbstones to stand up along this cell's surveyed kerb lines: the same
/// runs [`refine`] cuts its kerb bands along, moved half a kerb onto the raised
/// side so a box of [`KERB_M`] width sits exactly over its band.
///
/// The inward side is probed per segment, for the reason the module note gives.
#[must_use]
pub fn kerb_lines(areas: &[PavedArea]) -> Vec<Vec<[f32; 2]>> {
    let grid = Grid::build(areas);
    let half = KERB_M / 2.0;
    let mut out = Vec::new();
    for (i, area) in areas.iter().enumerate() {
        let (kerbs, _) = runs(i, area, areas, &grid);
        for line in kerbs {
            // Each segment's unit normal into the surface.
            let normals: Vec<[f32; 2]> = line
                .windows(2)
                .map(|w| {
                    let len = dist(w[0], w[1]).max(1e-6);
                    let n = [-(w[1][1] - w[0][1]) / len, (w[1][0] - w[0][0]) / len];
                    let m = lerp(w[0], w[1], 0.5);
                    let probe = [m[0] + n[0] * 0.05, m[1] + n[1] * 0.05];
                    if contains(&area.rings, probe) {
                        n
                    } else {
                        [-n[0], -n[1]]
                    }
                })
                .collect();
            if normals.is_empty() {
                continue;
            }
            // Each point moves along the mean of the normals either side of it.
            let moved = line
                .iter()
                .enumerate()
                .map(|(k, p)| {
                    let a = normals[k.saturating_sub(1).min(normals.len() - 1)];
                    let b = normals[k.min(normals.len() - 1)];
                    let (sx, sz) = (a[0] + b[0], a[1] + b[1]);
                    let l = (sx * sx + sz * sz).sqrt().max(1e-6);
                    [p[0] + sx / l * half, p[1] + sz / l * half]
                })
                .collect();
            out.push(moved);
        }
    }
    out
}

/// How far `p` lies from the nearest of `lines`, metres.
#[must_use]
pub fn distance_to(lines: &[Vec<[f32; 2]>], p: [f32; 2]) -> f32 {
    let mut best = f32::INFINITY;
    for line in lines {
        for w in line.windows(2) {
            best = best.min(segment_distance(p, w[0], w[1]));
        }
    }
    best
}

fn segment_distance(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let ab = [b[0] - a[0], b[1] - a[1]];
    let len2 = ab[0] * ab[0] + ab[1] * ab[1];
    let t = if len2 > 0.0 {
        (((p[0] - a[0]) * ab[0] + (p[1] - a[1]) * ab[1]) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    dist(p, [a[0] + ab[0] * t, a[1] + ab[1] * t])
}

fn to_int(p: [f32; 2]) -> IntPoint<i32> {
    IntPoint::new(
        (p[0] * LATTICE).round() as i32,
        (p[1] * LATTICE).round() as i32,
    )
}

fn from_int(p: IntPoint<i32>) -> [f32; 2] {
    [p.x as f32 / LATTICE, p.y as f32 / LATTICE]
}

fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

fn lerp(a: [f32; 2], b: [f32; 2], t: f32) -> [f32; 2] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}

/// Even-odd point in polygon over all rings, so holes are outside.
fn contains(rings: &[Vec<[f32; 2]>], p: [f32; 2]) -> bool {
    let mut inside = false;
    for ring in rings {
        let n = ring.len();
        for i in 0..n {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            if (a[1] > p[1]) != (b[1] > p[1])
                && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0]
            {
                inside = !inside;
            }
        }
    }
    inside
}

/// Which surfaces' bounding boxes reach each bucket of the cell.
struct Grid {
    boxes: Vec<[f32; 4]>,
    buckets: std::collections::HashMap<(i32, i32), Vec<usize>>,
}

impl Grid {
    fn build(areas: &[PavedArea]) -> Self {
        let mut boxes = Vec::with_capacity(areas.len());
        let mut buckets: std::collections::HashMap<(i32, i32), Vec<usize>> =
            std::collections::HashMap::new();
        for (i, a) in areas.iter().enumerate() {
            let mut b = [
                f32::INFINITY,
                f32::INFINITY,
                f32::NEG_INFINITY,
                f32::NEG_INFINITY,
            ];
            for p in a.rings.iter().flatten() {
                b = [
                    b[0].min(p[0]),
                    b[1].min(p[1]),
                    b[2].max(p[0]),
                    b[3].max(p[1]),
                ];
            }
            boxes.push(b);
            if !b[0].is_finite() {
                continue;
            }
            let k = |v: f32| (v / BUCKET_M).floor() as i32;
            for x in k(b[0])..=k(b[2]) {
                for y in k(b[1])..=k(b[3]) {
                    buckets.entry((x, y)).or_default().push(i);
                }
            }
        }
        Self { boxes, buckets }
    }

    /// The surface other than `me` that `p` falls in, if any.
    fn neighbour<'a>(&self, all: &'a [PavedArea], me: usize, p: [f32; 2]) -> Option<&'a PavedArea> {
        let key = (
            (p[0] / BUCKET_M).floor() as i32,
            (p[1] / BUCKET_M).floor() as i32,
        );
        self.buckets.get(&key)?.iter().copied().find_map(|i| {
            let b = self.boxes[i];
            (i != me
                && p[0] >= b[0]
                && p[0] <= b[2]
                && p[1] >= b[1]
                && p[1] <= b[3]
                && contains(&all[i].rings, p))
            .then(|| &all[i])
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(kind: PavingKind, x0: f32, z0: f32, x1: f32, z1: f32) -> PavedArea {
        PavedArea {
            kind,
            material: None,
            rings: vec![vec![[x0, z0], [x1, z0], [x1, z1], [x0, z1]]],
            measured: None,
        }
    }

    fn area(rings: &[Vec<[f32; 2]>]) -> f32 {
        let ring = |r: &Vec<[f32; 2]>| {
            let mut s = 0.0;
            for i in 0..r.len() {
                let (a, b) = (r[i], r[(i + 1) % r.len()]);
                s += a[0] * b[1] - b[0] * a[1];
            }
            (s / 2.0).abs()
        };
        ring(&rings[0]) - rings[1..].iter().map(ring).sum::<f32>()
    }

    /// A 3 m pavement beside a street gets a kerb exactly along the shared
    /// edge, and the kerb and the rest still cover the pavement exactly once.
    /// Both windings, because the outside is found by probing, not by winding.
    #[test]
    fn a_pavement_beside_a_street_gets_a_kerb_on_that_side_only() {
        for flip in [false, true] {
            let mut pavement = rect(PavingKind::Footway, 0.0, 0.0, 20.0, 3.0);
            if flip {
                pavement.rings[0].reverse();
            }
            let street = rect(PavingKind::Street, 0.0, 3.0, 20.0, 10.0);
            let pieces = refine(&[pavement, street]);
            let kerbs: Vec<_> = pieces
                .iter()
                .filter(|p| p.edge == Some(EdgeKind::Kerb))
                .collect();
            assert_eq!(kerbs.len(), 1, "flip={flip}");
            let kerb = &kerbs[0].area;
            assert!(
                (area(&kerb.rings) - 20.0 * KERB_M).abs() < 0.05,
                "flip={flip}"
            );
            assert!(kerb.rings[0].iter().all(|p| p[1] >= 3.0 - KERB_M - 1e-3));
            let footway: f32 = pieces
                .iter()
                .filter(|p| p.area.kind == PavingKind::Footway)
                .map(|p| area(&p.area.rings))
                .sum();
            assert!(
                (footway - 60.0).abs() < 0.05,
                "the pieces cover the pavement once"
            );
            // The street itself is untouched.
            assert!(pieces.iter().any(|p| p.area.kind == PavingKind::Street
                && p.edge.is_none()
                && (area(&p.area.rings) - 140.0).abs() < 1e-3));
        }
    }

    /// The kerbstone runs along the street edge half a kerb onto the pavement,
    /// whichever way the ring winds.
    #[test]
    fn the_kerbstone_stands_on_the_pavement_side() {
        for flip in [false, true] {
            let mut pavement = rect(PavingKind::Footway, 0.0, 0.0, 20.0, 3.0);
            if flip {
                pavement.rings[0].reverse();
            }
            let street = rect(PavingKind::Street, 0.0, 3.0, 20.0, 10.0);
            let lines = kerb_lines(&[pavement, street]);
            assert_eq!(lines.len(), 1, "flip={flip}");
            for p in &lines[0] {
                assert!(
                    (p[1] - (3.0 - KERB_M / 2.0)).abs() < 1e-3,
                    "flip={flip}: {p:?}"
                );
            }
        }
    }

    /// No kerb along a woonerf, and none where nothing is surveyed beyond.
    #[test]
    fn no_kerb_without_a_carriageway() {
        let pavement = rect(PavingKind::Footway, 0.0, 0.0, 20.0, 3.0);
        let woonerf = rect(PavingKind::LivingStreet, 0.0, 3.0, 20.0, 10.0);
        let pieces = refine(&[pavement, woonerf]);
        assert!(pieces.iter().all(|p| p.edge.is_none()));
        assert_eq!(pieces.len(), 2);
    }

    /// Paving beside a verge wears along that edge, and the band is shaded
    /// from that edge.
    #[test]
    fn paving_beside_a_verge_wears() {
        let pavement = rect(PavingKind::Footway, 0.0, 0.0, 20.0, 3.0);
        let verge = rect(PavingKind::Verge, 0.0, -2.0, 20.0, 0.0);
        let pieces = refine(&[pavement, verge]);
        let worn: Vec<_> = pieces
            .iter()
            .filter(|p| p.edge == Some(EdgeKind::Worn))
            .collect();
        assert_eq!(worn.len(), 1);
        assert!((area(&worn[0].area.rings) - 20.0 * WORN_M).abs() < 0.05);
        assert!(distance_to(&worn[0].shade_from, [10.0, 0.0]) < 1e-3);
        assert!((distance_to(&worn[0].shade_from, [10.0, WORN_M]) - WORN_M).abs() < 1e-3);
    }

    /// Half a pavement's edge on the street and half on a woonerf: the kerb
    /// stops where the street does.
    #[test]
    fn a_kerb_runs_only_as_far_as_the_street() {
        let pavement = rect(PavingKind::Footway, 0.0, 0.0, 20.0, 3.0);
        let street = rect(PavingKind::Street, 0.0, 3.0, 10.0, 10.0);
        let woonerf = rect(PavingKind::LivingStreet, 10.0, 3.0, 20.0, 10.0);
        let pieces = refine(&[pavement, street, woonerf]);
        let kerb: f32 = pieces
            .iter()
            .filter(|p| p.edge == Some(EdgeKind::Kerb))
            .map(|p| area(&p.area.rings))
            .sum();
        assert!((kerb - 10.0 * KERB_M).abs() < 0.05, "{kerb}");
    }
}

/// Run the edge pass over one real cell and draw a plan of it — the check that
/// the kerbs land where the survey's streets are, which no synthetic test can
/// make. `CARTO_BGT_CELL=…/14/x/y.surfaces.json`, `CARTO_PLAN_AT=x,z` (cell
/// metres) and `CARTO_PLAN_OUT=plan.ppm`; prints counts and the time taken.
#[cfg(test)]
mod real_cell {
    use super::*;

    const KINDS: [PavingKind; 12] = [
        PavingKind::Footway,
        PavingKind::Cycleway,
        PavingKind::Parking,
        PavingKind::Drive,
        PavingKind::Verge,
        PavingKind::Island,
        PavingKind::Paved,
        PavingKind::Steps,
        PavingKind::Street,
        PavingKind::LivingStreet,
        PavingKind::Road,
        PavingKind::Yard,
    ];
    const MATERIALS: [PavingMaterial; 12] = [
        PavingMaterial::Asphalt,
        PavingMaterial::Concrete,
        PavingMaterial::Brick,
        PavingMaterial::ConcretePaver,
        PavingMaterial::Tiles,
        PavingMaterial::Stone,
        PavingMaterial::GrassPaver,
        PavingMaterial::Grass,
        PavingMaterial::Planting,
        PavingMaterial::Gravel,
        PavingMaterial::Sand,
        PavingMaterial::Unpaved,
    ];

    #[test]
    #[ignore = "needs a real cell on disk"]
    fn plan_of_a_real_cell() {
        let path = std::env::var("CARTO_BGT_CELL").expect("CARTO_BGT_CELL");
        let (cx, cy) = {
            let mut it = path.trim_end_matches(".surfaces.json").rsplit('/');
            let y: i64 = it.next().unwrap().parse().unwrap();
            let x: i64 = it.next().unwrap().parse().unwrap();
            (x, y)
        };
        let n = f64::from(1u32 << 14);
        let lat_of = |y: f64| {
            (std::f64::consts::PI * (1.0 - 2.0 * y / n))
                .sinh()
                .atan()
                .to_degrees()
        };
        let (north, south) = (lat_of(cy as f64), lat_of(cy as f64 + 1.0));
        let (west, east) = (
            cx as f64 / n * 360.0 - 180.0,
            (cx + 1) as f64 / n * 360.0 - 180.0,
        );
        let tile_m = 40_075_016.7 / n * ((north + south) / 2.0).to_radians().cos();
        let merc = |lat: f64| lat.to_radians().tan().asinh();
        let to_local = |(lat, lng): (f64, f64)| -> [f32; 2] {
            let fx = (lng - west) / (east - west);
            let fz = (merc(north) - merc(lat)) / (merc(north) - merc(south));
            [(fx * tile_m) as f32, (fz * tile_m) as f32]
        };
        let text = std::fs::read_to_string(&path).unwrap();
        let wire = crate::formats::bgt::parse_paving(&text).unwrap();
        let areas: Vec<PavedArea> = wire
            .surfaces
            .iter()
            .filter_map(|s| {
                Some(PavedArea {
                    kind: *KINDS.get(s.0)?,
                    material: s.2.and_then(|m| MATERIALS.get(m).copied()),
                    rings: s
                        .1
                        .iter()
                        .map(|r| r.iter().copied().map(to_local).collect())
                        .collect(),
                    measured: None,
                })
            })
            .collect();
        let t0 = std::time::Instant::now();
        let pieces = refine(&areas);
        let took = t0.elapsed();
        let count = |k| pieces.iter().filter(|p| p.edge == Some(k)).count();
        let len = |k| -> f32 {
            pieces
                .iter()
                .filter(|p| p.edge == Some(k))
                .map(|p| {
                    let w = if k == EdgeKind::Kerb { KERB_M } else { WORN_M };
                    let r = &p.area.rings[0];
                    let mut s = 0.0;
                    for i in 0..r.len() {
                        let (a, b) = (r[i], r[(i + 1) % r.len()]);
                        s += a[0] * b[1] - b[0] * a[1];
                    }
                    (s / 2.0).abs() / w
                })
                .sum()
        };
        println!(
            "{} surfaces -> {} pieces in {took:?}; kerb bands {} (~{:.0} m), worn bands {} (~{:.0} m)",
            areas.len(),
            pieces.len(),
            count(EdgeKind::Kerb),
            len(EdgeKind::Kerb),
            count(EdgeKind::Worn),
            len(EdgeKind::Worn),
        );
        let Ok(at) = std::env::var("CARTO_PLAN_AT") else {
            return;
        };
        let at: Vec<f32> = at.split(',').map(|v| v.parse().unwrap()).collect();
        let (size, span) = (600usize, 50.0f32);
        let mut img = vec![[255u8, 255, 255]; size * size];
        for p in &pieces {
            let near = p.area.rings[0]
                .iter()
                .any(|q| (q[0] - at[0]).abs() < span && (q[1] - at[1]).abs() < span);
            if !near {
                continue;
            }
            let rgb = match p.edge {
                Some(EdgeKind::Kerb) => 0xE0_20_20,
                Some(EdgeKind::Worn) => 0x8A_5A_20,
                None => crate::paving::area_rgb(p.area.kind, p.area.material),
            };
            let c = [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8];
            for (i, px) in img.iter_mut().enumerate() {
                let q = [
                    at[0] - span / 2.0 + (i % size) as f32 * span / size as f32,
                    at[1] - span / 2.0 + (i / size) as f32 * span / size as f32,
                ];
                if contains(&p.area.rings, q) {
                    *px = c;
                }
            }
        }
        let mut out = format!("P6 {size} {size} 255\n").into_bytes();
        out.extend(img.iter().flatten());
        std::fs::write(std::env::var("CARTO_PLAN_OUT").unwrap(), out).unwrap();
    }
}
