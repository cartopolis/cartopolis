//! Named pieces of world the server can install into itself.
//!
//! # Why the content is here and not in a file next to the tool
//!
//! It began as a Lua file in the repository and a Python script that read it,
//! built a prim specification and posted it. That is wrong twice over. The
//! *definition* of a demo — which prims, where, what shape, which script — is a
//! statement about this world, so it belongs to the thing that owns the world;
//! and a deployed simulator has no repository checked out beside it, so content
//! kept in the tree is content a running server cannot install.
//!
//! So the scripts are `include_str!`-ed and travel inside the binary, and the
//! specifications are ordinary Rust beside them. `tools/author.py install
//! firebird` is now one HTTP call with a name in it; everything it used to
//! decide is decided here.
//!
//! # A pack is a canned request, not a second write path
//!
//! Each prim is a [`PrimSpec`] — the *same* type an HTTP body deserialises into —
//! and installing one runs it through the same handler an author's `POST` does.
//! There is one code path that creates a prim, and a pack cannot drift from it or
//! quietly acquire abilities the API does not have.
//!
//! # Installing twice replaces, and it matches on provenance
//!
//! Installing a pack removes whatever that pack installed last time, because the
//! alternative — a second firebird circling the same city every time somebody
//! re-ran the command — is the more surprising of the two.
//!
//! What it matches on is `prims.origin` (`V14__prim_origin.sql`), and that column
//! exists because the obvious cheaper thing does not work. The first version
//! matched the prim *names* a pack declares, and the pack broke it itself: the
//! beacon's script renames it to whatever its label currently says, so the second
//! install found nothing called "Beacon", left the old one standing and added
//! another. A script may rewrite any field a client can see; provenance is a fact
//! about the row and is not one of them.

use crate::authoring::{ParticleSpec, PrimSpec};

/// A named piece of world.
pub struct Pack {
    /// What `POST /api/admin/content/{name}` is keyed on.
    pub name: &'static str,
    /// One line, for the listing.
    pub summary: &'static str,
    /// What it installs. Order matters only in that they are created in it.
    pub prims: Vec<PrimSpec>,
}

/// Groningen's Grote Markt — where the beacon stands, and the place this world
/// has been anchored to since there was one.
const MARKT_LAT: f64 = 53.2194;
const MARKT_LNG: f64 = 6.5665;

/// Every pack this build carries.
#[must_use]
pub fn packs() -> Vec<Pack> {
    vec![firebird(), football(), theremin()]
}

/// One pack by name.
#[must_use]
pub fn pack(name: &str) -> Option<Pack> {
    packs().into_iter().find(|p| p.name == name)
}

/// The firebird and the beacon that calls it.
///
/// Two prims and two scripts, because the pair is the demonstration: a button
/// cannot reach the thing it controls except over a channel (`send` /
/// `on_message`), and that is the only way any prim reaches any other.
fn firebird() -> Pack {
    Pack {
        name: "firebird",
        summary: "A burning thing that circles the city, and a beacon on the \
                  Grote Markt that calls it in and sends it back out.",
        prims: vec![
            PrimSpec {
                name: Some("Firebird".into()),
                shape: Some("cylinder".into()),
                latitude: Some(MARKT_LAT),
                longitude: Some(MARKT_LNG),
                altitude: Some(45.0),
                scale: Some([1.6, 0.9, 1.6]),
                color: Some([0.09, 0.07, 0.06]),
                // Alight in the very first snapshot that carries it, before its
                // own `on_start` has run — and a demonstration that an emitter is
                // an ordinary authored field rather than something only a script
                // can set. Only what differs from the fire preset is named, which
                // is the same way `particles` takes a rules table.
                particles: Some(Some(ParticleSpec {
                    count: Some(120),
                    speed: Some(2.2),
                    source_radius: Some(0.6),
                    size_start: Some(1.1),
                    turbulence: Some(1.2),
                    ..ParticleSpec::default()
                })),
                script: Some(include_str!("../content/firebird.lua").into()),
                ..PrimSpec::default()
            },
            PrimSpec {
                name: Some("Beacon".into()),
                shape: Some("box".into()),
                // A few metres off the anchor, so it is beside the middle of the
                // square rather than exactly on the point every other thing in
                // this repository uses as its example coordinate.
                latitude: Some(53.21935),
                longitude: Some(6.56675),
                altitude: Some(1.3),
                scale: Some([1.6, 2.6, 0.28]),
                color: Some([1.0, 0.42, 0.08]),
                // Alight before its own `on_start` runs, for the same reason the
                // firebird is: the very first snapshot that carries it should
                // already look like the thing it is.
                particles: Some(Some(ParticleSpec {
                    count: Some(70),
                    life: Some(1.6),
                    speed: Some(1.1),
                    spread_deg: Some(16.0),
                    gravity: Some(1.6),
                    source_radius: Some(0.35),
                    size_start: Some(0.55),
                    size_end: Some(0.08),
                    turbulence: Some(0.7),
                    ..ParticleSpec::default()
                })),
                script: Some(include_str!("../content/beacon.lua").into()),
                ..PrimSpec::default()
            },
        ],
    }
}

/// The middle of each goal line on the Euroborg's pitch — FC Groningen's
/// stadium — from OpenStreetMap way 185043967 (106 × 68 m, running north–south).
/// `football.lua` carries the same two numbers; a test holds them together.
const EUROBORG_NORTH_GOAL: (f64, f64) = (53.206_619_75, 6.591_665_8);
const EUROBORG_SOUTH_GOAL: (f64, f64) = (53.205_663_15, 6.591_642_7);

/// A goal mouth, inside the posts.
const GOAL_WIDTH_M: f64 = 7.32;
const GOAL_HEIGHT_M: f32 = 2.44;
/// Posts and crossbar.
const GOAL_BAR_M: f32 = 0.12;
/// How far the net runs back from the line.
const GOAL_DEPTH_M: f64 = 2.0;

/// A point on the Euroborg pitch, `along` metres from the centre spot toward the
/// north goal and `across` metres square to that (roughly east). Returns the geo
/// anchor and the pitch's heading, in degrees about the vertical, that a box
/// laid square to the goal line wants.
fn pitch_point(along: f64, across: f64) -> (f64, f64, f32) {
    let (n, s) = (EUROBORG_NORTH_GOAL, EUROBORG_SOUTH_GOAL);
    let centre = ((n.0 + s.0) / 2.0, (n.1 + s.1) / 2.0);
    let (m_lat, m_lng) = cartopolis_core::degree_scale_m(centre.0);
    let (ae, an) = ((n.1 - s.1) * m_lng, (n.0 - s.0) * m_lat);
    let len = (ae * ae + an * an).sqrt();
    let (ae, an) = (ae / len, an / len);
    // Square to the axis, pointing east-ish: (an, -ae) in (east, north).
    let east = along * ae + across * an;
    let north = along * an - across * ae;
    // A box's local x turned about +y by `a` points (cos a, 0, -sin a) in the
    // client frame (x east, z south), i.e. north by sin a. The across direction
    // is north by -ae.
    let heading = (-ae).asin().to_degrees() as f32;
    (centre.0 + north / m_lat, centre.1 + east / m_lng, heading)
}

fn pitch_half_length() -> f64 {
    let (n, s) = (EUROBORG_NORTH_GOAL, EUROBORG_SOUTH_GOAL);
    cartopolis_core::ground_distance_m(n.0, n.1, s.0, s.1) / 2.0
}

/// One box of a goal frame or net, square to the goal line.
fn goal_part(
    name: &str,
    along: f64,
    across: f64,
    altitude: f32,
    size: [f32; 3],
    net: bool,
) -> PrimSpec {
    let (lat, lng, heading) = pitch_point(along, across);
    PrimSpec {
        name: Some(name.into()),
        shape: Some("box".into()),
        latitude: Some(lat),
        longitude: Some(lng),
        altitude: Some(altitude),
        rotation: Some([0.0, heading, 0.0]),
        scale: Some(size),
        color: Some([1.0, 1.0, 1.0]),
        surface: net.then(|| cartopolis_core::PrimSurface {
            alpha: 0.3,
            ..cartopolis_core::PrimSurface::default()
        }),
        ..PrimSpec::default()
    }
}

/// A goal on the line at `end` (+1 north, -1 south): two posts, a crossbar, and
/// a net — back, sides and roof — for the ball to stop in.
fn goal(end: f64, label: &str) -> Vec<PrimSpec> {
    let line = end * pitch_half_length();
    let bar = f64::from(GOAL_BAR_M);
    let post_across = GOAL_WIDTH_M / 2.0 + bar / 2.0;
    let outer = (GOAL_WIDTH_M + 2.0 * bar) as f32;
    let height = GOAL_HEIGHT_M + GOAL_BAR_M;
    let behind = line + end * (bar / 2.0);
    let net_mid = line + end * GOAL_DEPTH_M / 2.0;
    let depth = GOAL_DEPTH_M as f32;
    vec![
        goal_part(
            &format!("{label} goal — left post"),
            behind,
            -post_across,
            height / 2.0,
            [GOAL_BAR_M, height, GOAL_BAR_M],
            false,
        ),
        goal_part(
            &format!("{label} goal — right post"),
            behind,
            post_across,
            height / 2.0,
            [GOAL_BAR_M, height, GOAL_BAR_M],
            false,
        ),
        goal_part(
            &format!("{label} goal — crossbar"),
            behind,
            0.0,
            GOAL_HEIGHT_M + GOAL_BAR_M / 2.0,
            [outer, GOAL_BAR_M, GOAL_BAR_M],
            false,
        ),
        goal_part(
            &format!("{label} goal — back net"),
            line + end * GOAL_DEPTH_M,
            0.0,
            height / 2.0,
            [outer, height, 0.05],
            true,
        ),
        goal_part(
            &format!("{label} goal — left net"),
            net_mid,
            -post_across,
            height / 2.0,
            [0.05, height, depth],
            true,
        ),
        goal_part(
            &format!("{label} goal — right net"),
            net_mid,
            post_across,
            height / 2.0,
            [0.05, height, depth],
            true,
        ),
        goal_part(
            &format!("{label} goal — roof net"),
            net_mid,
            0.0,
            height,
            [outer, 0.05, depth],
            true,
        ),
    ]
}

/// How far outside the lines the advertising boards stand.
const BOARDS_OUT_M: f64 = 4.0;
const BOARD_HEIGHT_M: f32 = 1.0;
const BOARD_THICKNESS_M: f32 = 0.1;

/// The advertising boards round the pitch: eight, two to a side, because a prim
/// is at most 64 m long. They are what keeps the ball on the pitch — the
/// server's physics sees prims and nothing else, so the stadium around it
/// (`docs/notes/building-models.md`) is not there for a ball. Avatars are not
/// stopped by prims, so the boards are no fence for people.
fn boards() -> Vec<PrimSpec> {
    let half_len = pitch_half_length();
    let along = half_len + BOARDS_OUT_M;
    let across = PITCH_HALF_WIDTH_M + BOARDS_OUT_M;
    let mut out = Vec::new();
    for (side, s) in [("west", -1.0), ("east", 1.0)] {
        for (half, a) in [("north", 1.0), ("south", -1.0)] {
            out.push(goal_part(
                &format!("Advertising board — {side} side, {half} half"),
                a * along / 2.0,
                s * across,
                BOARD_HEIGHT_M / 2.0,
                [BOARD_THICKNESS_M, BOARD_HEIGHT_M, along as f32],
                false,
            ));
        }
    }
    for (end, a) in [("north", 1.0), ("south", -1.0)] {
        for (side, s) in [("west", -1.0), ("east", 1.0)] {
            out.push(goal_part(
                &format!("Advertising board — {end} end, {side} side"),
                a * along,
                s * across / 2.0,
                BOARD_HEIGHT_M / 2.0,
                [across as f32, BOARD_HEIGHT_M, BOARD_THICKNESS_M],
                false,
            ));
        }
    }
    out
}

/// Half the pitch's width, from the same OpenStreetMap way: 68 m. The goal-line
/// points carry the length and not the width, so the width is stated.
const PITCH_HALF_WIDTH_M: f64 = 34.0;

/// A ball and two goals on the Euroborg's pitch.
///
/// The ball is the only physical prim (`docs/notes/physical-prims.md`); the
/// goals are ordinary prims it bounces off. Its script referees: it keeps the
/// score and puts the ball back on the centre spot after a goal.
fn football() -> Pack {
    let (lat, lng, _) = pitch_point(0.0, 0.0);
    let mut prims = vec![PrimSpec {
        name: Some("Football".into()),
        shape: Some("sphere".into()),
        latitude: Some(lat),
        longitude: Some(lng),
        // Dropped onto the centre spot, so the first thing anyone sees it do is
        // bounce.
        altitude: Some(1.0),
        scale: Some([0.22, 0.22, 0.22]),
        color: Some([1.0, 1.0, 1.0]),
        physical: Some(true),
        script: Some(include_str!("../content/football.lua").into()),
        ..PrimSpec::default()
    }];
    prims.extend(goal(1.0, "North"));
    prims.extend(goal(-1.0, "South"));
    prims.extend(boards());
    Pack {
        name: "football",
        summary: "A football and two goals on the Euroborg's pitch. Run into the \
                  ball to dribble it, kick it with the kick emote; the ball keeps \
                  the score.",
        prims,
    }
}

/// Where the theremin's cabinet stands: on the open paving of the Grote Markt,
/// fifteen metres clear of the buildings on its west side. It stood at
/// (53.21952, 6.56655) until 2026-10-29, which is inside one of them — checked
/// against the aerial photograph (`--aerial-reference`) that time.
const THEREMIN_AT: (f64, f64) = (53.21858, 6.56750);
/// Height of the cabinet's centre — waist high, played standing.
const THEREMIN_CABINET_Y: f32 = 1.0;
/// The two antennas, metres from the cabinet's centre (east, up, south). The
/// same numbers are `ROD` and `LOOP` in `theremin.lua`; a test holds them
/// together, because the rod you see and the rod you play must be one rod.
const THEREMIN_ROD: [f32; 3] = [0.35, 0.45, 0.0];
const THEREMIN_LOOP: [f32; 3] = [-0.4, 0.05, 0.0];

/// A theremin you play with your hand: a cabinet on a stand, with its pitch rod
/// and volume loop. Everything that makes it an instrument is `theremin.lua`,
/// built on `sense_hands`, `on_hand` and `set_tone`.
///
/// The rod and the loop are prims of their own, placed where the cabinet's
/// script measures the hand against — they are what the player reaches for,
/// and antennas nobody can see are antennas nobody can learn.
fn theremin() -> Pack {
    let (lat, lng) = THEREMIN_AT;
    let (_, per_lng) = cartopolis_core::degree_scale_m(lat);
    // East of the cabinet by `east` metres. The cabinet is not turned, so its
    // own +x is east and an antenna offset is a longitude offset.
    let east = |m: f32| lng + f64::from(m) / per_lng;
    let part =
        |name: &str, shape: &str, x: f32, y: f32, scale: [f32; 3], color: [f32; 3]| PrimSpec {
            name: Some(name.into()),
            shape: Some(shape.into()),
            latitude: Some(lat),
            longitude: Some(east(x)),
            altitude: Some(y),
            scale: Some(scale),
            color: Some(color),
            ..PrimSpec::default()
        };
    let brass = [0.78, 0.62, 0.32];
    Pack {
        name: "theremin",
        summary: "A theremin on the Grote Markt that one person at a time picks up \
                  and plays with their hand, heard by everybody near it.",
        prims: vec![
            PrimSpec {
                script: Some(include_str!("../content/theremin.lua").into()),
                ..part(
                    "Theremin",
                    "box",
                    0.0,
                    THEREMIN_CABINET_Y,
                    [0.6, 0.25, 0.3],
                    [0.28, 0.16, 0.09],
                )
            },
            part(
                "Theremin stand",
                "cylinder",
                0.0,
                THEREMIN_CABINET_Y / 2.0 - 0.06,
                [0.06, THEREMIN_CABINET_Y - 0.12, 0.06],
                [0.1, 0.1, 0.1],
            ),
            part(
                "Theremin pitch rod",
                "cylinder",
                THEREMIN_ROD[0],
                THEREMIN_CABINET_Y + THEREMIN_ROD[1],
                [0.02, 0.6, 0.02],
                brass,
            ),
            part(
                "Theremin volume loop",
                "torus",
                THEREMIN_LOOP[0],
                THEREMIN_CABINET_Y + THEREMIN_LOOP[1],
                [0.25, 0.04, 0.25],
                brass,
            ),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pack_is_named_and_reachable() {
        for p in packs() {
            assert!(!p.name.is_empty());
            assert!(!p.prims.is_empty(), "{} installs nothing", p.name);
            assert!(pack(p.name).is_some(), "{} is not findable by name", p.name);
        }
        assert!(pack("no-such-pack").is_none());
    }

    /// Not load-bearing for reinstall any more (that matches on `prims.origin`),
    /// but a prim nobody named is one a player cannot refer to — and the beacon's
    /// name is its label.
    #[test]
    fn every_prim_in_a_pack_is_named() {
        for p in packs() {
            for prim in &p.prims {
                assert!(
                    prim.name.as_ref().is_some_and(|n| !n.trim().is_empty()),
                    "a prim in {} has no name",
                    p.name
                );
            }
        }
    }

    /// The scripts are embedded, not read from disk — a deployed server has no
    /// repository beside it. An empty one means the `include_str!` path moved and
    /// somebody pointed it at a stub.
    #[test]
    fn every_pack_prim_carries_its_script() {
        for p in packs() {
            // Not every prim: a goal post is scenery. But every pack is driven by
            // at least one, and none of those is a stub.
            assert!(
                p.prims.iter().any(|prim| prim.script.is_some()),
                "{} has no script at all",
                p.name
            );
            for prim in p.prims.iter().filter(|prim| prim.script.is_some()) {
                let source = prim.script.as_deref().unwrap_or_default();
                assert!(
                    source.len() > 200,
                    "{}'s {:?} has no script worth the name",
                    p.name,
                    prim.name
                );
            }
        }
    }

    /// The theremin's script plays the antennas the pack drew.
    #[test]
    fn the_theremin_plays_the_antennas_it_shows() {
        let source = include_str!("../content/theremin.lua");
        let fmt = |name: &str, v: [f32; 3]| {
            format!("local {name} = {{{:?}, {:?}, {}}}", v[0], v[1], v[2] as i32)
        };
        for (name, v) in [("ROD", THEREMIN_ROD), ("LOOP", THEREMIN_LOOP)] {
            let line = fmt(name, v);
            assert!(source.contains(&line), "theremin.lua lacks `{line}`");
        }
    }

    /// The referee in `football.lua` finds the goals by the same two points the
    /// pack built them on. Written twice, so held together here.
    #[test]
    fn the_football_script_knows_where_the_goals_are() {
        let source = include_str!("../content/football.lua");
        for (name, (lat, lng)) in [
            ("NORTH", EUROBORG_NORTH_GOAL),
            ("SOUTH", EUROBORG_SOUTH_GOAL),
        ] {
            let line = format!("local {name} = {{ lat = {lat}, lng = {lng} }}");
            assert!(source.contains(&line), "football.lua lacks `{line}`");
        }
    }

    /// The goals stand where the pitch is: the posts of each goal are a goal's
    /// width apart and on that end's goal line, 106 m from each other.
    #[test]
    fn the_goals_are_on_the_goal_lines() {
        let pack = football();
        let at = |name: &str| {
            let p = pack
                .prims
                .iter()
                .find(|p| p.name.as_deref() == Some(name))
                .unwrap();
            (p.latitude.unwrap(), p.longitude.unwrap())
        };
        let d =
            |a: (f64, f64), b: (f64, f64)| cartopolis_core::ground_distance_m(a.0, a.1, b.0, b.1);
        let width = d(at("North goal — left post"), at("North goal — right post"));
        assert!(
            (width - (GOAL_WIDTH_M + f64::from(GOAL_BAR_M))).abs() < 0.01,
            "{width}"
        );
        let length = d(at("North goal — crossbar"), at("South goal — crossbar"));
        assert!((length - 106.5).abs() < 1.0, "goals {length} m apart");
        let ball = pack.prims[0].clone();
        let to_north = d(
            (ball.latitude.unwrap(), ball.longitude.unwrap()),
            EUROBORG_NORTH_GOAL,
        );
        assert!((to_north - pitch_half_length()).abs() < 0.01);
        assert_eq!(ball.physical, Some(true));
    }

    /// The pair only works because they agree on a channel, and nothing but this
    /// test would notice one of them being edited.
    #[test]
    fn the_beacon_and_the_firebird_are_on_the_same_channel() {
        let channel = "local CHANNEL = 4747";
        let pack = pack("firebird").unwrap();
        for prim in &pack.prims {
            assert!(
                prim.script.as_deref().unwrap_or_default().contains(channel),
                "{:?} is not on the channel the other one is",
                prim.name
            );
        }
    }
}
