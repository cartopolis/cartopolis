//! The drivable network, read off the `streets` layer the cell already parsed.
//!
//! This is the pure half of ambient traffic: it turns a tile into a list of
//! centrelines a car may drive along, with the direction it may drive them in and
//! how fast. The ECS half — the car pool, the steering and the meshes — is
//! `cartopolis`'s `systems::traffic`.
//!
//! Like the street furniture and the parked vehicles, it costs **no request and
//! no extra decode**: `map_geometry` fetches one MVT body per cell and this reads
//! a layer that body already contains and that `tessellate_roads` has already
//! walked. Unlike them it is not a fill — every line here is a surveyed road, and
//! the only invented thing is the traffic on it.
//!
//! Four things shape it:
//!
//! * **Direction is a three-way answer, not a flag.** Shortbread carries
//!   `oneway` *and* `oneway_reverse` — the second meaning the way is one-way
//!   against its own digitisation order. `street_style` ORs them together because
//!   a painted road is the same width either way; traffic cannot, because the two
//!   send a car in opposite directions. Getting this wrong is not subtle: half
//!   the one-way streets in the city would run backwards.
//! * **Tunnels are refused**, by going through
//!   [`drawn_street`](crate::vector_tiles::drawn_street) — the same refusal the
//!   geometry, the raster and the street labels inherit. A car on a tunnelled
//!   carriageway drives along the surface above it.
//! * **Lines are simplified and short ones dropped.** A z14 street carries
//!   junction-accurate detail no car needs, and every vertex is a step in the
//!   steering loop. [`DRIVE_SIMPLIFY_M`] is the same idea as the labels'
//!   `LABEL_SIMPLIFY_M` and for the same reason.
//! * **Ownership is half-open on the first vertex**, as everywhere else here, so
//!   a road crossing a cell boundary is emitted once. The geometry is taken
//!   **unclipped** — a car that drives off the edge of its own cell is on the
//!   same road, and clipping would put a junction there that the map does not
//!   have.

use mvt_reader::feature::Feature;

use crate::vector_tiles::{drawn_street, kind_of, TileData};

/// Vertices closer together than this are merged. A z14 street line has
/// junction-accurate detail; a car following it needs a driving line.
const DRIVE_SIMPLIFY_M: f32 = 8.0;

/// Lines shorter than this are dropped. A stub that takes under two seconds to
/// drive is a place to hand a car off, not a road to spawn one on.
const MIN_DRIVE_LEN_M: f32 = 25.0;

/// Which way traffic may run along a [`DriveLine`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// Both ways — the usual case.
    Both,
    /// Only in the polyline's own order.
    Forward,
    /// Only against it (`oneway_reverse`).
    Backward,
}

/// Who may travel along a line.
///
/// Not an enum, because the common case is **both**: a residential street carries
/// cars on the carriageway and people on the pavement beside it, and modelling
/// that as one-or-the-other would either empty the pavements or put pedestrians
/// in the road.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Uses {
    pub drive: bool,
    pub walk: bool,
}

/// One travelable centreline, in the tessellator's tile-local metre frame (origin
/// at the cell's north-west corner, +x east, +z south).
#[derive(Debug, Clone)]
pub struct DriveLine {
    pub points: Vec<[f32; 2]>,
    pub flow: Flow,
    /// Free-flow speed in m/s **for a car**. Zero on a line no car may use;
    /// walkers carry their own pace and do not read this.
    pub speed: f32,
    /// Half the carriageway, so a car can sit in its own lane rather than on the
    /// centre line — and so a walker can be put on the pavement *outside* it.
    pub half_width: f32,
    pub uses: Uses,
}

impl DriveLine {
    /// Total length in metres.
    #[must_use]
    pub fn length(&self) -> f32 {
        self.points
            .windows(2)
            .map(|w| {
                let (dx, dz) = (w[1][0] - w[0][0], w[1][1] - w[0][1]);
                (dx * dx + dz * dz).sqrt()
            })
            .sum()
    }
}

/// Free-flow speed in m/s for a Shortbread `streets` kind, or `None` for the
/// kinds a car may not drive on.
///
/// Deliberately below the legal limits. These are *ambient* cars on a city map:
/// what has to read correctly is that a motorway moves much faster than a
/// residential street, not that either matches a speedometer. A car doing a true
/// 100 km/h past a stationary camera is a streak.
#[must_use]
pub fn drive_speed(kind: &str) -> Option<f32> {
    Some(match kind {
        "motorway" => 26.0,
        "trunk" => 22.0,
        "primary" => 15.0,
        "secondary" => 13.0,
        "tertiary" => 11.0,
        "residential" | "unclassified" => 7.5,
        "living_street" => 4.5,
        "service" => 4.5,
        // Everything else is a path, a railway or something we do not recognise.
        // The tree scatter's rule applies: no traffic beats invented traffic.
        _ => return None,
    })
}

/// Whether people may walk along a Shortbread `streets` kind.
///
/// Broader than [`drive_speed`] in one direction and narrower in the other. A
/// footway, a path, a pedestrian street or a cycleway carries people and no cars;
/// an ordinary street carries both, because it has pavements the tile does not
/// model separately and putting nobody on them empties the city. A motorway, a
/// trunk road and every railway carry nobody — a pedestrian strolling up the A28
/// is worse than an empty verge.
///
/// `steps` is deliberately included even though nothing here climbs: a flight of
/// steps between two streets is a route people are on, and a walker traversing it
/// at pavement level reads as a person on a staircase from any distance this is
/// seen at. It is listed rather than falling out of a default so that the choice
/// is visible.
#[must_use]
pub fn walk_ok(kind: &str) -> bool {
    matches!(
        kind,
        "footway"
            | "path"
            | "pedestrian"
            | "steps"
            | "cycleway"
            | "living_street"
            | "residential"
            | "unclassified"
            | "service"
            | "tertiary"
            | "secondary"
    )
}

/// Read a tile's `streets` layer as a travelable network.
///
/// Takes the cell's already-parsed [`TileData`] rather than the tile body, for
/// the reason every other consumer on this worker does: they each read a
/// different layer out of the same bytes.
pub fn extract_drive_lines(tile: &TileData, tile_m: f32) -> Result<Vec<DriveLine>, String> {
    use geo_types::Geometry;

    let Some(meta) = tile.layer("streets") else {
        return Ok(Vec::new());
    };
    let features = tile.features(meta);
    let extent = TileData::extent(meta);
    let scale = tile_m / extent;

    let mut out = Vec::new();
    for feature in features {
        let Some(kind) = kind_of(feature) else {
            continue;
        };
        let drive = drive_speed(kind);
        let walk = walk_ok(kind);
        if drive.is_none() && !walk {
            continue;
        }
        let speed = drive.unwrap_or(0.0);
        // The same refusal the geometry and the raster make — and the reason it
        // is shared rather than restated is that a tunnel that is drawn in one
        // place and not another is worse than either answer.
        let Some((_, half_width)) = drawn_street(feature) else {
            continue;
        };
        let flow = flow_of(feature);

        let lines: Vec<&geo_types::LineString<f32>> = match &feature.geometry {
            Geometry::LineString(ls) => vec![ls],
            Geometry::MultiLineString(ml) => ml.iter().collect(),
            _ => continue,
        };
        for line in lines {
            let Some(first) = line.0.first() else {
                continue;
            };
            // Half-open ownership on the first vertex: a road crossing a cell
            // boundary belongs to exactly one cell and is emitted once, with no
            // tolerance to tune. The rest of the line is kept **unclipped** — a
            // car driving off the edge is still on the same road.
            if first.x < 0.0 || first.y < 0.0 || first.x >= extent || first.y >= extent {
                continue;
            }
            let raw: Vec<[f32; 2]> = line
                .0
                .iter()
                // `mvt-reader` clips its cursor at `i32::MAX` rather than at the
                // extent, so a wild coordinate is a real possibility here — see
                // `TILE_BUFFER_FRAC`. Written as a positive test so a NaN is
                // rejected rather than admitted.
                .filter(|c| c.x.abs() < extent * 8.0 && c.y.abs() < extent * 8.0)
                .map(|c| [c.x * scale, c.y * scale])
                .collect();
            let points = simplify(&raw, DRIVE_SIMPLIFY_M);
            if points.len() < 2 {
                continue;
            }
            let candidate = DriveLine {
                points,
                flow,
                speed,
                half_width,
                uses: Uses {
                    drive: drive.is_some(),
                    walk,
                },
            };
            if candidate.length() < MIN_DRIVE_LEN_M {
                continue;
            }
            out.push(candidate);
        }
    }
    Ok(out)
}

/// Which way traffic runs, from the two flags Shortbread carries.
///
/// **`oneway` and `oneway_reverse` are different answers and must not be ORed.**
/// `vector_tiles::street_style` does OR them, correctly, because a painted road
/// is the same width whichever way it runs; a car is not, and treating the two
/// as one sends every `oneway_reverse` street's traffic backwards.
///
/// Split from the feature so the rule is a pure function with a test on it — the
/// MVT plumbing around it is not what can be got wrong here.
#[must_use]
pub fn flow_from(oneway: bool, oneway_reverse: bool) -> Flow {
    if oneway_reverse {
        Flow::Backward
    } else if oneway {
        Flow::Forward
    } else {
        Flow::Both
    }
}

fn flow_of(feature: &Feature) -> Flow {
    flow_from(
        crate::vector_tiles::flag_of(feature, "oneway"),
        crate::vector_tiles::flag_of(feature, "oneway_reverse"),
    )
}

/// Drop vertices closer together than `min_m`, keeping both ends.
///
/// A cheap running filter rather than Douglas–Peucker: the input is already a
/// generalised z14 line, the output is a driving line and not a drawn one, and
/// the only thing that matters is that no step is so short the steering loop
/// spends a frame on it.
fn simplify(points: &[[f32; 2]], min_m: f32) -> Vec<[f32; 2]> {
    if points.len() < 2 {
        return points.to_vec();
    }
    let min2 = min_m * min_m;
    let mut out = vec![points[0]];
    for p in &points[1..] {
        let last = *out.last().expect("seeded above");
        let (dx, dz) = (p[0] - last[0], p[1] - last[1]);
        if dx * dx + dz * dz >= min2 {
            out.push(*p);
        }
    }
    // The end always survives, or a line loses its last stretch and every car on
    // it hands off early — but never as a duplicate of the point before it.
    let end = *points.last().expect("checked above");
    let last = *out.last().expect("seeded above");
    if (end[0] - last[0]).abs() > 1e-3 || (end[1] - last[1]).abs() > 1e-3 {
        out.push(end);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::testdata::groningen as fixture;

    const TILE_M: f32 = 1472.0;

    #[test]
    fn the_fixture_yields_a_drivable_network() {
        let lines = extract_drive_lines(&fixture(), TILE_M).expect("extract");
        assert!(
            lines.len() > 10,
            "a city tile should be drivable: {}",
            lines.len()
        );
        for l in &lines {
            assert!(l.points.len() >= 2);
            assert!(l.length() >= MIN_DRIVE_LEN_M);
            assert!(l.uses.drive || l.uses.walk, "a line nobody may use");
            if l.uses.drive {
                assert!(l.speed > 0.0 && l.speed <= 30.0);
            }
            assert!(l.half_width > 0.0);
        }
        assert!(
            lines.iter().any(|l| l.uses.walk && !l.uses.drive),
            "a city tile has footways cars may not use"
        );
        assert!(
            lines.iter().any(|l| l.uses.drive && l.uses.walk),
            "an ordinary street carries both"
        );
    }

    /// Nobody walks up a motorway or along a railway, and the two lists are not
    /// the same list — a footway carries people and no cars, a motorway the
    /// reverse.
    #[test]
    fn the_two_networks_differ_in_both_directions() {
        for kind in ["footway", "path", "pedestrian", "cycleway", "steps"] {
            assert!(walk_ok(kind), "{kind} carries people");
            assert!(drive_speed(kind).is_none(), "{kind} carries no cars");
        }
        for kind in ["motorway", "trunk"] {
            assert!(!walk_ok(kind), "{kind} carries nobody on foot");
            assert!(drive_speed(kind).is_some(), "{kind} carries cars");
        }
        for kind in ["rail", "tram", "subway"] {
            assert!(!walk_ok(kind) && drive_speed(kind).is_none(), "{kind}");
        }
        for kind in ["residential", "living_street", "service"] {
            assert!(
                walk_ok(kind) && drive_speed(kind).is_some(),
                "{kind} carries both"
            );
        }
    }

    /// The one thing in this module a picture cannot show and a drive can: a
    /// `oneway_reverse` street must send its traffic the *other* way. The raster
    /// path ORs the two flags because a road's width does not care; traffic does.
    #[test]
    fn oneway_and_oneway_reverse_are_opposite_answers() {
        assert_eq!(flow_from(true, false), Flow::Forward);
        assert_eq!(flow_from(false, true), Flow::Backward);
        assert_eq!(flow_from(false, false), Flow::Both);
        // A way tagged both ways round is nonsense in the source; `reverse` wins
        // rather than the pair silently collapsing to "forward", which is the
        // answer ORing them would give.
        assert_eq!(flow_from(true, true), Flow::Backward);
    }

    /// Nothing may drive on a footpath, a cycleway or a railway. An invented car
    /// on a towpath is worse than an empty one.
    #[test]
    fn only_roads_are_drivable() {
        for kind in [
            "footway",
            "path",
            "cycleway",
            "steps",
            "pedestrian",
            "rail",
            "tram",
            "subway",
            "light_rail",
        ] {
            assert!(drive_speed(kind).is_none(), "{kind} must not be drivable");
        }
        for kind in ["motorway", "primary", "residential", "service"] {
            assert!(drive_speed(kind).is_some(), "{kind} must be drivable");
        }
    }

    /// A motorway has to be clearly faster than a residential street, or the
    /// whole speed table is decoration.
    #[test]
    fn the_speed_table_is_ordered() {
        let motorway = drive_speed("motorway").expect("drivable");
        let primary = drive_speed("primary").expect("drivable");
        let residential = drive_speed("residential").expect("drivable");
        let living = drive_speed("living_street").expect("drivable");
        assert!(motorway > primary && primary > residential && residential > living);
    }

    /// Simplification keeps both ends and never emits a zero-length step — a
    /// zero step is a frame the steering loop spends going nowhere.
    #[test]
    fn simplify_keeps_both_ends_and_drops_the_crowd() {
        let dense: Vec<[f32; 2]> = (0..=40).map(|i| [i as f32, 0.0]).collect();
        let out = simplify(&dense, 8.0);
        assert_eq!(out.first(), Some(&[0.0, 0.0]));
        assert_eq!(out.last(), Some(&[40.0, 0.0]));
        assert!(out.len() < dense.len(), "nothing was dropped");
        for w in out.windows(2) {
            let (dx, dz) = (w[1][0] - w[0][0], w[1][1] - w[0][1]);
            assert!(dx * dx + dz * dz > 1e-6, "a zero-length step survived");
        }
    }
}
