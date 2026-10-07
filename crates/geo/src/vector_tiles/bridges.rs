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
