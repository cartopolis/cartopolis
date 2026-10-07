//! Street-name label lines: which streets carry a name, simplified and measured in metres for
//! the client's street_labels to place text along.

use super::*;

/// A named street as text-ready geometry: the centre line a label is laid along,
/// in tile-local metres.
///
/// The name comes from Shortbread's `street_labels` layer and not from `streets`,
/// because that is where the source puts it — the two layers are merged and
/// generalised independently, so there is no id to join on (see
/// `routing::LabelSeg`, which pays the same price for the turn list). For a
/// *label* that costs nothing: the line only has to run roughly down the street
/// it names, and `street_labels` is exactly a per-name polyline.
#[derive(Debug, Clone)]
pub struct StreetLabelLine {
    pub name: String,
    /// Drawing priority, low first: a motorway keeps its label when the screen
    /// is too crowded to fit an alley's.
    pub rank: u8,
    /// Polyline in tile-local metres (x east, y south) — the frame the surface
    /// meshes are built in, so the same cell origin places both.
    pub points: Vec<[f32; 2]>,
}

/// Vertices closer together than this are merged. z14 street lines carry
/// junction-accurate detail a text baseline cannot use, and every vertex is a
/// point projected per frame.
const LABEL_SIMPLIFY_M: f32 = 6.0;

/// Lines shorter than this are dropped outright: nothing that fits a name at any
/// readable size, so they would only cost projection work to fail the on-screen
/// length test later.
const LABEL_MIN_LEN_M: f32 = 15.0;

/// Read the tile's `street_labels` layer as one polyline per named street.
///
/// Clipped to the tile like the road ribbons are ([`clip_polyline`]), so a street
/// crossing a cell boundary contributes its own stretch to each cell rather than a
/// full copy to both — the label renderer's same-name spacing rule then thins the
/// repeats by *screen* distance, which is the only place that judgement can be made.
pub fn extract_street_labels(tile: &TileData, tile_m: f32) -> Result<Vec<StreetLabelLine>, String> {
    let Some(meta) = tile.layer("street_labels") else {
        return Ok(Vec::new());
    };
    let features = tile.features(meta);
    let extent = TileData::extent(meta);
    let scale = tile_m / extent;

    let mut out = Vec::new();
    for feature in features {
        // A tunnel's name is not written on the ground above it, for exactly the
        // reason its carriageway is not drawn there (`drawn_street`): the label
        // would be laid across whatever genuinely *is* up there — the junction the
        // tunnel passes under, or the park it runs beneath. Shortbread puts `tunnel`
        // on `street_labels` as well as on `streets`, so this costs nothing.
        //
        // There is no matching rule for a bridge, and it is not an oversight:
        // `street_labels` carries no `bridge` attribute (checked 2026-08-20 on the
        // Groningen fixture), so a name on a viaduct cannot be told from one on the
        // road underneath it. See `docs/notes/grade-separation.md`.
        // The bare `tunnel` flag here, not `tunnels::underground`: that predicate
        // also reads `layer`, which Shortbread carries on `streets` and **not**
        // on `street_labels`, so asking for it would answer "level 0" for every
        // name in the layer and write every tunnel's name across the park over it.
        // The cost is that an arcade's name is still withheld, which is the
        // behaviour this layer has always had.
        if flag_of(feature, "tunnel") {
            continue;
        }
        let Some(name) = str_prop(feature, "name").filter(|n| !n.is_empty()) else {
            continue;
        };
        let rank = label_rank(kind_of(feature).unwrap_or(""));
        let mut lines: Vec<&geo_types::LineString<f32>> = Vec::new();
        match &feature.geometry {
            Geometry::LineString(ls) => lines.push(ls),
            Geometry::MultiLineString(mls) => lines.extend(mls.iter()),
            _ => continue,
        }
        for ls in lines {
            let pts: Vec<[f32; 2]> =
                ls.0.iter()
                    .filter(|c| c.x.is_finite() && c.y.is_finite())
                    .map(|c| [c.x, c.y])
                    .collect();
            for run in clip_polyline(&pts, 0.0, extent) {
                let metres: Vec<[f32; 2]> =
                    run.iter().map(|p| [p[0] * scale, p[1] * scale]).collect();
                let points = simplify_polyline(&metres, LABEL_SIMPLIFY_M);
                if polyline_len(&points) < LABEL_MIN_LEN_M {
                    continue;
                }
                out.push(StreetLabelLine {
                    name: name.to_string(),
                    rank,
                    points,
                });
            }
        }
    }
    Ok(out)
}

/// Label priority from Shortbread's `kind`. Same ordering the road styling uses,
/// collapsed to five bands — this only has to decide who loses a collision.
fn label_rank(kind: &str) -> u8 {
    match kind {
        "motorway" | "trunk" => 0,
        "primary" => 1,
        "secondary" | "tertiary" => 2,
        "residential" | "unclassified" | "living_street" | "pedestrian" => 3,
        _ => 4,
    }
}

/// Read a string property off a feature.
fn str_prop<'a>(feature: &'a Feature, key: &str) -> Option<&'a str> {
    match feature.properties.as_ref()?.get(key)? {
        Value::String(s) => Some(s.as_str()),
        _ => None,
    }
}

/// Drop vertices within `min_step` of the one before, always keeping the last —
/// so the line's total length and end direction survive.
fn simplify_polyline(points: &[[f32; 2]], min_step: f32) -> Vec<[f32; 2]> {
    let mut out: Vec<[f32; 2]> = Vec::with_capacity(points.len());
    for (i, p) in points.iter().enumerate() {
        let last = out.last().copied();
        let keep = match last {
            None => true,
            Some(q) => i == points.len() - 1 || (p[0] - q[0]).hypot(p[1] - q[1]) >= min_step,
        };
        if keep {
            out.push(*p);
        }
    }
    // The final vertex may have landed on top of its predecessor.
    if out.len() >= 2 {
        let (a, b) = (out[out.len() - 2], out[out.len() - 1]);
        if (a[0] - b[0]).hypot(a[1] - b[1]) < 1e-3 {
            out.pop();
        }
    }
    out
}

fn polyline_len(points: &[[f32; 2]]) -> f32 {
    points
        .windows(2)
        .map(|w| (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector_tiles::bytes::extract_street_labels;

    // A real z14 tile over Groningen, from the VersaTiles Shortbread source
    // (now self-hosted at tiles.cartopolis.org; the archive is identical).
    use crate::testdata::GRONINGEN_Z14;

    /// A tunnel's name must not be written on the ground over it.
    ///
    /// Same rule the carriageway already follows (`drawn_street`) and the same
    /// consequence for missing it: the label lands across whatever genuinely *is* up
    /// there — the junction the tunnel passes under, the park it runs beneath.
    ///
    /// Asserted on geometry rather than on names, because Shortbread merges by
    /// attribute and a street with a tunnel section keeps a *separate* surface
    /// feature under the same name — the fixture has nine tunnel-flagged label
    /// features and not one name that is tunnel-only. So "Kastanjelaan is not
    /// labelled" is the wrong claim; "nothing is labelled along the buried stretch
    /// of it" is the right one.
    ///
    /// There is no matching rule for bridges, and that is not an oversight:
    /// `street_labels` carries no `bridge` attribute (checked 2026-08-20 on this
    /// fixture), so a name on a viaduct cannot be told from one on the road beneath.
    /// `docs/notes/grade-separation.md` has it.
    #[test]
    fn a_tunnel_gets_no_street_label_along_its_buried_stretch() {
        const TILE_M: f32 = 1500.0;
        let tile = TileData::parse(GRONINGEN_Z14).expect("parse");
        let meta = tile.layer("street_labels").expect("street_labels layer");
        let scale = TILE_M / TileData::extent(meta);

        // A point in the middle of each tunnel run, in the metres frame the output
        // uses.
        let mut probes: Vec<(String, [f32; 2])> = Vec::new();
        for f in tile.features(meta) {
            if !flag_of(f, "tunnel") {
                continue;
            }
            let Some(name) = str_prop(f, "name").filter(|n| !n.is_empty()) else {
                continue;
            };
            let mut lines: Vec<&geo_types::LineString<f32>> = Vec::new();
            match &f.geometry {
                Geometry::LineString(ls) => lines.push(ls),
                Geometry::MultiLineString(mls) => lines.extend(mls.iter()),
                _ => continue,
            }
            for ls in lines {
                // The midpoint of the middle *segment*, never a vertex. A vertex
                // would routinely be the tunnel's entrance, which the surface run of
                // the same street shares — and a probe sitting on a shared node
                // reports a hit of exactly 0.0 m against a label that is correctly
                // drawn.
                if ls.0.len() < 2 {
                    continue;
                }
                let m = (ls.0.len() - 1) / 2;
                let (a, b) = (ls.0[m], ls.0[m + 1]);
                probes.push((
                    name.to_string(),
                    [(a.x + b.x) * 0.5 * scale, (a.y + b.y) * 0.5 * scale],
                ));
            }
        }
        assert!(
            !probes.is_empty(),
            "the fixture carries no named tunnel label — this test proves nothing, \
             and a source that stopped shipping `tunnel` on `street_labels` would \
             look exactly like this"
        );

        // Half the simplification tolerance: a surviving label vertex lands within a
        // few metres of the centre line it came from, and nothing else on the map is
        // that close to a tunnel's midpoint while carrying the tunnel's own name.
        let near_m = LABEL_SIMPLIFY_M * 0.5;
        let drawn = super::extract_street_labels(&tile, TILE_M).expect("extract");
        for (name, probe) in &probes {
            for line in drawn.iter().filter(|l| &l.name == name) {
                for p in &line.points {
                    let d = ((p[0] - probe[0]).powi(2) + (p[1] - probe[1]).powi(2)).sqrt();
                    assert!(
                        d > near_m,
                        "{name} is labelled {d:.1} m from the middle of its own \
                         tunnel run — the buried stretch was drawn on the surface"
                    );
                }
            }
        }
    }

    #[test]
    fn street_labels_come_out_named_and_in_metres() {
        let tile_m = 1500.0;
        let labels = extract_street_labels(GRONINGEN_Z14, tile_m).expect("extract");
        assert!(
            labels.len() > 50,
            "a city tile should carry plenty of named streets, got {}",
            labels.len()
        );
        assert!(labels.iter().all(|l| !l.name.trim().is_empty()));
        // Tile-local metres, clipped to the tile: a line escaping these bounds
        // would be drawn into the neighbouring cell's streets.
        for l in &labels {
            assert!(l.points.len() >= 2, "{}: degenerate line", l.name);
            for p in &l.points {
                assert!(
                    (-0.5..=tile_m + 0.5).contains(&p[0]) && (-0.5..=tile_m + 0.5).contains(&p[1]),
                    "{} escapes the tile at {p:?}",
                    l.name
                );
            }
            assert!(polyline_len(&l.points) >= LABEL_MIN_LEN_M);
        }
        // The rank is what decides who survives a crowded frame, so the tile has
        // to actually carry a spread of classes rather than one default.
        let ranks: std::collections::HashSet<u8> = labels.iter().map(|l| l.rank).collect();
        assert!(
            ranks.len() > 2,
            "every street came out the same class: {ranks:?}"
        );
    }

    #[test]
    fn simplify_keeps_the_shape_and_both_ends() {
        // A line with a burst of near-coincident vertices in the middle: the
        // dense stretch collapses, the end points and the length do not.
        let mut points = vec![[0.0f32, 0.0]];
        for i in 1..20 {
            points.push([i as f32 * 0.5, 0.0]);
        }
        points.push([100.0, 0.0]);
        let simplified = simplify_polyline(&points, LABEL_SIMPLIFY_M);
        assert!(simplified.len() < points.len() / 2);
        assert_eq!(simplified.first(), Some(&[0.0, 0.0]));
        assert_eq!(simplified.last(), Some(&[100.0, 0.0]));
        assert!((polyline_len(&simplified) - 100.0).abs() < 1e-3);
    }
}
