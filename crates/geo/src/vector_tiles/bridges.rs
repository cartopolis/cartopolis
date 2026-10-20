//! Bridge decks: slabs built from bridge outlines and from bridge-flagged ways, and the rule
//! for which ground a bridge's side walls may stand on.

use super::*;

/// One bridge deck: a `bridges` polygon, tessellated but deliberately **not**
/// merged with its neighbours.
///
/// Every other surface here is batched into one mesh per group per cell, and this
/// is the exception, because each deck needs its own *height* — a number that does
/// not exist in the tile and arrives later, per deck, from `systems::height`. So
/// the worker does the tessellation and hands back the pieces; the main thread
/// lifts each one to its measured height and merges them then.
///
/// The rings come back too. They are what the deck's fascia is extruded from, and
/// what says whether a sample of AHN — or a road ribbon — is *on* this deck.
#[derive(Debug, Clone)]
pub struct BridgeDeck {
    /// Deck surface triangles, tile-local metres at `y = 0`.
    pub positions: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
    /// Outline rings, clipped and closed (exterior first, then holes).
    pub rings: Vec<Vec<[f32; 2]>>,
    /// Tile-local bounding box of the outline.
    pub min: [f32; 2],
    pub max: [f32; 2],
}

impl BridgeDeck {
    /// Is this tile-local point on the deck? Even-odd crossing count over every
    /// ring, so a hole in the structure counts as off it.
    #[must_use]
    pub fn contains(&self, p: [f32; 2]) -> bool {
        if p[0] < self.min[0] || p[0] > self.max[0] || p[1] < self.min[1] || p[1] > self.max[1] {
            return false;
        }
        let mut inside = false;
        for ring in &self.rings {
            for e in ring.windows(2) {
                let (a, b) = (e[0], e[1]);
                if (a[1] > p[1]) != (b[1] > p[1]) {
                    let t = (p[1] - a[1]) / (b[1] - a[1]);
                    if p[0] < a[0] + t * (b[0] - a[0]) {
                        inside = !inside;
                    }
                }
            }
        }
        inside
    }

    /// Longest side of the deck's footprint, in metres — how a caller sizes a
    /// height sample grid to the thing being measured.
    #[must_use]
    pub fn span_m(&self) -> f32 {
        (self.max[0] - self.min[0]).max(self.max[1] - self.min[1])
    }
}

/// Tessellate the `bridges` layer: one [`BridgeDeck`] per structure.
///
/// Shortbread's `bridges` layer is **areas**, not lines — the outline of the
/// structure itself, carriageway and parapets and all. The raster path only ever
/// stroked them as a hairline and the geometry path ignored them entirely, which is
/// why a viaduct was nothing but the road ribbon crossing it. Measured over
/// Groningen station: the Emmaviaduct arrives as 34 × 149 m and 68 × 176 m of
/// footprint. That is a deck you can stand geometry on.
///
/// **Not every bridge has an area.** Where OSM maps the bridge-flagged ways and
/// no `man_made=bridge` outline, the structure is built from the ways
/// themselves ([`street_decks`]) and follows the outlines in the list.
pub fn tessellate_bridges(tile: &TileData, tile_m: f32) -> Result<Vec<BridgeDeck>, String> {
    let mut decks = area_decks(tile, tile_m);
    let from_streets = street_decks(tile, tile_m, &decks);
    decks.extend(from_streets);
    Ok(decks)
}

/// The decks the `bridges` layer draws as areas.
fn area_decks(tile: &TileData, tile_m: f32) -> Vec<BridgeDeck> {
    let Some(meta) = tile.layer("bridges") else {
        return Vec::new();
    };
    let features = tile.features(meta);
    let extent = TileData::extent(meta);
    let scale = tile_m / extent;

    let mut decks = Vec::new();
    for feature in features {
        let mut polys: Vec<&geo_types::Polygon<f32>> = Vec::new();
        match &feature.geometry {
            Geometry::Polygon(p) => polys.push(p),
            Geometry::MultiPolygon(mp) => polys.extend(mp.iter()),
            _ => continue,
        }
        for poly in polys {
            let mut positions = Vec::new();
            let mut indices = Vec::new();
            tessellate_polygon(poly, scale, extent, &mut positions, &mut indices);
            let rings = clipped_rings(poly, scale, extent);
            if indices.is_empty() || rings.is_empty() {
                continue;
            }
            let mut min = [f32::MAX, f32::MAX];
            let mut max = [f32::MIN, f32::MIN];
            for p in rings.iter().flatten() {
                min = [min[0].min(p[0]), min[1].min(p[1])];
                max = [max[0].max(p[0]), max[1].max(p[1])];
            }
            decks.push(BridgeDeck {
                positions,
                indices,
                rings,
                min,
                max,
            });
        }
    }
    decks
}

/// How far a deck built from a street reaches past the carriageway on either
/// side, metres: the edge beam and the parapet an outline would have included.
const STREET_DECK_MARGIN_M: f32 = 1.0;

/// Shortest stretch of a bridge-flagged street that is given a deck of its own,
/// metres. Below it the stretch is a sliver of a way overhanging an outline, or a
/// culvert nobody would call a bridge.
const STREET_DECK_MIN_M: f32 = 6.0;

/// Smallest deck built from a street, square metres — a footpath's
/// [`STREET_DECK_MIN_M`] of span is about fifteen.
const STREET_DECK_MIN_AREA_M2: f32 = 8.0;

/// **Decks for the bridges OSM maps only as flagged ways.** Shortbread's `bridges`
/// layer is the `man_made=bridge` areas, and many bridges have none: the
/// Europaplein flyover in Groningen, where the ring road crosses the junction
/// (reported 2026-09-26), is four `bridge=yes` carriageways and no outline. With
/// no deck under them nothing measured their height and nothing lifted them, so
/// the flyover was drawn on the ground across the roads it passes over.
///
/// Each stretch of a bridge-flagged road or railway that no outline covers is buffered
/// into one: the carriageway's own width plus [`STREET_DECK_MARGIN_M`] either
/// side, round at the bends and square at the ends, where the way — and so the
/// structure — stops. Overlapping stretches are merged, so an interchange's slip
/// road joining the main span is one deck rather than two slabs fighting over the
/// same ground, and the result is clipped to the tile the way an outline is.
///
/// From here a deck is a deck: `split_by_deck` puts the ways on it,
/// `extend_approaches` finds their ramps and the height is measured over the
/// outline as it is for any other — which is also what keeps the canal bridges
/// flat, since they measure under `MIN_LIFT_M`.
fn street_decks(tile: &TileData, tile_m: f32, areas: &[BridgeDeck]) -> Vec<BridgeDeck> {
    use i_overlay::core::fill_rule::FillRule;
    use i_overlay::core::overlay::{Overlay, ShapeType};
    use i_overlay::core::overlay_rule::OverlayRule;
    use i_overlay::i_float::int::point::IntPoint;

    let Some(meta) = tile.layer("streets") else {
        return Vec::new();
    };
    let features = tile.features(meta);
    let extent = TileData::extent(meta);
    let scale = tile_m / extent;
    let mm = |p: [f32; 2]| {
        IntPoint::new(
            (p[0] * 1000.0).round() as i32,
            (p[1] * 1000.0).round() as i32,
        )
    };
    // Clipped to the tile the way an outline is — every piece is convex, so
    // Sutherland–Hodgman is exact — and given one winding: under the non-zero rule
    // a clockwise piece over an anticlockwise one would cancel into a hole
    // instead of merging.
    let contour = |pts: Vec<[f32; 2]>| -> Vec<IntPoint<i32>> {
        let pts = clip_ring(&pts, tile_m);
        let twice_area: f32 = pts
            .iter()
            .zip(pts.iter().cycle().skip(1))
            .map(|(a, b)| a[0] * b[1] - b[0] * a[1])
            .sum();
        let mut out: Vec<IntPoint<i32>> = pts.into_iter().map(mm).collect();
        if twice_area < 0.0 {
            out.reverse();
        }
        out
    };

    let mut overlay: Overlay<i32> = Overlay::new(64);
    let mut any = false;
    let mut add = |overlay: &mut Overlay<i32>, pts: Vec<[f32; 2]>| {
        let c = contour(pts);
        if c.len() >= 3 {
            overlay.add_contour(&c, ShapeType::Subject);
            any = true;
        }
    };
    for feature in features {
        if !flag_of(feature, "bridge") {
            continue;
        }
        // Main roads and rail only. A path or a lane is narrow and routinely runs
        // under trees, and the survey over a strip that narrow is the canopy:
        // Groningen's boardwalks and Stadspark lanes measured 11–23 m
        // (2026-09-26), which would have stood them on slabs over the park.
        let Some((class, half)) = drawn_street(feature) else {
            continue;
        };
        if matches!(class, RoadClass::Path | RoadClass::Minor) {
            continue;
        }
        let w = half + STREET_DECK_MARGIN_M;
        let lines: Vec<&geo_types::LineString<f32>> = match &feature.geometry {
            Geometry::LineString(ls) => vec![ls],
            Geometry::MultiLineString(mls) => mls.iter().collect(),
            _ => continue,
        };
        for ls in lines {
            let pts: Vec<[f32; 2]> =
                ls.0.iter()
                    .filter(|c| c.x.is_finite() && c.y.is_finite())
                    .map(|c| [c.x, c.y])
                    .collect();
            let whole_m = crate::tunnels::polyline_len(&pts) * scale;
            if whole_m < STREET_DECK_MIN_M {
                continue;
            }
            for run in clip_polyline(&pts, 0.0, extent) {
                // Asked every couple of metres, because `split_by_deck` only asks
                // at the vertices: a way whose two ends lie just off an outline
                // and whose whole span crosses it otherwise reads as never
                // touching it, and was given a second deck over the first.
                let mut metres: Vec<[f32; 2]> = Vec::new();
                for s in run.windows(2) {
                    let (a, b) = (
                        [s[0][0] * scale, s[0][1] * scale],
                        [s[1][0] * scale, s[1][1] * scale],
                    );
                    let steps = ((b[0] - a[0]).hypot(b[1] - a[1]) / 2.0).ceil().max(1.0) as usize;
                    metres.extend((0..steps).map(|k| {
                        let t = k as f32 / steps as f32;
                        [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
                    }));
                }
                if let Some(&last) = run.last() {
                    metres.push([last[0] * scale, last[1] * scale]);
                }
                // A run cut by the tile border is short here and continues next
                // door, where the rest of the same structure is built.
                let at_border = |p: [f32; 2]| {
                    p[0] <= 0.01 || p[1] <= 0.01 || p[0] >= tile_m - 0.01 || p[1] >= tile_m - 0.01
                };
                for (owner, part) in split_by_deck(&metres, areas) {
                    if owner.is_some() || part.len() < 2 {
                        continue;
                    }
                    let clipped = part.first().is_some_and(|p| at_border(*p))
                        || part.last().is_some_and(|p| at_border(*p));
                    if crate::tunnels::polyline_len(&part) < STREET_DECK_MIN_M && !clipped {
                        continue;
                    }
                    // Back to the way's own bends before it is buffered. One
                    // rectangle per two-metre sample meets its neighbour on an edge
                    // computed twice, and after rounding the two copies disagree by
                    // a millimetre: the union came back full of zero-width spikes
                    // and slits, and the fascia hung a sliver of wall from every one
                    // (the Europaplein deck, 2026-09-27).
                    let part = without_collinear(&part);
                    for s in part.windows(2) {
                        let (a, b) = (s[0], s[1]);
                        let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
                        let len = dx.hypot(dz);
                        if len < 1e-3 {
                            continue;
                        }
                        let n = [-dz / len * w, dx / len * w];
                        add(
                            &mut overlay,
                            vec![
                                [a[0] + n[0], a[1] + n[1]],
                                [b[0] + n[0], b[1] + n[1]],
                                [b[0] - n[0], b[1] - n[1]],
                                [a[0] - n[0], a[1] - n[1]],
                            ],
                        );
                    }
                    // Round joins, so a bend is not notched on its outside —
                    // only where it bends, not at every sample laid above.
                    for s in part.windows(3) {
                        let (u, v) = (
                            [s[1][0] - s[0][0], s[1][1] - s[0][1]],
                            [s[2][0] - s[1][0], s[2][1] - s[1][1]],
                        );
                        let turn = (u[0] * v[1] - u[1] * v[0]).abs()
                            / (u[0].hypot(u[1]) * v[0].hypot(v[1])).max(1e-6);
                        if turn < 1e-3 {
                            continue;
                        }
                        let c = s[1];
                        let disc = (0..12)
                            .map(|k| {
                                let t = k as f32 * std::f32::consts::TAU / 12.0;
                                [c[0] + w * t.cos(), c[1] + w * t.sin()]
                            })
                            .collect();
                        add(&mut overlay, disc);
                    }
                }
            }
        }
    }
    if !any {
        return Vec::new();
    }
    // Less the outlines, so no ground is claimed by two decks: a street deck
    // only meets an outline where the way runs off it, and there the two must
    // share an edge rather than overlap at two measured heights.
    for deck in areas {
        for ring in &deck.rings {
            let pts: Vec<IntPoint<i32>> = ring[..ring.len() - 1].iter().map(|p| mm(*p)).collect();
            overlay.add_contour(&pts, ShapeType::Clip);
        }
    }

    let mut decks = Vec::new();
    for shape in overlay.overlay(OverlayRule::Difference, FillRule::NonZero) {
        let rings: Vec<Vec<[f32; 2]>> = shape
            .iter()
            .map(|c| {
                without_spikes(
                    c.iter()
                        .map(|p| [p.x as f32 / 1000.0, p.y as f32 / 1000.0])
                        .collect(),
                )
            })
            .filter(|r| r.len() >= 3)
            .map(|mut ring| {
                ring.push(ring[0]);
                ring
            })
            .collect();
        // What the subtraction leaves along an outline's edge where a way ran
        // onto it is a sliver, not a structure.
        let area = rings.first().map_or(0.0, |r| {
            0.5 * r
                .windows(2)
                .map(|e| e[0][0] * e[1][1] - e[1][0] * e[0][1])
                .sum::<f32>()
                .abs()
        });
        if area < STREET_DECK_MIN_AREA_M2 {
            continue;
        }
        let mut flat: Vec<f64> = Vec::new();
        let mut holes: Vec<usize> = Vec::new();
        for (ri, ring) in rings.iter().enumerate() {
            if ri > 0 {
                holes.push(flat.len() / 2);
            }
            for p in &ring[..ring.len() - 1] {
                flat.extend([f64::from(p[0]), f64::from(p[1])]);
            }
        }
        let Ok(tris) = earcutr::earcut(&flat, &holes, 2) else {
            continue;
        };
        let positions: Vec<[f32; 3]> = flat
            .chunks_exact(2)
            .map(|p| [p[0] as f32, 0.0, p[1] as f32])
            .collect();
        let mut indices: Vec<u32> = Vec::with_capacity(tris.len());
        for t in tris.chunks_exact(3) {
            let [a, b, c] = [positions[t[0]], positions[t[1]], positions[t[2]]];
            // Facing up, as every ground triangle out of `tessellate_polygon` does;
            // the union keeps no winding of its own worth trusting.
            let cross = (b[0] - a[0]) * (c[2] - a[2]) - (b[2] - a[2]) * (c[0] - a[0]);
            if cross.abs() < f32::EPSILON {
                continue;
            }
            #[allow(clippy::cast_possible_truncation)]
            let t: [u32; 3] = [t[0] as u32, t[1] as u32, t[2] as u32];
            indices.extend(if cross < 0.0 { t } else { [t[0], t[2], t[1]] });
        }
        if indices.is_empty() {
            continue;
        }
        let mut min = [f32::MAX, f32::MAX];
        let mut max = [f32::MIN, f32::MIN];
        for p in rings.iter().flatten() {
            min = [min[0].min(p[0]), min[1].min(p[1])];
            max = [max[0].max(p[0]), max[1].max(p[1])];
        }
        decks.push(BridgeDeck {
            positions,
            indices,
            rings,
            min,
            max,
        });
    }
    decks
}

/// An open ring with its zero-width spikes removed: every vertex the outline
/// runs out to and straight back from.
///
/// Where two carriageways' square-cut ends meet at an angle, the union of their
/// buffers keeps the sliver between them as a slit a few centimetres wide, and
/// the fascia is extruded from every edge of the ring — so the Europaplein deck
/// hung a sliver of wall from each (2026-09-27). A spike covers no ground, so
/// dropping it changes nothing the deck stands over.
fn without_spikes(mut ring: Vec<[f32; 2]>) -> Vec<[f32; 2]> {
    loop {
        let n = ring.len();
        if n < 3 {
            return ring;
        }
        let spike = (0..n).find(|&k| {
            let (a, b, c) = (ring[(k + n - 1) % n], ring[k], ring[(k + 1) % n]);
            let (u, v) = ([b[0] - a[0], b[1] - a[1]], [c[0] - b[0], c[1] - b[1]]);
            let (lu, lv) = (u[0].hypot(u[1]), v[0].hypot(v[1]));
            lu < 1e-3
                || lv < 1e-3
                || (u[0] * v[0] + u[1] * v[1] < 0.0
                    && (u[0] * v[1] - u[1] * v[0]).abs() < 0.01 * lu * lv)
        });
        match spike {
            Some(k) => {
                ring.remove(k);
            }
            None => return ring,
        }
    }
}

/// A polyline with the vertices it runs straight through dropped: the ends, and
/// every vertex where the direction turns by more than about a twentieth of a
/// degree.
fn without_collinear(pts: &[[f32; 2]]) -> Vec<[f32; 2]> {
    let mut out: Vec<[f32; 2]> = Vec::with_capacity(pts.len());
    for &p in pts {
        if out
            .last()
            .is_some_and(|q| (q[0] - p[0]).hypot(q[1] - p[1]) < 1e-3)
        {
            continue;
        }
        if out.len() >= 2 {
            let (a, b) = (out[out.len() - 2], out[out.len() - 1]);
            let (u, v) = ([b[0] - a[0], b[1] - a[1]], [p[0] - b[0], p[1] - b[1]]);
            let turn = (u[0] * v[1] - u[1] * v[0]) / (u[0].hypot(u[1]) * v[0].hypot(v[1]));
            let ahead = u[0] * v[0] + u[1] * v[1] > 0.0;
            if ahead && turn.abs() < 1e-3 {
                out.pop();
            }
        }
        out.push(p);
    }
    out
}

/// How far outside a deck's outline a surveyed wall may run and still be that
/// deck's own parapet or screen, metres. Wide, because a deck built from the
/// ways ([`street_decks`]) is only the carriageways' drawn width: the real
/// Europaplein viaduct carries shoulders and a slip lane besides, and its
/// parapets stood 14 m from the carriageway's centre line, nine past the deck.
pub const DECK_WALL_REACH_M: f32 = 10.0;

/// How far from an approach's centre line a surveyed wall may run and still be
/// that ramp's own retaining wall or screen, metres: half a dual carriageway and
/// its verge.
pub const APPROACH_WALL_REACH_M: f32 = 14.0;

/// Tallest measured wall on a deck or beside a ramp that is taken to stand where
/// it is drawn rather than to be the structure's own, metres: a canal bridge's
/// parapet measures about one.
pub const BRIDGE_WALL_MAX_M: f32 = 1.5;

/// Whether a surveyed **wall** belongs to a bridge — a parapet or noise screen on
/// its deck, a retaining wall or screen along the ramp up to it — and so cannot
/// be drawn the way every other boundary is.
///
/// A boundary is a box standing *up from the ground* by its measured height, and
/// the 3D product measures a wall from its lowest foot to its top. For a
/// structure's own walls both halves of that are wrong: the viaduct's screens
/// over Groningen's Europaplein measured 7–9.5 m and were 170–270 m slabs
/// standing on the Europaweg under the deck, and its unmeasured parapets were
/// 48 m walls across the same road at ground level (2026-09-26). It is the
/// bridge's version of [`crate::tunnels::wall_of_a_sunken_road`], and it answers
/// the same way: the line is sampled, and it is the bridge's when most of it lies
/// on a deck (widened by [`DECK_WALL_REACH_M`]) or beside a ramp that climbs to
/// one. Only a wall with no measurement or one taller than [`BRIDGE_WALL_MAX_M`]
/// qualifies: a measured metre on a canal bridge is its parapet, standing where
/// it is drawn, and a low wall by a ramp is a garden wall. An unmeasured one
/// counts on the ramp too: OSM's bridge way routinely stops short of the
/// structure, and the Europaplein's parapets ran half off the deck and stood
/// as 2 m slabs on the Europaweg.
#[must_use]
pub fn wall_of_a_bridge(
    decks: &[BridgeDeck],
    approaches: &[Approach],
    line: &[[f32; 2]],
    height_m: Option<f32>,
) -> bool {
    if line.len() < 2 || decks.is_empty() {
        return false;
    }
    if height_m.is_some_and(|h| h <= BRIDGE_WALL_MAX_M) {
        return false;
    }
    let on_deck = |p: [f32; 2]| {
        decks.iter().any(|d| {
            p[0] >= d.min[0] - DECK_WALL_REACH_M
                && p[0] <= d.max[0] + DECK_WALL_REACH_M
                && p[1] >= d.min[1] - DECK_WALL_REACH_M
                && p[1] <= d.max[1] + DECK_WALL_REACH_M
                && (d.contains(p)
                    || d.rings.iter().any(|r| {
                        r.windows(2)
                            .any(|e| segment_distance(p, e[0], e[1]) <= DECK_WALL_REACH_M)
                    }))
        })
    };
    let by_ramp = |p: [f32; 2]| {
        approaches.iter().any(|a| {
            a.points
                .windows(2)
                .any(|e| segment_distance(p, e[0], e[1]) <= APPROACH_WALL_REACH_M)
        })
    };
    let (mut samples, mut near) = (0usize, 0usize);
    for w in line.windows(2) {
        let (a, b) = (w[0], w[1]);
        let steps = ((b[0] - a[0]).hypot(b[1] - a[1]) / 1.5).ceil().max(1.0) as usize;
        for k in 0..=steps {
            let t = k as f32 / steps as f32;
            let p = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
            samples += 1;
            if on_deck(p) || by_ramp(p) {
                near += 1;
            }
        }
    }
    near * 5 >= samples * 3
}

/// How close to a carriageway on the deck a point of the outline may lie and
/// still be the deck's side rather than the place the road runs off it, metres.
/// The split lands the road exactly on the outline where it leaves
/// (`split_by_deck`), and a deck built from the ways is the carriageway plus
/// [`STREET_DECK_MARGIN_M`] either side, so anything under that margin is a road
/// crossing the edge.
const RAILING_ROAD_CLEAR_M: f32 = 0.6;

/// How far along a deck's outline the railing's samples are taken, metres.
const RAILING_STEP_M: f32 = 0.5;

/// Shortest railing worth standing, metres: under this it is a corner of an
/// outline between two openings.
const RAILING_MIN_M: f32 = 2.0;

/// How far an edge may lie from a road end of the deck, metres, and how nearly
/// parallel to it (cosine), and still be part of that end: the pavement and the
/// verge either side of where the carriageway runs off, which a railing must not
/// close off.
const RAILING_END_REACH_M: f32 = 8.0;
const RAILING_END_PARALLEL: f32 = 0.9;

/// **Where a deck's railings run**: the stretches of its outline that the
/// carriageway does not cross, tile-local.
///
/// A deck was a slab with nothing along its edge, and from street level a slab
/// over a canal reads as a pontoon rather than a bridge (reported 2026-10-19). An
/// outline is the structure, parapets included (`area_decks`), so its edge is
/// where the parapet stands — except at the ends, where the road runs off it, and
/// along a tile border, where the deck goes on in the next cell. The ends are
/// found from the roads the deck carries (`carriageways`, the on-deck meshes the
/// road tessellation cut out of it) and the streets running off it
/// (`approaches`, abutment first): an outline edge a carriageway reaches or an
/// approach crosses is an end, and so is a nearly parallel edge close beside one — the pavement and the
/// verge at the same abutment. A road only grazing a corner does not count, or
/// a street along the quay would open the whole side it touches. A deck no road
/// reaches is railed down its long sides only ([`long_sides`]). Only the
/// exterior ring: a hole is a gap between carriageways, not an edge anyone walks
/// along.
#[must_use]
pub fn railing_runs(
    deck: &BridgeDeck,
    carriageways: &[&SurfaceMesh],
    approaches: &[&[[f32; 2]]],
    tile_m: f32,
) -> Vec<Vec<[f32; 2]>> {
    let Some((pts, kinds)) = edge_kinds(deck, carriageways, approaches, tile_m) else {
        return Vec::new();
    };
    let open: Vec<bool> = kinds.iter().map(|k| *k != Edge::Side).collect();
    join_runs(&pts, &open)
}

/// **Where a deck's abutments are**: the stretches of its outline where the road
/// runs off it onto the land — the ends [`railing_runs`] leaves open, less the
/// tile-border cuts. What a canal bridge stands on at each end is a wall from its
/// deck down into the water, and a deck drawn without one was open underneath to
/// whatever the field put at the bank (2026-10-19).
#[must_use]
pub fn abutment_runs(
    deck: &BridgeDeck,
    carriageways: &[&SurfaceMesh],
    approaches: &[&[[f32; 2]]],
    tile_m: f32,
) -> Vec<Vec<[f32; 2]>> {
    let Some((pts, kinds)) = edge_kinds(deck, carriageways, approaches, tile_m) else {
        return Vec::new();
    };
    let open: Vec<bool> = kinds.iter().map(|k| *k != Edge::End).collect();
    join_runs(&pts, &open)
}

/// How far an abutment wall reaches below its deck, metres: past a canal's bed
/// under any deck a city bridge stands at, and buried in the ground where the
/// ground is higher.
const ABUTMENT_WALL_DEPTH_M: f32 = 12.0;

/// The abutment wall along `run` ([`abutment_runs`]), at `y = 0` for the deck's
/// top and reaching [`ABUTMENT_WALL_DEPTH_M`] down: one vertical face per
/// segment, facing in under `deck`, where the canal sees it. Lifted onto the
/// deck with its carriageways, and bent along its profile.
///
/// **Not a face toward the street**: the street meets the deck's top there, and
/// wherever it dipped a few centimetres between its vertices that face stood out
/// of it as a brown band along the Oosterbrug's south end (2026-10-20).
#[must_use]
pub fn abutment_wall_mesh(deck: &BridgeDeck, run: &[[f32; 2]]) -> SurfaceMesh {
    let centre = [
        (deck.min[0] + deck.max[0]) * 0.5,
        (deck.min[1] + deck.max[1]) * 0.5,
    ];
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    let d = ABUTMENT_WALL_DEPTH_M;
    for w in run.windows(2) {
        let (a, b) = (w[0], w[1]);
        let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
        let len = dx.hypot(dz);
        if len < 1e-3 {
            continue;
        }
        let n = [-dz / len, dx / len];
        // In: toward the middle of the deck. The run may stand a few metres out
        // over the lip (`reach_the_quay`), so not a probe off the outline.
        let mid = [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
        let inward = (centre[0] - mid[0]) * n[0] + (centre[1] - mid[1]) * n[1] >= 0.0;
        for face in [if inward { n } else { [-n[0], -n[1]] }] {
            let base = positions.len() as u32;
            positions.extend_from_slice(&[
                [a[0], 0.0, a[1]],
                [b[0], 0.0, b[1]],
                [b[0], -d, b[1]],
                [a[0], -d, a[1]],
            ]);
            normals.extend_from_slice(&[[face[0], 0.0, face[1]]; 4]);
            // Wound to face `face`, whichever way round the run goes.
            let (v0, v1, v2) = (
                positions[base as usize],
                positions[base as usize + 1],
                positions[base as usize + 2],
            );
            let (e1, e2) = (
                [v1[0] - v0[0], v1[1] - v0[1], v1[2] - v0[2]],
                [v2[0] - v0[0], v2[1] - v0[1], v2[2] - v0[2]],
            );
            let cross = [
                e1[1] * e2[2] - e1[2] * e2[1],
                e1[2] * e2[0] - e1[0] * e2[2],
                e1[0] * e2[1] - e1[1] * e2[0],
            ];
            if cross[0] * face[0] + cross[2] * face[1] >= 0.0 {
                indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
            } else {
                indices.extend_from_slice(&[base, base + 2, base + 1, base, base + 3, base + 2]);
            }
        }
    }
    SurfaceMesh {
        group: SurfaceGroup::QuayWall,
        positions,
        indices,
        uvs: Vec::new(),
        normals,
        colors: Vec::new(),
        rungs: Vec::new(),
    }
}

/// Furthest a deck's end is carried out over water to meet its quay, metres.
const ABUTMENT_REACH_M: f32 = 3.0;

/// Step the water is probed at along an end's outward normal, metres.
const ABUTMENT_PROBE_M: f32 = 0.25;

/// **An end that stops short of its quay is carried out to it.** OSM's outline
/// and the register's water seldom meet: the Oosterbrug's ends stop a metre or
/// so over the canal, and with the slab ending there the gap was open water
/// between the deck and the street (2026-10-19). Each vertex of an abutment run
/// is moved outward — away from the deck — for as long as `water` says the
/// ground beyond is water, up to [`ABUTMENT_REACH_M`], and one probe further so
/// the deck lands on the quay rather than at its edge. Where the end already sits
/// on land the run comes back as it was.
#[must_use]
pub fn reach_the_quay(
    deck: &BridgeDeck,
    run: &[[f32; 2]],
    water: &dyn Fn([f32; 2]) -> bool,
) -> Vec<[f32; 2]> {
    let n = run.len();
    if n < 2 {
        return run.to_vec();
    }
    let seg_normal = |a: [f32; 2], b: [f32; 2]| -> Option<[f32; 2]> {
        let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
        let len = dx.hypot(dz);
        (len > 1e-3).then(|| {
            let nrm = [-dz / len, dx / len];
            // Outward: the side the deck is not on.
            let mid = [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
            if deck.contains([mid[0] + nrm[0] * 0.5, mid[1] + nrm[1] * 0.5]) {
                [-nrm[0], -nrm[1]]
            } else {
                nrm
            }
        })
    };
    (0..n)
        .map(|i| {
            let p = run[i];
            let before = (i > 0).then(|| seg_normal(run[i - 1], p)).flatten();
            let after = (i + 1 < n).then(|| seg_normal(p, run[i + 1])).flatten();
            let nrm = match (before, after) {
                (Some(a), Some(b)) => {
                    let s = [a[0] + b[0], a[1] + b[1]];
                    let l = s[0].hypot(s[1]);
                    if l > 1e-3 {
                        [s[0] / l, s[1] / l]
                    } else {
                        a
                    }
                }
                (Some(a), None) | (None, Some(a)) => a,
                (None, None) => return p,
            };
            let mut d = 0.0;
            while d < ABUTMENT_REACH_M
                && water([
                    p[0] + nrm[0] * (d + ABUTMENT_PROBE_M),
                    p[1] + nrm[1] * (d + ABUTMENT_PROBE_M),
                ])
            {
                d += ABUTMENT_PROBE_M;
            }
            if d > 0.0 {
                d += ABUTMENT_PROBE_M;
            }
            [p[0] + nrm[0] * d, p[1] + nrm[1] * d]
        })
        .collect()
}

/// The strip of deck between an end as outlined and where it meets its quay
/// ([`reach_the_quay`]), at `y = 0` for the deck's top: the slab's own surface,
/// carried on. `None` where the two coincide.
#[must_use]
pub fn quay_lip_mesh(inner: &[[f32; 2]], outer: &[[f32; 2]]) -> Option<SurfaceMesh> {
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    for k in 0..inner.len().min(outer.len()).saturating_sub(1) {
        let (a, b, c, d) = (inner[k], inner[k + 1], outer[k + 1], outer[k]);
        let reach = (d[0] - a[0])
            .hypot(d[1] - a[1])
            .max((c[0] - b[0]).hypot(c[1] - b[1]));
        if reach < 1e-3 {
            continue;
        }
        let base = positions.len() as u32;
        positions.extend_from_slice(&[
            [a[0], 0.0, a[1]],
            [b[0], 0.0, b[1]],
            [c[0], 0.0, c[1]],
            [d[0], 0.0, d[1]],
        ]);
        // Wound to face up whichever way round the end runs.
        let up = (b[0] - a[0]) * (d[1] - a[1]) - (b[1] - a[1]) * (d[0] - a[0]);
        if up <= 0.0 {
            indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        } else {
            indices.extend_from_slice(&[base, base + 2, base + 1, base, base + 3, base + 2]);
        }
    }
    (!indices.is_empty()).then(|| SurfaceMesh {
        group: SurfaceGroup::BridgeDeck,
        normals: vec![[0.0, 1.0, 0.0]; positions.len()],
        positions,
        indices,
        uvs: Vec::new(),
        colors: Vec::new(),
        rungs: Vec::new(),
    })
}

/// How far outside a deck's edge the water it spans is looked for, metres: past
/// the metre by which the register and OSM disagree about a bank, short of the
/// far bank of the narrowest canal.
const SPANNED_WATER_PROBE_M: f32 = 1.5;

/// Shortest outline edge asked whether water lies beyond it, metres: a notch in
/// a parapet is not a side.
const SPANNED_WATER_EDGE_M: f32 = 2.0;

/// How closely the water on a deck's two sides must agree to be one canal,
/// metres. Two levels further apart are a lock or a weir, and the ground under
/// the structure between them is not either's to claim.
const SPANNED_WATER_LEVEL_M: f32 = 0.25;

/// **The water a bridge stands over that the register leaves out**: the level of
/// the water just outside two opposite edges of `deck`, where it spans one body
/// of water, or `None`.
///
/// The Kadaster's water stops at a bridge's outline, the BGT's water and its
/// bridge decks tiling the ground between them, while OSM's canal runs on under
/// the bridge. So under the Oosterbrug the client drew OSM's canal with no level,
/// at the height of the ground, and atlas graded that ground as land up to its
/// quays: mounds and slanted walls under the span, and water climbing to meet
/// them (2026-10-20). The deck's outline is that gap. Each exterior edge is
/// probed [`SPANNED_WATER_PROBE_M`] outward at a quarter, a half and three
/// quarters of its length; two edges facing apart with water beyond both, at
/// levels within [`SPANNED_WATER_LEVEL_M`], make the deck span it. `level_at`
/// answers the registered water's level at a tile-local point.
#[must_use]
pub fn spanned_water(deck: &BridgeDeck, level_at: impl Fn([f32; 2]) -> Option<f32>) -> Option<f32> {
    let ring = deck.rings.first()?;
    let mut wet: Vec<([f32; 2], f32)> = Vec::new();
    for e in ring.windows(2) {
        let (a, b) = (e[0], e[1]);
        let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
        let len = dx.hypot(dz);
        if len < SPANNED_WATER_EDGE_M {
            continue;
        }
        let mid = [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
        let nrm = [dz / len, -dx / len];
        // Outward: probed 5 cm off the edge, never read from the winding.
        let out = if deck.contains([mid[0] + nrm[0] * 0.05, mid[1] + nrm[1] * 0.05]) {
            [-nrm[0], -nrm[1]]
        } else {
            nrm
        };
        let mut levels: Vec<f32> = [0.25f32, 0.5, 0.75]
            .iter()
            .filter_map(|t| {
                let p = [
                    a[0] + dx * t + out[0] * SPANNED_WATER_PROBE_M,
                    a[1] + dz * t + out[1] * SPANNED_WATER_PROBE_M,
                ];
                if deck.contains(p) {
                    return None;
                }
                level_at(p)
            })
            .collect();
        if levels.len() >= 2 {
            levels.sort_by(f32::total_cmp);
            wet.push((out, levels[levels.len() / 2]));
        }
    }
    let mut spanned: Option<f32> = None;
    for (i, (n1, l1)) in wet.iter().enumerate() {
        for (n2, l2) in &wet[i + 1..] {
            if n1[0] * n2[0] + n1[1] * n2[1] < -0.7 && (l1 - l2).abs() <= SPANNED_WATER_LEVEL_M {
                let l = l1.min(*l2);
                spanned = Some(spanned.map_or(l, |s: f32| s.min(l)));
            }
        }
    }
    spanned
}

/// What one outline edge of a deck is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edge {
    /// Along the structure: where its parapet runs.
    Side,
    /// Where the road runs off it onto the land.
    End,
    /// A tile-border cut, or nothing that can be told: neither.
    Neither,
}

/// Each edge of a deck's exterior ring, without its closing repeat — see
/// [`railing_runs`] for the rule.
fn edge_kinds(
    deck: &BridgeDeck,
    carriageways: &[&SurfaceMesh],
    approaches: &[&[[f32; 2]]],
    tile_m: f32,
) -> Option<(Vec<[f32; 2]>, Vec<Edge>)> {
    let Some(ring) = deck.rings.first() else {
        return None;
    };
    // The ring without its closing repeat, so every edge is `ring[k]→ring[k+1]`
    // with the last wrapping round to the first.
    let mut pts: Vec<[f32; 2]> = ring.clone();
    if pts.len() > 1 && pts[0] == pts[pts.len() - 1] {
        pts.pop();
    }
    let n = pts.len();
    if n < 3 {
        return None;
    }
    let tris: Vec<[[f32; 2]; 3]> = carriageways
        .iter()
        .flat_map(|m| {
            m.indices.chunks_exact(3).filter_map(move |t| {
                let q = |i: u32| m.positions.get(i as usize).map(|v| [v[0], v[2]]);
                Some([q(t[0])?, q(t[1])?, q(t[2])?])
            })
        })
        .collect();
    let near_road = |p: [f32; 2]| {
        tris.iter().any(|t| {
            let r = RAILING_ROAD_CLEAR_M;
            let (lo_x, hi_x) = (
                t[0][0].min(t[1][0]).min(t[2][0]),
                t[0][0].max(t[1][0]).max(t[2][0]),
            );
            let (lo_z, hi_z) = (
                t[0][1].min(t[1][1]).min(t[2][1]),
                t[0][1].max(t[1][1]).max(t[2][1]),
            );
            if p[0] < lo_x - r || p[0] > hi_x + r || p[1] < lo_z - r || p[1] > hi_z + r {
                return false;
            }
            point_in_triangle(p, t)
                || segment_distance(p, t[0], t[1]) <= r
                || segment_distance(p, t[1], t[2]) <= r
                || segment_distance(p, t[2], t[0]) <= r
        })
    };
    let on_border = |a: [f32; 2], b: [f32; 2]| {
        let e = 0.05;
        [0.0, tile_m].iter().any(|&v| {
            ((a[0] - v).abs() < e && (b[0] - v).abs() < e)
                || ((a[1] - v).abs() < e && (b[1] - v).abs() < e)
        })
    };
    let edge = |k: usize| (pts[k], pts[(k + 1) % n]);

    // An edge the road reaches is an end; so is a border cut.
    let mut open = vec![false; n];
    let mut border = vec![false; n];
    let mut ends: Vec<usize> = Vec::new();
    for k in 0..n {
        let (a, b) = edge(k);
        if on_border(a, b) {
            open[k] = true;
            border[k] = true;
            continue;
        }
        let len = (b[0] - a[0]).hypot(b[1] - a[1]);
        let steps = (len / RAILING_STEP_M).ceil().max(1.0) as usize;
        let reached = (0..=steps)
            .filter(|&s| {
                let t = s as f32 / steps as f32;
                near_road([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t])
            })
            .count();
        // A road *crossing* the edge, not grazing a corner of it: a street along
        // the quay touches the corner where a side meets an end, and that must
        // not open the whole side.
        let reached_m = reached as f32 * len / steps as f32;
        // …or the street running off it: OSM's bridge way often stops short of
        // the outline (the Oosterbrug's, 2026-10-19), so the carriageway on the
        // deck reaches no edge, and the street carries on as the approach.
        let crossed = approaches.iter().any(|run| {
            run.first()
                .is_some_and(|&p| segment_distance(p, a, b) <= RAILING_ROAD_CLEAR_M)
                || run.windows(2).any(|w| segments_cross(w[0], w[1], a, b))
        });
        if crossed || (reached > 0 && reached_m >= RAILING_MIN_M.min(len * 0.5)) {
            open[k] = true;
            ends.push(k);
        }
    }
    if ends.is_empty() {
        let kinds = long_sides(&pts, &border);
        return Some((pts, kinds));
    }
    // …and an edge lying beside an end and along it.
    for k in 0..n {
        if open[k] {
            continue;
        }
        let (a, b) = edge(k);
        let len = (b[0] - a[0]).hypot(b[1] - a[1]);
        if len < 1e-3 {
            open[k] = true;
            continue;
        }
        let mid = [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
        open[k] = ends.iter().any(|&j| {
            let (c, d) = edge(j);
            let lj = (d[0] - c[0]).hypot(d[1] - c[1]);
            if lj < 1e-3 {
                return false;
            }
            let cos =
                ((b[0] - a[0]) * (d[0] - c[0]) + (b[1] - a[1]) * (d[1] - c[1])).abs() / (len * lj);
            cos >= RAILING_END_PARALLEL
                && segment_distance(mid, c, d)
                    .min(segment_distance(a, c, d))
                    .min(segment_distance(b, c, d))
                    <= RAILING_END_REACH_M
        });
    }
    let kinds = (0..n)
        .map(|k| {
            if border[k] {
                Edge::Neither
            } else if open[k] {
                Edge::End
            } else {
                Edge::Side
            }
        })
        .collect();
    Some((pts, kinds))
}

/// How much longer than wide a deck no road reaches must be for its long sides
/// to be told from its ends.
const RAILING_LONG_ASPECT: f32 = 1.5;

/// The edges of a deck no road reaches: its two long sides are the edges within
/// 45° of its long axis and the rest are its ends, and a deck about as wide as it
/// is long has neither — railing it all round would wall off whatever the tiles
/// failed to draw crossing it, and there is no telling which way that runs.
fn long_sides(pts: &[[f32; 2]], border: &[bool]) -> Vec<Edge> {
    let n = pts.len() as f32;
    let c = [
        pts.iter().map(|p| p[0]).sum::<f32>() / n,
        pts.iter().map(|p| p[1]).sum::<f32>() / n,
    ];
    let (mut sxx, mut szz, mut sxz) = (0.0f32, 0.0f32, 0.0f32);
    for p in pts {
        let (x, z) = (p[0] - c[0], p[1] - c[1]);
        sxx += x * x;
        szz += z * z;
        sxz += x * z;
    }
    let th = 0.5 * (2.0 * sxz).atan2(sxx - szz);
    let axis = [th.cos(), th.sin()];
    let (mut lo, mut hi, mut wlo, mut whi) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
    for p in pts {
        let (x, z) = (p[0] - c[0], p[1] - c[1]);
        let along = x * axis[0] + z * axis[1];
        let across = -x * axis[1] + z * axis[0];
        (lo, hi) = (lo.min(along), hi.max(along));
        (wlo, whi) = (wlo.min(across), whi.max(across));
    }
    let k = pts.len();
    if hi - lo < RAILING_LONG_ASPECT * (whi - wlo) {
        return vec![Edge::Neither; k];
    }
    (0..k)
        .map(|i| {
            let (a, b) = (pts[i], pts[(i + 1) % k]);
            let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
            let len = dx.hypot(dz);
            if border[i] || len < 1e-3 {
                Edge::Neither
            } else if (dx * axis[0] + dz * axis[1]).abs() / len < std::f32::consts::FRAC_1_SQRT_2 {
                Edge::End
            } else {
                Edge::Side
            }
        })
        .collect()
}

/// The edges of a ring `open` does not mark, joined into runs.
fn join_runs(pts: &[[f32; 2]], open: &[bool]) -> Vec<Vec<[f32; 2]>> {
    let n = pts.len();
    let edge = |k: usize| (pts[k], pts[(k + 1) % n]);
    // Start just after an open edge so no run is split by the ring's own seam.
    let Some(start) = (0..n).find(|&k| open[k]) else {
        let mut all = pts.to_vec();
        all.push(pts[0]);
        return vec![all];
    };
    let mut runs: Vec<Vec<[f32; 2]>> = Vec::new();
    let mut run: Vec<[f32; 2]> = Vec::new();
    for s in 1..=n {
        let k = (start + s) % n;
        if open[k] {
            if run.len() >= 2 {
                runs.push(std::mem::take(&mut run));
            }
            run.clear();
            continue;
        }
        let (a, b) = edge(k);
        if run.is_empty() {
            run.push(a);
        }
        run.push(b);
    }
    if run.len() >= 2 {
        runs.push(run);
    }
    runs.retain(|r| crate::tunnels::polyline_len(r) >= RAILING_MIN_M);
    runs.into_iter().map(|r| skip_notches(&r)).collect()
}

/// Deepest and longest detour in an outline a railing runs straight across,
/// metres. The Oosterbrug's outline steps in ~2.6 m on each side where its
/// bascule's mechanism stands, ~6 m of outline in all, and a railing following
/// it in and out read from the quay as a break in it (2026-10-19). Bounded by
/// length as well as depth so a curved deck keeps its curve: over 8 m of a
/// 20 m radius the chord is 0.4 m off the arc.
const RAILING_NOTCH_DEEP_M: f32 = 3.0;
const RAILING_NOTCH_LONG_M: f32 = 8.0;

/// Up to this much outline is crossed straight too, metres, where it is a
/// *detour* — its path [`RAILING_DETOUR_RATIO`] times the straight line or more,
/// which a smooth curve never is (16 m round a 20 m radius: 1.03). The
/// Oosterbrug's north notch starts 8.6 m along its side, past the short reach,
/// and left a metre's step in the railing.
const RAILING_DETOUR_LONG_M: f32 = 16.0;
const RAILING_DETOUR_RATIO: f32 = 1.05;

/// `run` with its short detours cut straight: from each kept vertex, the
/// farthest vertex within [`RAILING_NOTCH_LONG_M`] of path — or
/// [`RAILING_DETOUR_LONG_M`] where the path is a detour — whose in-between
/// vertices all lie within [`RAILING_NOTCH_DEEP_M`] of the straight line to it.
fn skip_notches(run: &[[f32; 2]]) -> Vec<[f32; 2]> {
    if run.len() < 3 {
        return run.to_vec();
    }
    let dist = |a: [f32; 2], b: [f32; 2]| (b[0] - a[0]).hypot(b[1] - a[1]);
    let mut out = vec![run[0]];
    let mut i = 0;
    while i < run.len() - 1 {
        let mut next = i + 1;
        let mut path = 0.0f32;
        for j in i + 1..run.len() {
            path += dist(run[j - 1], run[j]);
            if path > RAILING_DETOUR_LONG_M {
                break;
            }
            let short = path <= RAILING_NOTCH_LONG_M;
            let detour = path >= RAILING_DETOUR_RATIO * dist(run[i], run[j]);
            if (short || detour)
                && (i + 1..j)
                    .all(|k| segment_distance(run[k], run[i], run[j]) <= RAILING_NOTCH_DEEP_M)
            {
                next = j;
            }
        }
        out.push(run[next]);
        i = next;
    }
    out
}

/// Cut out of `runs` every stretch for which `keep(point, direction)` says no,
/// the direction being the unit vector of the run there, sampled every
/// [`RAILING_STEP_M`]; what is left shorter than [`RAILING_MIN_M`] is dropped.
/// How a railing gives way to a parapet the register already surveys.
#[must_use]
pub fn cut_runs(
    runs: Vec<Vec<[f32; 2]>>,
    keep: impl Fn([f32; 2], [f32; 2]) -> bool,
) -> Vec<Vec<[f32; 2]>> {
    let mut out = Vec::new();
    for r in runs {
        let mut cur: Vec<[f32; 2]> = Vec::new();
        // Whether `cur`'s last point is a sample partway along the current
        // segment, which the next kept sample of that segment replaces: a run
        // keeps its corners and its ends, not every sample.
        let mut mid_segment;
        for w in r.windows(2) {
            let (a, b) = (w[0], w[1]);
            let len = (b[0] - a[0]).hypot(b[1] - a[1]);
            let steps = (len / RAILING_STEP_M).ceil().max(1.0) as usize;
            let dir = if len > 0.0 {
                [(b[0] - a[0]) / len, (b[1] - a[1]) / len]
            } else {
                [1.0, 0.0]
            };
            mid_segment = false;
            // Sample 0 is the corner the previous segment already ended on.
            for s in usize::from(!cur.is_empty())..=steps {
                let t = s as f32 / steps as f32;
                let p = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
                if !keep(p, dir) {
                    if cur.len() >= 2 {
                        out.push(std::mem::take(&mut cur));
                    }
                    cur.clear();
                    mid_segment = false;
                    continue;
                }
                if mid_segment {
                    if let Some(last) = cur.last_mut() {
                        *last = p;
                    }
                } else {
                    cur.push(p);
                }
                mid_segment = cur.len() >= 2 && s < steps;
            }
        }
        if cur.len() >= 2 {
            out.push(cur);
        }
    }
    out.retain(|r: &Vec<[f32; 2]>| {
        r.len() >= 2 && crate::tunnels::polyline_len(r) >= RAILING_MIN_M
    });
    out
}

/// Whether segments `p`–`q` and `a`–`b` cross.
fn segments_cross(p: [f32; 2], q: [f32; 2], a: [f32; 2], b: [f32; 2]) -> bool {
    let side = |o: [f32; 2], d: [f32; 2], x: [f32; 2]| {
        (d[0] - o[0]) * (x[1] - o[1]) - (d[1] - o[1]) * (x[0] - o[0])
    };
    let (d1, d2) = (side(a, b, p), side(a, b, q));
    let (d3, d4) = (side(p, q, a), side(p, q, b));
    (d1 > 0.0) != (d2 > 0.0) && (d3 > 0.0) != (d4 > 0.0)
}

fn point_in_triangle(p: [f32; 2], t: &[[f32; 2]; 3]) -> bool {
    let s =
        |a: [f32; 2], b: [f32; 2]| (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]);
    let (d0, d1, d2) = (s(t[0], t[1]), s(t[1], t[2]), s(t[2], t[0]));
    let neg = d0 < 0.0 || d1 < 0.0 || d2 < 0.0;
    let pos = d0 > 0.0 || d1 > 0.0 || d2 > 0.0;
    !(neg && pos)
}

/// Distance from `p` to the segment `a`–`b`.
fn segment_distance(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
    let len2 = dx * dx + dz * dz;
    let t = if len2 > 0.0 {
        (((p[0] - a[0]) * dx + (p[1] - a[1]) * dz) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (p[0] - a[0] - dx * t).hypot(p[1] - a[1] - dz * t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector_tiles::bytes::{roads_and_decks, tessellate_bridges};

    // A real z14 tile over Groningen, from the VersaTiles Shortbread source
    // (now self-hosted at tiles.cartopolis.org; the archive is identical).
    use crate::testdata::GRONINGEN_Z14;

    fn rect_deck(ring: &[[f32; 2]]) -> BridgeDeck {
        let mut min = [f32::MAX, f32::MAX];
        let mut max = [f32::MIN, f32::MIN];
        for p in ring {
            min = [min[0].min(p[0]), min[1].min(p[1])];
            max = [max[0].max(p[0]), max[1].max(p[1])];
        }
        let mut closed = ring.to_vec();
        closed.push(ring[0]);
        BridgeDeck {
            positions: Vec::new(),
            indices: Vec::new(),
            rings: vec![closed],
            min,
            max,
        }
    }

    fn rect_road(x0: f32, z0: f32, x1: f32, z1: f32) -> SurfaceMesh {
        SurfaceMesh {
            group: SurfaceGroup::Road(RoadClass::Minor),
            positions: vec![[x0, 0.0, z0], [x1, 0.0, z0], [x1, 0.0, z1], [x0, 0.0, z1]],
            indices: vec![0, 1, 2, 0, 2, 3],
            uvs: Vec::new(),
            normals: Vec::new(),
            colors: Vec::new(),
            rungs: Vec::new(),
        }
    }

    /// **A canal bridge's railings run down its sides and stop at its ends.** A
    /// 30 × 12 m deck carrying a 6 m road end to end: two runs, each the whole
    /// length of a long side, and nothing across the road at either end.
    #[test]
    fn railings_run_down_the_sides_not_across_the_road() {
        let deck = rect_deck(&[
            [100.0, 100.0],
            [130.0, 100.0],
            [130.0, 112.0],
            [100.0, 112.0],
        ]);
        let road = rect_road(100.0, 103.0, 130.0, 109.0);
        let runs = railing_runs(&deck, &[&road], &[], 1465.0);
        assert_eq!(runs.len(), 2, "{runs:?}");
        for r in &runs {
            let len = crate::tunnels::polyline_len(r);
            assert!((len - 30.0).abs() < 1e-3, "a side is {len} m: {r:?}");
            assert!(
                r.iter()
                    .all(|p| (p[1] - 100.0).abs() < 1e-3 || (p[1] - 112.0).abs() < 1e-3),
                "a run crosses an end: {r:?}"
            );
        }
    }

    /// **A deck spanning a canal whose register stops at its outline takes the
    /// canal's level under it** — the Oosterbrug's shape: the canal runs along x,
    /// the deck crosses it, water beyond both its sides and land beyond its ends.
    /// Not between two levels (a lock), and not where water lies beside one side.
    #[test]
    fn a_deck_across_a_canal_spans_its_water() {
        let deck = rect_deck(&[
            [100.0, 100.0],
            [112.0, 100.0],
            [112.0, 130.0],
            [100.0, 130.0],
        ]);
        let canal = |p: [f32; 2]| (p[1] > 100.0 && p[1] < 130.0).then_some(0.8);
        assert_eq!(spanned_water(&deck, canal), Some(0.8));
        let lock = |p: [f32; 2]| {
            (p[1] > 100.0 && p[1] < 130.0).then_some(if p[0] < 106.0 { 0.8 } else { 2.0 })
        };
        assert_eq!(spanned_water(&deck, lock), None);
        let beside = |p: [f32; 2]| (p[0] > 112.0).then_some(0.8);
        assert_eq!(spanned_water(&deck, beside), None);
        assert_eq!(spanned_water(&deck, |_| None), None);
    }

    /// **The abutments are the ends the railings leave open**, and each stands as
    /// a wall facing both ways from the deck down.
    #[test]
    fn a_deck_stands_on_an_abutment_at_each_end() {
        let deck = rect_deck(&[
            [100.0, 100.0],
            [130.0, 100.0],
            [130.0, 112.0],
            [100.0, 112.0],
        ]);
        let road = rect_road(100.0, 103.0, 130.0, 109.0);
        let ends = abutment_runs(&deck, &[&road], &[], 1465.0);
        assert_eq!(ends.len(), 2, "{ends:?}");
        for e in &ends {
            assert!(
                e.iter()
                    .all(|p| (p[0] - 100.0).abs() < 1e-3 || (p[0] - 130.0).abs() < 1e-3),
                "{e:?}"
            );
            assert!(
                (crate::tunnels::polyline_len(e) - 12.0).abs() < 1e-3,
                "{e:?}"
            );
            let wall = abutment_wall_mesh(&deck, e);
            assert_eq!(wall.group, SurfaceGroup::QuayWall);
            // One segment, one face of two triangles, facing in under the deck.
            assert_eq!(wall.indices.len(), 6);
            assert!(wall
                .positions
                .iter()
                .all(|p| p[1] <= 0.0 && p[1] >= -ABUTMENT_WALL_DEPTH_M));
            let inward = if (e[0][0] - 100.0).abs() < 1e-3 {
                1.0
            } else {
                -1.0
            };
            assert!(
                wall.normals.iter().all(|v| v[0] * inward > 0.9),
                "{:?}",
                wall.normals
            );
        }
    }

    /// **An end over water is carried out to its quay**, a probe onto the land,
    /// and no further than the reach; an end on land stays where it is.
    #[test]
    fn an_end_short_of_its_quay_reaches_it() {
        let deck = rect_deck(&[
            [100.0, 100.0],
            [130.0, 100.0],
            [130.0, 112.0],
            [100.0, 112.0],
        ]);
        let east = [[130.0, 100.0], [130.0, 112.0]];
        // Water for a metre and a half past the east end, then the quay.
        let water = |p: [f32; 2]| p[0] > 130.0 && p[0] < 131.5;
        let out = reach_the_quay(&deck, &east, &water);
        for p in &out {
            assert!((p[0] - 131.5).abs() <= 0.26, "{out:?}");
        }
        let lip = quay_lip_mesh(&east, &out).expect("a lip");
        assert_eq!(lip.group, SurfaceGroup::BridgeDeck);
        assert_eq!(lip.indices.len(), 6);
        // An open canal past it is crossed no further than the reach.
        let sea = |p: [f32; 2]| p[0] > 130.0;
        let far = reach_the_quay(&deck, &east, &sea);
        assert!(
            far.iter()
                .all(|p| p[0] <= 130.0 + ABUTMENT_REACH_M + ABUTMENT_PROBE_M + 1e-3),
            "{far:?}"
        );
        // An end already on its quay stays put, and makes no lip.
        let dry = |_: [f32; 2]| false;
        let same = reach_the_quay(&deck, &east, &dry);
        assert_eq!(same, east.to_vec());
        assert!(quay_lip_mesh(&east, &same).is_none());
    }

    /// The pavement either side of the road at an end is part of that end: an
    /// outline whose end is three edges (verge, carriageway, verge) gets no
    /// railing across any of them.
    #[test]
    fn an_end_split_into_several_edges_stays_open() {
        let deck = rect_deck(&[
            [100.0, 100.0],
            [130.0, 100.0],
            [130.0, 103.0],
            [130.0, 109.0],
            [130.0, 112.0],
            [100.0, 112.0],
            [100.0, 109.0],
            [100.0, 103.0],
        ]);
        let road = rect_road(100.0, 103.0, 130.0, 109.0);
        let runs = railing_runs(&deck, &[&road], &[], 1465.0);
        let total: f32 = runs.iter().map(|r| crate::tunnels::polyline_len(r)).sum();
        assert!((total - 60.0).abs() < 1e-3, "railing {total} m: {runs:?}");
    }

    /// **A bridge way that stops short of its outline still opens the ends.** The
    /// Oosterbrug's road ends a few metres inside the deck on both sides, and the
    /// deck was railed shut all round (2026-10-19); the street running on off
    /// it — the approach — is what crosses the end.
    #[test]
    fn an_approach_crossing_an_end_opens_it() {
        let deck = rect_deck(&[
            [100.0, 100.0],
            [124.0, 100.0],
            [124.0, 122.0],
            [100.0, 122.0],
        ]);
        // The carriageway stops 3 m short of both ends.
        let road = rect_road(103.0, 106.0, 121.0, 116.0);
        let west: &[[f32; 2]] = &[[103.0, 111.0], [80.0, 111.0]];
        let east: &[[f32; 2]] = &[[121.0, 111.0], [150.0, 111.0]];
        let runs = railing_runs(&deck, &[&road], &[west, east], 1465.0);
        assert_eq!(runs.len(), 2, "{runs:?}");
        for r in &runs {
            assert!(
                r.iter()
                    .all(|p| (p[1] - 100.0).abs() < 1e-3 || (p[1] - 122.0).abs() < 1e-3),
                "a railing closes an end: {r:?}"
            );
        }
    }

    /// **A notch in the outline is not a break in the railing.** The Oosterbrug's
    /// sides step in about two and a half metres where the bascule's mechanism
    /// stands; the railing runs straight past, and keeps its real corners.
    #[test]
    fn a_railing_runs_straight_across_a_shallow_notch() {
        // The Oosterbrug's south side as logged, tile-local metres.
        let side = [
            [253.0, 1345.0],
            [247.0, 1338.0],
            [247.0, 1339.0],
            [245.0, 1339.0],
            [244.0, 1338.0],
            [245.0, 1337.0],
            [240.0, 1329.0],
        ];
        let s = skip_notches(&side);
        for notch in [[247.0, 1339.0], [245.0, 1339.0], [244.0, 1338.0]] {
            assert!(
                !s.contains(&notch),
                "the railing goes into the notch: {s:?}"
            );
        }
        assert_eq!((s[0], s[s.len() - 1]), (side[0], side[side.len() - 1]));
        // The north side, whose notch starts 8.6 m along it: no step left.
        let north = [
            [256.0, 1315.0],
            [261.0, 1322.0],
            [262.0, 1322.0],
            [263.0, 1324.0],
            [263.0, 1325.0],
            [262.0, 1326.0],
            [267.0, 1334.0],
        ];
        let s = skip_notches(&north);
        // Every turn the railing makes along it is a few degrees at most: the
        // kink into the notch's first vertex was 16°, and read as a step.
        for w in s.windows(3) {
            let (u, v) = (
                [w[1][0] - w[0][0], w[1][1] - w[0][1]],
                [w[2][0] - w[1][0], w[2][1] - w[1][1]],
            );
            let cos = (u[0] * v[0] + u[1] * v[1]) / (u[0].hypot(u[1]) * v[0].hypot(v[1]));
            assert!(cos > 0.99, "a {}° kink: {s:?}", cos.acos().to_degrees());
        }
        let corner = [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]];
        assert_eq!(skip_notches(&corner), corner.to_vec());
        // A gentle curve, 2 m vertices round a 20 m radius, stays within
        // half a metre of itself.
        let arc: Vec<[f32; 2]> = (0..16)
            .map(|k| {
                let t = k as f32 * 0.1;
                [20.0 * t.cos(), 20.0 * t.sin()]
            })
            .collect();
        for p in &arc {
            let s = skip_notches(&arc);
            let off = s
                .windows(2)
                .map(|w| segment_distance(*p, w[0], w[1]))
                .fold(f32::MAX, f32::min);
            assert!(off < 0.5, "{p:?} is {off} m off the railing");
        }
    }

    /// A street along the quay that only touches a corner of the deck does not
    /// open the side it touches.
    #[test]
    fn a_road_grazing_a_corner_leaves_the_side_railed() {
        let deck = rect_deck(&[
            [100.0, 100.0],
            [130.0, 100.0],
            [130.0, 112.0],
            [100.0, 112.0],
        ]);
        let road = rect_road(100.0, 103.0, 130.0, 109.0);
        // A quay street running across the deck's west end, reaching 0.5 m onto
        // both long sides at their corners.
        let quay = rect_road(94.0, 90.0, 100.5, 122.0);
        let runs = railing_runs(&deck, &[&road, &quay], &[], 1465.0);
        let total: f32 = runs.iter().map(|r| crate::tunnels::polyline_len(r)).sum();
        assert!(total > 55.0, "railing {total} m: {runs:?}");
    }

    /// A deck no road reaches is railed down its long sides, and not at all if
    /// it is about square: enclosing it would wall off a road the tiles did not
    /// draw onto it.
    #[test]
    fn a_deck_no_road_reaches_is_railed_down_its_long_sides_only() {
        let long = rect_deck(&[
            [100.0, 100.0],
            [130.0, 100.0],
            [130.0, 106.0],
            [100.0, 106.0],
        ]);
        let runs = railing_runs(&long, &[], &[], 1465.0);
        assert_eq!(runs.len(), 2, "{runs:?}");
        assert!(
            runs.iter()
                .all(|r| (crate::tunnels::polyline_len(r) - 30.0).abs() < 1e-3),
            "{runs:?}"
        );
        let square = rect_deck(&[
            [100.0, 100.0],
            [120.0, 100.0],
            [120.0, 118.0],
            [100.0, 118.0],
        ]);
        assert!(railing_runs(&square, &[], &[], 1465.0).is_empty());
    }

    /// Where the tile border cuts a deck, the deck goes on next door: no railing
    /// along the cut.
    #[test]
    fn no_railing_along_a_tile_border() {
        let tile_m = 1465.0;
        let deck = rect_deck(&[
            [tile_m - 20.0, 100.0],
            [tile_m, 100.0],
            [tile_m, 112.0],
            [tile_m - 20.0, 112.0],
        ]);
        let road = rect_road(tile_m - 20.0, 103.0, tile_m, 109.0);
        let runs = railing_runs(&deck, &[&road], &[], tile_m);
        assert_eq!(runs.len(), 2, "{runs:?}");
        assert!(runs.iter().flatten().all(|p| p[1] != 105.0));
        let total: f32 = runs.iter().map(|r| crate::tunnels::polyline_len(r)).sum();
        assert!((total - 40.0).abs() < 1e-3, "railing {total} m: {runs:?}");
    }

    /// A railing gives way where `keep` says, and keeps its corners elsewhere.
    #[test]
    fn cut_runs_removes_a_stretch_and_keeps_corners() {
        let run = vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]];
        let cut = cut_runs(vec![run], |p, _| !(p[0] > 3.9 && p[0] < 6.1 && p[1] < 0.5));
        assert_eq!(cut.len(), 2, "{cut:?}");
        assert!(
            (cut[0][0][0]).abs() < 1e-4 && (cut[0][cut[0].len() - 1][0] - 3.5).abs() < 1e-4,
            "{cut:?}"
        );
        // The second piece turns the corner at (10, 0).
        assert!(
            cut[1]
                .iter()
                .any(|p| (p[0] - 10.0).abs() < 1e-4 && p[1].abs() < 1e-4),
            "{cut:?}"
        );
        assert!((cut[1][cut[1].len() - 1][1] - 10.0).abs() < 1e-4, "{cut:?}");
        assert_eq!(cut[1].len(), 3, "samples along a segment collapse: {cut:?}");
    }

    /// Over the fixture tile every deck the tiles draw a road onto gets a
    /// railing, and none of it lies on the road.
    #[test]
    fn every_fixture_deck_with_a_road_gets_a_railing_beside_it() {
        let tile_m = 1465.3;
        let decks = tessellate_bridges(GRONINGEN_Z14, tile_m).expect("bridges");
        let (roads, decks) = roads_and_decks(GRONINGEN_Z14, tile_m, &decks).expect("roads");
        let mut railed = 0;
        let mut with_road = 0;
        for (i, d) in decks.iter().enumerate() {
            let ways: Vec<&SurfaceMesh> = roads
                .on_deck
                .iter()
                .filter(|(di, _)| *di as usize == i)
                .map(|(_, m)| m)
                .collect();
            if ways.is_empty() {
                continue;
            }
            with_road += 1;
            let runs = railing_runs(d, &ways, &[], tile_m);
            if !runs.is_empty() {
                railed += 1;
            }
        }
        assert!(with_road > 0);
        assert!(
            railed * 10 >= with_road * 8,
            "{railed} of {with_road} decks with a road got a railing"
        );
    }

    /// **A bridge OSM maps only as flagged ways still gets a deck.** The ring
    /// fixture's Europaplein flyover is four `bridge=yes` trunk carriageways with
    /// no `bridges` outline, and was drawn on the ground across the junction it
    /// crosses (reported 2026-09-26). Every bridge-flagged road here must now
    /// stand on a deck along its whole length, and the flyover's must carry its
    /// carriageway and have an approach to ramp.
    #[test]
    fn a_bridge_mapped_only_as_ways_gets_a_deck() {
        let tile = crate::testdata::groningen_ring();
        let tile_m = 1465.3;
        let areas = super::area_decks(&tile, tile_m);
        let mut decks = super::tessellate_bridges(&tile, tile_m).expect("bridges");
        assert!(
            decks.len() > areas.len(),
            "no deck was built from a street: {} decks, {} outlines",
            decks.len(),
            areas.len()
        );

        let meta = tile.layer("streets").expect("streets");
        let extent = TileData::extent(meta);
        let scale = tile_m / extent;
        let mut checked = 0;
        for f in tile.features(meta) {
            if !flag_of(&f, "bridge")
                || !drawn_street(&f)
                    .is_some_and(|(c, _)| !matches!(c, RoadClass::Path | RoadClass::Minor))
            {
                continue;
            }
            let Geometry::LineString(ls) = &f.geometry else {
                continue;
            };
            let pts: Vec<[f32; 2]> = ls.0.iter().map(|c| [c.x, c.y]).collect();
            for run in clip_polyline(&pts, 0.0, extent) {
                let m: Vec<[f32; 2]> = run.iter().map(|p| [p[0] * scale, p[1] * scale]).collect();
                if crate::tunnels::polyline_len(&m) < STREET_DECK_MIN_M {
                    continue;
                }
                // Every metre of it, bar the ends: a way may overhang an outline by
                // up to `STREET_DECK_MIN_M`, and that sliver is left on the ground.
                for s in m.windows(2) {
                    let (a, b) = (s[0], s[1]);
                    let l = (b[0] - a[0]).hypot(b[1] - a[1]);
                    let steps = l.floor() as usize;
                    for k in 1..steps {
                        let t = k as f32 / l;
                        let p = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
                        let near_end = (p[0] - m[0][0]).hypot(p[1] - m[0][1]) < STREET_DECK_MIN_M
                            || (p[0] - m[m.len() - 1][0]).hypot(p[1] - m[m.len() - 1][1])
                                < STREET_DECK_MIN_M;
                        assert!(
                            near_end || decks.iter().any(|d| d.contains(p)),
                            "a bridge street is off every deck at {p:?}"
                        );
                    }
                }
                checked += 1;
            }
        }
        assert!(checked >= 4, "only {checked} bridge streets checked");

        // Europaplein, between the two main carriageways' spans.
        let europaplein = [1247.9, 125.7];
        let di = decks
            .iter()
            .position(|d| d.contains(europaplein))
            .expect("no deck at the Europaplein flyover");
        assert!(
            di >= areas.len(),
            "the flyover's deck is an outline after all"
        );
        // A clean outline: no edge that runs out and straight back. Those are the
        // zero-width spikes a union of abutting rectangles leaves, and the fascia
        // is extruded from every one of them.
        for d in &decks[areas.len()..] {
            for ring in &d.rings {
                let pts = &ring[..ring.len() - 1];
                for k in 0..pts.len() {
                    let (a, b, c) = (pts[k], pts[(k + 1) % pts.len()], pts[(k + 2) % pts.len()]);
                    let (u, v) = ([b[0] - a[0], b[1] - a[1]], [c[0] - b[0], c[1] - b[1]]);
                    let back = u[0] * v[0] + u[1] * v[1] < 0.0
                        && (u[0] * v[1] - u[1] * v[0]).abs()
                            < 0.01 * u[0].hypot(u[1]) * v[0].hypot(v[1]);
                    assert!(!back, "a deck's outline doubles back on itself at {b:?}");
                }
            }
        }
        let roads = super::tessellate_roads(&tile, tile_m, &mut decks, None, false).expect("roads");
        assert!(
            roads.on_deck.iter().any(|(d, _)| *d as usize == di),
            "no carriageway rides the flyover"
        );
        // Both of the N7's own carriageways ride it, not only the slip lane.
        // The midpoints of the westbound and eastbound spans, tile-local.
        let c = [europaplein[0] + 6.4, europaplein[1] + 3.7];
        for (name, mid) in [
            ("westbound", [c[0] - 6.0, c[1] - 3.5]),
            ("eastbound", [c[0] + 2.0, c[1] + 5.5]),
        ] {
            let carried = roads
                .on_deck
                .iter()
                .filter(|(d, _)| *d as usize == di)
                .any(|(_, m)| {
                    m.indices.chunks(3).any(|t| {
                        let p = [0, 1, 2].map(|k| m.positions[t[k] as usize]);
                        let tri = [[p[0][0], p[0][2]], [p[1][0], p[1][2]], [p[2][0], p[2][2]]];
                        let side = |a: [f32; 2], b: [f32; 2]| {
                            (b[0] - a[0]) * (mid[1] - a[1]) - (b[1] - a[1]) * (mid[0] - a[0])
                        };
                        let (s0, s1, s2) = (
                            side(tri[0], tri[1]),
                            side(tri[1], tri[2]),
                            side(tri[2], tri[0]),
                        );
                        (s0 >= 0.0 && s1 >= 0.0 && s2 >= 0.0)
                            || (s0 <= 0.0 && s1 <= 0.0 && s2 <= 0.0)
                    })
                });
            assert!(
                carried,
                "the N7 {name} runs under its own deck instead of on it"
            );
        }
        let ramp = roads
            .approaches
            .iter()
            .filter(|a| a.deck as usize == di)
            .map(|a| crate::tunnels::polyline_len(&a.points))
            .fold(0.0f32, f32::max);
        assert!(
            ramp >= 40.0,
            "the flyover's longest approach is {ramp:.1} m"
        );
    }

    /// **A bridge's own walls are not drawn standing on the road under it.** The
    /// Europaplein viaduct's screens measured 7–9.5 m from the Europaweg to their
    /// top and its parapets were unmeasured 48 m walls along the deck; both stood
    /// on the road the deck crosses (2026-09-26). A measured metre on a deck is a
    /// canal bridge's parapet and stays, and so does a low wall beside a ramp.
    #[test]
    fn a_bridge_keeps_its_walls_off_the_road_beneath() {
        // A 60 × 30 m deck along x, and a ramp running on east from it.
        let ring = vec![
            [0.0, 0.0],
            [60.0, 0.0],
            [60.0, 30.0],
            [0.0, 30.0],
            [0.0, 0.0],
        ];
        let deck = BridgeDeck {
            positions: Vec::new(),
            indices: Vec::new(),
            rings: vec![ring],
            min: [0.0, 0.0],
            max: [60.0, 30.0],
        };
        let decks = [deck];
        let ramp = [Approach {
            deck: 0,
            points: vec![[60.0, 15.0], [220.0, 15.0]],
        }];
        let along_deck = [[0.0, 1.0], [60.0, 1.0]];
        let along_ramp = [[40.0, 5.0], [220.0, 5.0]];
        let elsewhere = [[0.0, 80.0], [60.0, 80.0]];
        // Beside the deck, past the drawn carriageway: a wide viaduct's parapet.
        let beside_deck = [[0.0, -9.0], [60.0, -9.0]];

        assert!(wall_of_a_bridge(&decks, &ramp, &along_deck, None));
        assert!(wall_of_a_bridge(&decks, &ramp, &along_deck, Some(9.5)));
        assert!(wall_of_a_bridge(&decks, &ramp, &beside_deck, None));
        assert!(
            !wall_of_a_bridge(&decks, &ramp, &along_deck, Some(1.0)),
            "a measured parapet on a deck stays"
        );
        assert!(wall_of_a_bridge(&decks, &ramp, &along_ramp, Some(7.0)));
        assert!(
            wall_of_a_bridge(&decks, &ramp, &along_ramp, None),
            "an unmeasured parapet running on past the bridge way is the bridge's"
        );
        assert!(!wall_of_a_bridge(&decks, &ramp, &along_ramp, Some(1.2)));
        assert!(wall_of_a_bridge(&decks, &ramp, &along_ramp, Some(2.4)));
        assert!(!wall_of_a_bridge(&decks, &ramp, &elsewhere, Some(9.0)));
        assert!(!wall_of_a_bridge(&[], &[], &along_deck, None));
    }

    /// The bridge-flagged streets, decks and approaches of one tile within
    /// `CARTO_BRIDGE_NEAR` metres (default 150) of `CARTO_BRIDGE_AT=lat,lng`, in
    /// tile-local metres — for an approach step `carto-view --site` reports, which
    /// says where it probed but not what the tile holds there.
    ///
    /// ```text
    /// CARTO_BRIDGE_TILE=/tmp/t.mvt CARTO_BRIDGE_XYZ=14/8489/5321 CARTO_BRIDGE_AT=53.2126,6.5422 \
    ///     cargo test -p cartopolis_geo dump_the_bridges_near -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "a diagnostic: needs CARTO_BRIDGE_TILE, CARTO_BRIDGE_XYZ and CARTO_BRIDGE_AT"]
    fn dump_the_bridges_near() {
        let bytes =
            std::fs::read(std::env::var("CARTO_BRIDGE_TILE").expect("tile")).expect("reads");
        let tile = TileData::parse(&bytes).expect("parses");
        let v: Vec<f64> = std::env::var("CARTO_BRIDGE_XYZ")
            .expect("xyz")
            .split('/')
            .filter_map(|p| p.parse().ok())
            .collect();
        let (z, x, y) = (v[0], v[1], v[2]);
        let at: Vec<f64> = std::env::var("CARTO_BRIDGE_AT")
            .expect("at")
            .split(',')
            .filter_map(|p| p.parse().ok())
            .collect();
        let near: f32 = std::env::var("CARTO_BRIDGE_NEAR")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(150.0);
        let n = 2f64.powf(z);
        let lat0 = (std::f64::consts::PI * (1.0 - 2.0 * y / n)).sinh().atan();
        let tile_m = (40_075_016.686 * lat0.cos() / n) as f32;
        let fx = (at[1] + 180.0) / 360.0 * n;
        let fy = (1.0
            - (at[0].to_radians().tan() + 1.0 / at[0].to_radians().cos()).ln()
                / std::f64::consts::PI)
            / 2.0
            * n;
        let p = [
            ((fx - x) * f64::from(tile_m)) as f32,
            ((fy - y) * f64::from(tile_m)) as f32,
        ];
        let close = |q: &[f32; 2]| (q[0] - p[0]).hypot(q[1] - p[1]) <= near;
        println!("tile_m={tile_m:.1} point={p:?}");
        let mut decks = super::tessellate_bridges(&tile, tile_m).expect("bridges");
        for (i, d) in decks.iter().enumerate() {
            if d.rings.iter().flatten().any(close) {
                println!("deck {i}: min {:?} max {:?}", d.min, d.max);
            }
        }
        for l in crate::tunnels::surface_lines(&tile, tile_m) {
            if l.bridge && l.pts.iter().any(close) {
                let f = |q: [f32; 2]| [q[0].round(), q[1].round()];
                println!(
                    "bridge way: {:?} (own {:?}) .. {:?} (own {:?}), {} points",
                    f(l.pts[0]),
                    l.own[0],
                    f(*l.pts.last().unwrap()),
                    l.own[1],
                    l.pts.len()
                );
            }
        }
        let roads = super::tessellate_roads(&tile, tile_m, &mut decks, None, false).expect("roads");
        for a in &roads.approaches {
            if a.points.iter().any(close) {
                let f = |q: [f32; 2]| [q[0].round(), q[1].round()];
                println!(
                    "approach of deck {}: {:?} .. {:?}, {:.0} m",
                    a.deck,
                    f(a.points[0]),
                    f(*a.points.last().unwrap()),
                    crate::tunnels::polyline_len(&a.points)
                );
            }
        }
    }

    /// Where every bridge deck in a tile is, **on Earth**.
    ///
    /// The sibling of `tunnels::tests::dump_the_fixture_bores`, and it exists for
    /// the same reason: `docs/grade-shots.toml`'s own header records that half
    /// its first bridge cameras missed, one of them photographing the city centre
    /// with the viaduct behind it, and re-aiming them by hand missed again on
    /// 2026-09-22. A deck's position is a thing the tiles know exactly, so it
    /// should be printed rather than estimated off a map.
    ///
    /// ```text
    /// CARTO_BRIDGE_TILE=/tmp/t.mvt CARTO_BRIDGE_XYZ=14/8490/5321 \
    ///     cargo test -p cartopolis_geo dump_the_bridge_decks -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "diagnostic: prints where the tile's viaducts are"]
    fn dump_the_bridge_decks() {
        let owned = std::env::var("CARTO_BRIDGE_TILE")
            .ok()
            .map(|p| std::fs::read(p).expect("tile reads"));
        let tile = match &owned {
            Some(b) => TileData::parse(b).expect("tile parses"),
            None => crate::testdata::groningen(),
        };
        let key: Option<(f64, f64, f64)> = std::env::var("CARTO_BRIDGE_XYZ").ok().and_then(|s| {
            let v: Vec<f64> = s.split('/').filter_map(|p| p.parse().ok()).collect();
            (v.len() == 3).then(|| (v[0], v[1], v[2]))
        });
        // The tile's own ground size, derived from its coordinates rather than
        // written down — the same care the tunnel dump takes.
        let tile_m = key.map_or(1471.0, |(z, _, y)| {
            let n = 2f64.powf(z);
            let lat0 = (std::f64::consts::PI * (1.0 - 2.0 * y / n)).sinh().atan();
            (40_075_016.686 * lat0.cos() / n) as f32
        });
        let decks = super::tessellate_bridges(&tile, tile_m).expect("bridges tessellate");
        println!("decks={}", decks.len());
        let mut sorted: Vec<&BridgeDeck> = decks.iter().collect();
        // Biggest first: a viaduct worth photographing is a large structure, and a
        // tile is mostly twenty-metre canal bridges.
        sorted.sort_by(|a, b| {
            let area = |d: &BridgeDeck| (d.max[0] - d.min[0]) * (d.max[1] - d.min[1]);
            area(b).total_cmp(&area(a))
        });
        for (i, d) in sorted.iter().take(8).enumerate() {
            let (cx, cz) = ((d.min[0] + d.max[0]) * 0.5, (d.min[1] + d.max[1]) * 0.5);
            let here = key.map_or(String::new(), |(z, x, y)| {
                let n = 2f64.powf(z);
                let lat0 = (std::f64::consts::PI * (1.0 - 2.0 * y / n)).sinh().atan();
                let span = 40_075_016.686 * lat0.cos() / n;
                let (fx, fy) = (x + f64::from(cx) / span, y + f64::from(cz) / span);
                let lat = (std::f64::consts::PI * (1.0 - 2.0 * fy / n))
                    .sinh()
                    .atan()
                    .to_degrees();
                format!("  at {lat:.6},{:.6}", fx / n * 360.0 - 180.0)
            });
            println!(
                "deck {i}: {:.0}x{:.0} m{here}",
                d.max[0] - d.min[0],
                d.max[1] - d.min[1]
            );
        }
    }
    /// The `bridges` layer is areas, and those areas are the deck a raised bridge
    /// stands on. Nothing drew them before this, which is why a viaduct was only
    /// ever the road ribbon crossing it.
    #[test]
    fn bridges_tessellate_into_decks_with_outlines() {
        let decks = tessellate_bridges(GRONINGEN_Z14, 1000.0).expect("bridges");
        assert!(!decks.is_empty(), "a city tile should hold bridge decks");
        for d in &decks {
            assert_eq!(d.indices.len() % 3, 0);
            assert!(!d.rings.is_empty(), "a deck with no outline has no fascia");
            assert!(
                d.indices.iter().all(|i| (*i as usize) < d.positions.len()),
                "index out of range"
            );
            // Inside the tile, flat, and a plausible size for a structure.
            for p in &d.positions {
                assert!((0.0..=1000.0).contains(&p[0]) && (0.0..=1000.0).contains(&p[2]));
                assert_eq!(
                    p[1], 0.0,
                    "a deck leaves the worker flat; height is measured"
                );
            }
            let span = d.span_m();
            assert!((1.0..=600.0).contains(&span), "deck span {span} m");
            // The centre of the bounding box is a weak test of `contains`, but a
            // deck that answers "no" everywhere would silently disable both the
            // ribbon suppression and the height sampling.
            let mid = [(d.min[0] + d.max[0]) * 0.5, (d.min[1] + d.max[1]) * 0.5];
            assert!(
                !d.contains([d.min[0] - 5.0, mid[1]]),
                "outside is not inside"
            );
        }
        assert!(
            decks
                .iter()
                .any(|d| d.contains([(d.min[0] + d.max[0]) * 0.5, (d.min[1] + d.max[1]) * 0.5])),
            "no deck contains its own centre — point-in-polygon is broken"
        );
    }

    /// The carriageway is cut **out of** the slab it stands on, so the two share a
    /// plane with nothing left to arbitrate. If the subtraction silently did
    /// nothing, the road and the deck would be two coplanar opaque surfaces and
    /// the deck — which sits at the top of the ground ladder — would win, i.e. the
    /// bare slab would be back with no other symptom.
    #[test]
    fn the_carriageway_is_cut_out_of_the_deck() {
        let decks = tessellate_bridges(GRONINGEN_Z14, 1000.0).expect("bridges");
        let (roads, carved) = roads_and_decks(GRONINGEN_Z14, 1000.0, &decks).expect("roads");
        let area = |d: &BridgeDeck| -> f64 {
            d.indices
                .chunks_exact(3)
                .map(|t| {
                    let p: Vec<[f32; 3]> = t.iter().map(|i| d.positions[*i as usize]).collect();
                    f64::from(
                        ((p[1][0] - p[0][0]) * (p[2][2] - p[0][2])
                            - (p[2][0] - p[0][0]) * (p[1][2] - p[0][2]))
                            .abs(),
                    ) * 0.5
                })
                .sum()
        };
        let mut carried = 0;
        for (di, (before, after)) in decks.iter().zip(&carved).enumerate() {
            let rides = roads.on_deck.iter().any(|(d, _)| *d as usize == di);
            if !rides {
                assert!(
                    (area(after) - area(before)).abs() < 1e-3,
                    "deck {di} carries no road but was cut anyway"
                );
                continue;
            }
            carried += 1;
            assert!(
                area(after) < area(before),
                "deck {di} carries a road and kept its whole surface"
            );
            // …and it is not cut away entirely: what is left is the verge and the
            // parapet either side of the carriageway, which is what makes a bridge
            // read as a structure rather than as a strip of tarmac in mid-air.
            assert!(
                area(after) > 0.05 * area(before),
                "deck {di} lost {:.0}% of its surface to the road on it",
                100.0 * (1.0 - area(after) / area(before))
            );
        }
        assert!(carried > 0, "no deck in the fixture carries a road");
    }
}
