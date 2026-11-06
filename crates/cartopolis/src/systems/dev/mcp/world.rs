//! The map answering about itself: what is at a point, how far apart places
//! are, whether a build stands on the ground, what a building is and what is
//! inside it, the sky it is lit by, what leaves the stop round the corner and
//! what the municipality published.
//!
//! **Why these exist.** Building by an agent was a loop of `build`, `view`,
//! look, guess: a picture takes seconds to tens of seconds, proves only what
//! the camera happened to see, and cannot say *by how much* an object floats.
//! Everything here is read from what the map already holds — the terrain
//! field, the register, the prims' own shapes, the transit feeds — so it
//! answers in a frame, offline, with numbers.
//!
//! Requests arrive on the bridge's `world` queue. One that has to wait for
//! something to arrive — an interior to build, a feed to answer — is kept in
//! [`WorldPending`] and asked again each frame until it can answer or its time
//! is up: every handler is written to be asked more than once, and does its
//! side effects only on the first asking.

use bevy::ecs::system::SystemParam;
use bevy::math::DVec3;
use bevy::prelude::*;
use big_space::prelude::GridCell;
use futures_channel::oneshot;
use serde_json::{json, Value};
use std::sync::Arc;

use super::surroundings as around;
use super::{McpBridge, Outcome, Reply};
use crate::components::{Prim, PrimGeo, PrimShape};
use crate::grid::AnchoredCamera;
use crate::resources::{AvatarState, DevPanelState, MapAnchor, SunMode};
use crate::systems::building_facts::{use_labels, BuildingFact, BuildingFacts};
use crate::systems::geo_nav::{bearing_deg, great_circle_m, lat_lng_to_world, world_to_lat_lng};
use crate::systems::session::SessionState;
use crate::systems::terrain::TerrainField;
use cartopolis_mcp::args::AgentReply;
use cartopolis_mcp::world::{
    BuildingArgs, CheckGeometryArgs, DeparturesArgs, LookInsideArgs, MeasureArgs, NoticesArgs,
    SetSkyArgs, VehiclesArgs, WorldRequest,
};

/// How long a request may wait for what it needs to arrive, in seconds.
const LOOK_INSIDE_WAIT_S: f64 = 10.0;
const FEED_WAIT_S: f64 = 25.0;

/// What `check_geometry` searches by default, and at most, in metres.
const CHECK_RADIUS_M: f32 = 40.0;
const CHECK_RADIUS_MAX_M: f32 = 300.0;
/// Off the ground by less than this is standing on it.
const GROUND_TOLERANCE_M: f32 = 0.05;
/// Two objects whose boxes share less than this are touching, not overlapping:
/// a stack of blocks shares whole faces, and a float's rounding is not a fault.
const OVERLAP_TOLERANCE_M: f32 = 0.02;

/// What waits on a later frame: the request as it came, asked again each frame.
#[derive(Resource, Default)]
pub struct WorldPending {
    waiting: Vec<Waiting>,
}

struct Waiting {
    request: WorldRequest,
    until: f64,
    reply: oneshot::Sender<Reply>,
}

impl WorldPending {
    /// Something here keeps the frame rate up (`mcp::pace_idle`): an interior
    /// is built over frames, and at the idle rate that is seconds.
    pub fn is_working(&self) -> bool {
        !self.waiting.is_empty()
    }
}

/// The ground, the register and the prims: what `probe`, `building` and
/// `check_geometry` read.
#[derive(SystemParam)]
pub struct Ground<'w, 's> {
    terrain: Res<'w, TerrainField>,
    places: Res<'w, crate::systems::world_places::WorldPlaces>,
    grass: Res<'w, crate::systems::grass::GrassAreas>,
    tunnels: Res<'w, crate::systems::tunnels::Tunnels>,
    facts: Res<'w, BuildingFacts>,
    lod: Res<'w, crate::systems::lod22::Lod22Buildings>,
    floors: Res<'w, crate::systems::interiors::InteriorFloors>,
    prims: Query<
        'w,
        's,
        (
            &'static Prim,
            Option<&'static PrimGeo>,
            Option<&'static GridCell>,
            &'static Transform,
        ),
    >,
}

/// What these tools change on the person's screen: the selected building, the
/// cutaway, the sky.
#[derive(SystemParam)]
pub struct Screen<'w> {
    selection: ResMut<'w, crate::systems::building_select::BuildingSelection>,
    cutaway: ResMut<'w, crate::systems::cutaway::Cutaway>,
    dev: ResMut<'w, DevPanelState>,
    weather: ResMut<'w, crate::systems::weather::WeatherState>,
    clock: Res<'w, crate::systems::transit_live::WorldClock>,
}

/// The live feeds: the transit layer and its boards, the vehicles, the notices.
#[derive(SystemParam)]
pub struct Feeds<'w> {
    layers: ResMut<'w, crate::systems::data_layers::DataLayers>,
    live: Res<'w, crate::systems::transit_live::TransitLive>,
    vehicles: ResMut<'w, crate::systems::transit_vehicles::TransitVehicles>,
    plans: ResMut<'w, crate::systems::plans::Plans>,
}

/// Everything `serve` reads and writes.
#[derive(SystemParam)]
pub struct WorldWorld<'w, 's> {
    anchor: Res<'w, MapAnchor>,
    session: Res<'w, SessionState>,
    avatar: Res<'w, AvatarState>,
    camera: AnchoredCamera<'w, 's>,
    ground: Ground<'w, 's>,
    screen: Screen<'w>,
    feeds: Feeds<'w>,
}

/// Answer every map question that arrived this frame, and every one still
/// waiting whose answer has arrived or whose time is up.
pub fn serve(
    mut bridge: ResMut<McpBridge>,
    mut pending: ResMut<WorldPending>,
    mut w: WorldWorld,
    real: Res<Time<Real>>,
) {
    let now = real.elapsed_secs_f64();
    for (request, reply) in std::mem::take(&mut bridge.world) {
        let label = request.label();
        match answer(&request, &mut w, true, false) {
            Some(answer) => finish(&mut bridge, label, reply, answer, now),
            None => {
                bridge.note(label.into(), "working…".into(), Outcome::Working, now);
                let until = now
                    + match request {
                        WorldRequest::LookInside(_) => LOOK_INSIDE_WAIT_S,
                        _ => FEED_WAIT_S,
                    };
                pending.waiting.push(Waiting {
                    request,
                    until,
                    reply,
                });
            }
        }
    }
    for wait in std::mem::take(&mut pending.waiting) {
        if wait.reply.is_canceled() {
            continue;
        }
        let last = now >= wait.until;
        match answer(&wait.request, &mut w, false, last) {
            Some(answer) => finish(&mut bridge, wait.request.label(), wait.reply, answer, now),
            None => pending.waiting.push(wait),
        }
    }
}

fn finish(
    bridge: &mut McpBridge,
    label: &str,
    reply: oneshot::Sender<Reply>,
    answer: AgentReply,
    now: f64,
) {
    let outcome = if answer.error {
        Outcome::Failed
    } else {
        Outcome::Done
    };
    let first = answer.text.lines().next().unwrap_or_default().to_string();
    bridge.note(label.into(), first, outcome, now);
    let _ = reply.send(Reply::Agent(Box::new(answer)));
}

/// One request's answer, or `None` while it waits. `first` is the asking that
/// may change things; `last` must answer, with whatever there is.
fn answer(
    request: &WorldRequest,
    w: &mut WorldWorld,
    first: bool,
    last: bool,
) -> Option<AgentReply> {
    let Some(anchor) = w.anchor.geo else {
        return Some(AgentReply::refuse(
            "The map has not loaded anywhere yet: `fly_to` or `view` a place first.",
        ));
    };
    match request {
        WorldRequest::Measure(args) => Some(measure(args)),
        WorldRequest::SetSky(args) => Some(set_sky(w, anchor, args)),
        WorldRequest::Probe(args) => Some(match w.point(anchor, args.lat, args.lng) {
            Ok(at) => probe(w, anchor, at),
            Err(why) => AgentReply::refuse(why),
        }),
        WorldRequest::CheckGeometry(args) => Some(check_geometry(w, anchor, args)),
        WorldRequest::Building(args) => Some(building(w, anchor, args)),
        WorldRequest::LookInside(args) => look_inside(w, anchor, args, first, last),
        WorldRequest::Departures(args) => departures(w, anchor, args, first, last),
        WorldRequest::Vehicles(args) => vehicles(w, anchor, args, first, last),
        WorldRequest::Notices(args) => notices(w, anchor, args, first, last),
    }
}

impl WorldWorld<'_, '_> {
    /// The point a call names, or where it is by default: the avatar online,
    /// the middle of the view offline.
    fn point(
        &self,
        anchor: (f64, f64, u32),
        lat: Option<f64>,
        lng: Option<f64>,
    ) -> Result<(f64, f64), String> {
        match (lat, lng) {
            (Some(lat), Some(lng)) => {
                if (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lng) {
                    Ok((lat, lng))
                } else {
                    Err(format!("lat/lng out of range: {lat:.5}, {lng:.5}"))
                }
            }
            (None, None) => {
                let xz = if self.session.is_online() {
                    Vec2::new(self.avatar.position.x, self.avatar.position.z)
                } else {
                    self.camera
                        .stream_xz()
                        .ok_or_else(|| "There is no camera to start from.".to_string())?
                };
                Ok(world_to_lat_lng(anchor, f64::from(xz.x), f64::from(xz.y)))
            }
            _ => Err("Give both `lat` and `lng`, or neither.".into()),
        }
    }
}

/// Anchored world XZ of a lat/lng.
fn xz(anchor: (f64, f64, u32), at: (f64, f64)) -> Vec2 {
    let (x, z) = lat_lng_to_world(anchor, at.0, at.1);
    Vec2::new(x as f32, z as f32)
}

fn metres(m: f64) -> String {
    if m >= 1000.0 {
        format!("{:.2} km", m / 1000.0)
    } else if m >= 10.0 {
        format!("{m:.0} m")
    } else {
        format!("{m:.2} m")
    }
}

fn compass(deg: f64) -> &'static str {
    around::compass(deg as f32)
}

// ---------------------------------------------------------------------------
// probe and measure
// ---------------------------------------------------------------------------

fn probe(w: &WorldWorld, anchor: (f64, f64, u32), at: (f64, f64)) -> AgentReply {
    let g = &w.ground;
    let p = xz(anchor, at);
    let mut lines = vec![format!("At {:.6}, {:.6}:", at.0, at.1)];
    let mut data = json!({"lat": at.0, "lng": at.1});

    // Heights are metres over the ground at the map's anchor, as everything
    // placed is: an object's `altitude_m` is measured from the ground under it,
    // so what matters to a builder is the difference between two points.
    if g.terrain.is_ready() {
        let ground = g.terrain.ground_at(p.x, p.y);
        let nap = ground + g.terrain.datum_m;
        lines.push(format!(
            "Ground {ground:+.2} m against the map's anchor ({nap:.2} m above NAP)."
        ));
        data["ground_m"] = json!(ground);
        data["ground_nap_m"] = json!(nap);
        if let Some(water) = g.terrain.water_surface_at(p.x, p.y) {
            lines.push(format!(
                "Water: its surface stands at {water:+.2} m ({:.2} m above NAP).",
                water + g.terrain.datum_m
            ));
            data["water_m"] = json!(water);
        }
    } else {
        lines.push(
            "The ground's heights have not loaded here: anything standing here is at 0.".into(),
        );
        data["ground_m"] = Value::Null;
    }
    if let Some(deck) = crate::systems::map_geometry::walk_deck_top(p.x, p.y) {
        lines.push(format!("A bridge deck, its top at {deck:+.2} m."));
        data["deck_m"] = json!(deck);
    }
    if let Some(road) = g.tunnels.road_under(p) {
        lines.push(format!("A tunnel underneath, its road at {road:+.2} m."));
        data["tunnel_road_m"] = json!(road);
    }

    let surface = match g.places.paving_at(p) {
        Some((kind, material)) => {
            let kind = format!("{kind:?}").to_lowercase();
            match material {
                Some(m) => format!("{kind}, {}", format!("{m:?}").to_lowercase()),
                None => kind,
            }
        }
        None if g.grass.grass_at(p) => "grass".to_string(),
        None => String::new(),
    };
    if !surface.is_empty() {
        lines.push(format!("Surface: {surface}."));
        data["surface"] = json!(surface);
    }

    match building_at(&g.facts, at) {
        Some((id, fact)) => {
            let address = addresses_of(&g.facts, id)
                .first()
                .and_then(|a| a.label())
                .unwrap_or_else(|| "no address".into());
            let uses = use_labels(fact.uses).join(", ");
            lines.push(format!(
                "Inside a building: {address}{}{} (`building` has the rest).",
                if uses.is_empty() { "" } else { ", " },
                uses
            ));
            data["building"] = json!({"id": id, "address": address});
        }
        None => {
            if let Some(a) = g.facts.nearest_address(at.0, at.1, 40.0) {
                if let Some(label) = a.label() {
                    lines.push(format!("Outside; the nearest address is {label}."));
                    data["nearest_address"] = json!(label);
                }
            }
        }
    }

    let objects: Vec<Value> = g
        .prims
        .iter()
        .filter_map(|(prim, geo, cell, tf)| {
            let c = prim_centre(geo, cell, tf, anchor);
            let d = c.distance(p);
            let reach = tf.scale.x.max(tf.scale.z) * 0.5 + 1.0;
            (d <= reach).then(|| json!({"id": prim.id, "name": prim.name, "distance_m": d}))
        })
        .collect();
    if !objects.is_empty() {
        lines.push(format!(
            "Objects here: {}.",
            objects
                .iter()
                .map(|o| format!("#{} {}", o["id"], o["name"].as_str().unwrap_or("")))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        data["objects"] = Value::Array(objects);
    }
    AgentReply::ok(lines.join("\n"), data)
}

fn measure(args: &MeasureArgs) -> AgentReply {
    let pts: Vec<(f64, f64)> = args.points.iter().map(|p| (p[0], p[1])).collect();
    if pts.len() < 2 {
        return AgentReply::refuse("Give at least two points.");
    }
    if let Some(p) = pts
        .iter()
        .find(|p| !(-90.0..=90.0).contains(&p.0) || !(-180.0..=180.0).contains(&p.1))
    {
        return AgentReply::refuse(format!("Out of range: {:.5}, {:.5}", p.0, p.1));
    }
    let legs: Vec<(f64, f64)> = pts
        .windows(2)
        .map(|w| (great_circle_m(w[0], w[1]), bearing_deg(w[0], w[1])))
        .collect();
    let total: f64 = legs.iter().map(|l| l.0).sum();
    let mut data = json!({
        "total_m": total,
        "legs": legs.iter().map(|(m, b)| json!({"m": m, "bearing_deg": b})).collect::<Vec<_>>(),
    });
    let mut text = if legs.len() == 1 {
        format!(
            "{}, bearing {:.0}° ({}).",
            metres(legs[0].0),
            legs[0].1,
            compass(legs[0].1)
        )
    } else {
        let each: Vec<String> = legs
            .iter()
            .enumerate()
            .map(|(i, (m, b))| format!("{}→{}: {} {}", i + 1, i + 2, metres(*m), compass(*b)))
            .collect();
        format!("{} in all. {}.", metres(total), each.join("; "))
    };
    if pts.len() >= 3 {
        let area = area_m2(&pts);
        let closing = great_circle_m(pts[pts.len() - 1], pts[0]);
        text.push_str(&format!(
            " Closed, the shape measures {:.0} m² and {} round.",
            area,
            metres(total + closing)
        ));
        data["area_m2"] = json!(area);
        data["perimeter_m"] = json!(total + closing);
    }
    AgentReply::ok(text, data)
}

/// The area a ring of lat/lngs encloses, on a flat projection about its first
/// point — within a fraction of a percent at the size of a city block.
fn area_m2(pts: &[(f64, f64)]) -> f64 {
    let o = pts[0];
    let flat: Vec<Vec2> = pts.iter().map(|&p| around::geo_step(o, p)).collect();
    let mut twice = 0.0_f64;
    for i in 0..flat.len() {
        let (a, b) = (flat[i], flat[(i + 1) % flat.len()]);
        twice += f64::from(a.x) * f64::from(b.y) - f64::from(b.x) * f64::from(a.y);
    }
    (twice * 0.5).abs()
}

// ---------------------------------------------------------------------------
// check_geometry
// ---------------------------------------------------------------------------

/// A prim's box as drawn: centre, axes and half sizes in the anchored world
/// frame, heights over the anchor's ground (the terrain field's frame).
#[derive(Clone, Copy)]
struct Boxed {
    centre: Vec3,
    axes: [Vec3; 3],
    half: Vec3,
}

impl Boxed {
    fn corners(&self) -> [Vec3; 8] {
        let mut out = [Vec3::ZERO; 8];
        for (i, c) in out.iter_mut().enumerate() {
            let s = |bit: usize| if i & bit == 0 { -1.0 } else { 1.0 };
            *c = self.centre
                + self.axes[0] * self.half.x * s(1)
                + self.axes[1] * self.half.y * s(2)
                + self.axes[2] * self.half.z * s(4);
        }
        out
    }

    fn radius_on(&self, axis: Vec3) -> f32 {
        self.half.x * self.axes[0].dot(axis).abs()
            + self.half.y * self.axes[1].dot(axis).abs()
            + self.half.z * self.axes[2].dot(axis).abs()
    }

    fn lowest(&self) -> f32 {
        self.centre.y - self.radius_on(Vec3::Y)
    }

    fn highest(&self) -> f32 {
        self.centre.y + self.radius_on(Vec3::Y)
    }
}

/// How deep two boxes overlap along the axis where they overlap least, or
/// `None` when an axis separates them: the separating-axis test, over the
/// fifteen axes two boxes can be told apart on.
fn overlap_depth(a: &Boxed, b: &Boxed) -> Option<f32> {
    let mut axes: Vec<Vec3> = Vec::with_capacity(15);
    axes.extend(a.axes);
    axes.extend(b.axes);
    for i in 0..3 {
        for j in 0..3 {
            let c = a.axes[i].cross(b.axes[j]);
            if c.length_squared() > 1e-6 {
                axes.push(c.normalize());
            }
        }
    }
    let d = b.centre - a.centre;
    let mut least = f32::MAX;
    for axis in axes {
        let gap = a.radius_on(axis) + b.radius_on(axis) - d.dot(axis).abs();
        if gap <= 0.0 {
            return None;
        }
        least = least.min(gap);
    }
    Some(least)
}

/// A prim's anchored world XZ, from its geo anchor where it has one — the
/// `Transform` is cell-local (`prim-scripting.md`).
fn prim_centre(
    geo: Option<&PrimGeo>,
    cell: Option<&GridCell>,
    tf: &Transform,
    anchor: (f64, f64, u32),
) -> Vec2 {
    match geo {
        Some(g) => xz(anchor, (g.latitude, g.longitude)),
        None => {
            let p = crate::systems::prim_geo::world_position(cell, tf);
            Vec2::new(p.x as f32, p.z as f32)
        }
    }
}

/// The box a prim is drawn in. Its `Transform` is in the sim's flat frame
/// while `Update` runs (`prim_geo::strip_prim_lift`), so its `y` is the height
/// over the ground; the ground under its anchor is added back here exactly as
/// `lift_prims_onto_terrain` adds it for drawing.
fn prim_box(prim: &Prim, cell: Option<&GridCell>, tf: &Transform, terrain: &TerrainField) -> Boxed {
    let flat: DVec3 = crate::systems::prim_geo::world_position(cell, tf);
    let lift = terrain.ground_at(flat.x as f32, flat.z as f32);
    // Unit meshes centred on the origin (`rendering.rs`), except the torus,
    // which Bevy builds 3 × 1 × 3.
    let unit = match prim.shape {
        PrimShape::Torus if prim.model.is_none() => Vec3::new(1.5, 0.5, 1.5),
        _ => Vec3::splat(0.5),
    };
    let r = tf.rotation;
    Boxed {
        centre: Vec3::new(flat.x as f32, flat.y as f32 + lift, flat.z as f32),
        axes: [r * Vec3::X, r * Vec3::Y, r * Vec3::Z],
        half: (tf.scale.abs() * unit).max(Vec3::splat(0.001)),
    }
}

fn check_geometry(w: &WorldWorld, anchor: (f64, f64, u32), args: &CheckGeometryArgs) -> AgentReply {
    let g = &w.ground;
    let tolerance = args
        .tolerance_m
        .unwrap_or(GROUND_TOLERANCE_M)
        .clamp(0.0, 2.0);
    let all: Vec<(&Prim, Boxed, Vec2)> = g
        .prims
        .iter()
        .map(|(prim, geo, cell, tf)| {
            (
                prim,
                prim_box(prim, cell, tf, &g.terrain),
                prim_centre(geo, cell, tf, anchor),
            )
        })
        .collect();
    let checked: Vec<usize> = match &args.ids {
        Some(ids) => {
            let missing: Vec<String> = ids
                .iter()
                .filter(|id| !all.iter().any(|(p, _, _)| p.id == **id))
                .map(|id| format!("#{id}"))
                .collect();
            if !missing.is_empty() {
                return AgentReply::refuse(format!(
                    "Not on the map here: {}. An object far from the view is not loaded; \
                     `view` it first.",
                    missing.join(", ")
                ));
            }
            (0..all.len())
                .filter(|&i| ids.contains(&all[i].0.id))
                .collect()
        }
        None => {
            let at = match w.point(anchor, args.lat, args.lng) {
                Ok(at) => xz(anchor, at),
                Err(why) => return AgentReply::refuse(why),
            };
            let r = args
                .radius_m
                .unwrap_or(CHECK_RADIUS_M)
                .clamp(1.0, CHECK_RADIUS_MAX_M);
            (0..all.len())
                .filter(|&i| all[i].2.distance(at) <= r)
                .collect()
        }
    };
    if checked.is_empty() {
        return AgentReply::ok(
            "No objects to check there.",
            json!({"checked": 0, "problems": []}),
        );
    }
    if !g.terrain.is_ready() {
        return AgentReply::refuse(
            "The ground's heights have not loaded yet, so nothing can be measured against \
             them. Ask again in a few seconds.",
        );
    }

    let mut problems: Vec<Value> = Vec::new();
    let mut lines: Vec<String> = Vec::new();
    for &i in &checked {
        let (prim, b, at) = &all[i];
        let name = format!("#{} {}", prim.id, prim.name);
        // Ground under every corner of the box, not only its middle: on a
        // slope one end of a long bench can float while the other is buried.
        let gaps: Vec<f32> = b
            .corners()
            .iter()
            .map(|c| c.y - g.terrain.ground_at(c.x, c.z))
            .collect();
        let lowest_gap = gaps.iter().copied().fold(f32::MAX, f32::min);
        let top_gap = b.highest() - g.terrain.ground_at(b.centre.x, b.centre.z);

        // What it rests on, if not the ground: another object's top, or a
        // roof. Something standing on a table floats over the ground, and is
        // not wrong to.
        let resting_on = all.iter().find_map(|(other, ob, _)| {
            if other.id == prim.id {
                return None;
            }
            let mut lowered = *b;
            lowered.centre.y -= tolerance + OVERLAP_TOLERANCE_M;
            (overlap_depth(&lowered, ob).is_some() && b.lowest() >= ob.highest() - 0.1)
                .then(|| format!("#{} {}", other.id, other.name))
        });
        let building = building_at(&g.facts, geo_of(anchor, *at));
        let roof = building.and_then(|(id, _)| {
            let prism = prism_of(g, anchor, id)?;
            ((b.lowest() - (prism.base + prism.top)).abs() <= 0.3).then(|| id.to_string())
        });

        if top_gap < -tolerance {
            lines.push(format!(
                "{name} is buried: its top is {:.2} m under the ground.",
                -top_gap
            ));
            problems.push(json!({"id": prim.id, "problem": "buried", "by_m": -top_gap}));
        } else if lowest_gap > tolerance && resting_on.is_none() && roof.is_none() {
            lines.push(format!("{name} floats {lowest_gap:.2} m above the ground."));
            problems.push(json!({"id": prim.id, "problem": "floats", "by_m": lowest_gap}));
        } else if lowest_gap < -tolerance && resting_on.is_none() {
            // Into the ground but not under it: a post set in a footing is
            // meant to; a table leg sunk 30 cm is not. Said, and left to judge.
            lines.push(format!(
                "{name} reaches {:.2} m into the ground (its lowest corner).",
                -lowest_gap
            ));
            problems.push(json!({"id": prim.id, "problem": "sunk", "by_m": -lowest_gap}));
        }

        for &j in &checked {
            if j <= i {
                continue;
            }
            let (other, ob, _) = &all[j];
            if let Some(depth) = overlap_depth(b, ob) {
                if depth > OVERLAP_TOLERANCE_M {
                    lines.push(format!(
                        "{name} and #{} {} overlap by {depth:.2} m.",
                        other.id, other.name
                    ));
                    problems.push(json!({
                        "id": prim.id, "problem": "overlaps", "other": other.id, "by_m": depth,
                    }));
                }
            }
        }

        if roof.is_none() {
            if let Some(id) = into_building(g, anchor, b) {
                let address = addresses_of(&g.facts, &id)
                    .first()
                    .and_then(|a| a.label())
                    .unwrap_or_else(|| "a building".into());
                lines.push(format!("{name} reaches into {address}."));
                problems.push(json!({"id": prim.id, "problem": "in_building", "building": id}));
            }
        }
    }
    let text = if lines.is_empty() {
        format!(
            "{} object{} checked: each stands on the ground or on something, and none \
             overlap.",
            checked.len(),
            if checked.len() == 1 { "" } else { "s" }
        )
    } else {
        format!(
            "{} object{} checked; {} problem{}:\n{}",
            checked.len(),
            if checked.len() == 1 { "" } else { "s" },
            lines.len(),
            if lines.len() == 1 { "" } else { "s" },
            lines.join("\n")
        )
    };
    AgentReply::ok(
        text,
        json!({"checked": checked.len(), "problems": problems}),
    )
}

fn geo_of(anchor: (f64, f64, u32), p: Vec2) -> (f64, f64) {
    world_to_lat_lng(anchor, f64::from(p.x), f64::from(p.y))
}

fn prism_of(
    g: &Ground,
    anchor: (f64, f64, u32),
    id: &str,
) -> Option<crate::systems::building_select::Prism> {
    crate::systems::building_select::prism(id, &g.facts, &g.lod, &g.terrain, anchor, |m| m.top_m)
}

/// The building a box reaches into, if any: a corner or its middle inside a
/// building's outline, between its ground and its roof.
fn into_building(g: &Ground, anchor: (f64, f64, u32), b: &Boxed) -> Option<String> {
    let mid = geo_of(anchor, Vec2::new(b.centre.x, b.centre.z));
    let reach = f64::from(b.half.max_element()) * 2.0 + 60.0;
    for (id, _) in g.facts.near(mid.0, mid.1, reach.max(120.0)) {
        let Some(prism) = prism_of(g, anchor, id) else {
            continue;
        };
        let (low, high) = (prism.base + 0.05, prism.base + prism.top - 0.05);
        let points = b.corners().into_iter().chain(std::iter::once(b.centre));
        let inside = points.into_iter().any(|c| {
            c.y > low
                && c.y < high
                && crate::systems::building_select::point_in_ring(&prism.ring, Vec2::new(c.x, c.z))
        });
        if inside {
            return Some(id.to_string());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Buildings
// ---------------------------------------------------------------------------

/// The building whose outline holds a point: the smallest, since a register
/// outline sometimes holds another building whole (`InteriorFloors::building_at`).
fn building_at(facts: &BuildingFacts, at: (f64, f64)) -> Option<(&str, &BuildingFact)> {
    facts
        .near(at.0, at.1, 300.0)
        .into_iter()
        .filter(|(_, b)| around::inside_ring(at, &b.footprint))
        .min_by(|a, b| ring_area(&a.1.footprint).total_cmp(&ring_area(&b.1.footprint)))
}

fn ring_area(ring: &[(f64, f64)]) -> f64 {
    if ring.len() < 3 {
        return 0.0;
    }
    area_m2(ring)
}

fn addresses_of(
    facts: &BuildingFacts,
    id: &str,
) -> Vec<crate::systems::building_facts::AddressFact> {
    facts
        .addresses()
        .filter(|a| a.pand.as_deref() == Some(id))
        .cloned()
        .collect()
}

/// The building a call means: at its point, or the one selected on the screen.
fn which_building(
    w: &WorldWorld,
    anchor: (f64, f64, u32),
    lat: Option<f64>,
    lng: Option<f64>,
) -> Result<Arc<str>, String> {
    if lat.is_none() && lng.is_none() {
        if let Some(id) = w.screen.selection.selected.clone() {
            return Ok(id);
        }
    }
    let at = w.point(anchor, lat, lng)?;
    if w.ground.facts.len() == 0 {
        return Err(
            "No building register is loaded here: it covers the Netherlands, and loads round \
             the camera. `fly_to` the place first."
                .into(),
        );
    }
    building_at(&w.ground.facts, at)
        .map(|(id, _)| Arc::from(id))
        .ok_or_else(|| {
            format!(
                "No building at {:.6}, {:.6}{}.",
                at.0,
                at.1,
                w.ground
                    .facts
                    .nearest_address(at.0, at.1, 40.0)
                    .and_then(|a| a.label())
                    .map(|l| format!("; the nearest address is {l}"))
                    .unwrap_or_default()
            )
        })
}

fn building(w: &mut WorldWorld, anchor: (f64, f64, u32), args: &BuildingArgs) -> AgentReply {
    let id = match which_building(w, anchor, args.lat, args.lng) {
        Ok(id) => id,
        Err(why) => return AgentReply::refuse(why),
    };
    let g = &w.ground;
    let Some(fact) = g.facts.building(&id) else {
        return AgentReply::refuse("That building's facts have streamed out; ask again.");
    };
    let addresses = addresses_of(&g.facts, &id);
    let street = crate::systems::building_card::street_line(&addresses);
    let uses = use_labels(fact.uses);
    let shell = g.lod.shell_measure(&id);
    let storeys = g.floors.storey_names(&id);
    let floors = storeys.as_ref().map(Vec::len).or_else(|| {
        shell.map(|s| {
            (s.datum_m / cartopolis_geo::building_design::STOREY_M)
                .round()
                .max(1.0) as usize
        })
    });

    let mut lines = vec![format!(
        "{}{}",
        street
            .clone()
            .unwrap_or_else(|| "A building with no address".into()),
        if uses.is_empty() {
            String::new()
        } else {
            format!(" — {}", uses.join(" · "))
        }
    )];
    if let Some(year) = fact.year {
        lines.push(format!("Built {year}."));
    }
    if let Some(status) = fact.status.label() {
        lines.push(format!("Status: {status}."));
    }
    if let Some(s) = shell {
        lines.push(format!(
            "Eaves {:.1} m, ridge {:.1} m; roof {:.0} m², volume {:.0} m³.",
            s.datum_m, s.top_m, s.roof_area_m2, s.volume_m3
        ));
    }
    if let Some(n) = floors {
        lines.push(if storeys.is_some() {
            format!("{n} {} inside.", if n == 1 { "storey" } else { "storeys" })
        } else {
            format!("About {n} storeys, from its height.")
        });
    }
    match (fact.area_m2, fact.area_max_m2) {
        (Some(a), Some(b)) if b > a => lines.push(format!("Floor area {a}–{b} m².")),
        (Some(a), _) => lines.push(format!("Floor area {a} m².")),
        _ => {}
    }
    if fact.units > 1 {
        lines.push(format!("{} addressed units.", fact.units));
    }
    if let Some(m) = fact.monument {
        lines.push(format!(
            "Listed monument {m}{}.",
            fact.monument_category
                .as_deref()
                .map(|c| format!(" ({c})"))
                .unwrap_or_default()
        ));
    }
    let labels: Vec<String> = addresses.iter().filter_map(|a| a.label()).collect();
    if labels.len() > 1 {
        let shown: Vec<&str> = labels.iter().take(8).map(String::as_str).collect();
        lines.push(format!(
            "Addresses: {}{}.",
            shown.join(", "),
            if labels.len() > 8 {
                format!(" and {} more", labels.len() - 8)
            } else {
                String::new()
            }
        ));
    }
    let designed =
        g.floors.is_designed(&id) || crate::systems::building_designs::get(&id).is_some();
    if designed {
        lines.push("Its inside was drawn by its owner.".into());
    }

    let data = json!({
        "id": &*id,
        "street": street,
        "addresses": labels,
        "uses": uses,
        "year": fact.year,
        "status": fact.status.label(),
        "eaves_m": shell.map(|s| s.datum_m),
        "ridge_m": shell.map(|s| s.top_m),
        "roof_area_m2": shell.map(|s| s.roof_area_m2),
        "volume_m3": shell.map(|s| s.volume_m3),
        "storeys": storeys,
        "floors": floors,
        "area_m2": fact.area_m2,
        "units": fact.units,
        "monument": fact.monument,
        "district": fact.district.as_deref(),
        "designed": designed,
        "outline": fact.footprint.iter().map(|p| [p.0, p.1]).collect::<Vec<_>>(),
        "register": crate::systems::building_card::bag_viewer_url(&id),
    });
    if args.select.unwrap_or(false) {
        w.screen.selection.selected = Some(id.clone());
        lines.push("Selected on the screen.".into());
    }
    AgentReply::ok(lines.join("\n"), data)
}

fn look_inside(
    w: &mut WorldWorld,
    anchor: (f64, f64, u32),
    args: &LookInsideArgs,
    first: bool,
    last: bool,
) -> Option<AgentReply> {
    if args.stop.unwrap_or(false) {
        w.screen.cutaway.stop_looking();
        return Some(AgentReply::ok(
            "The building is closed again.",
            json!({"looking": false}),
        ));
    }
    if first {
        let id = match which_building(w, anchor, args.lat, args.lng) {
            Ok(id) => id,
            Err(why) => return Some(AgentReply::refuse(why)),
        };
        // Both, or `resolve_cutaway` ends the look on the next frame: a cut
        // is of the selected building.
        w.screen.selection.selected = Some(id.clone());
        w.screen.cutaway.look_inside(id);
        return None;
    }
    let cut = &w.screen.cutaway;
    let Some(id) = cut.looking.clone() else {
        return Some(AgentReply::refuse(
            "The look inside was closed before it opened (the selection moved).",
        ));
    };
    let ready = cut.building.as_deref() == Some(&*id) && !cut.storeys.is_empty();
    if !ready {
        return last.then(|| {
            w.screen.cutaway.stop_looking();
            AgentReply::refuse(
                "This building has no inside to show: it needs an entrance in the register \
                 or a drawn design.",
            )
        });
    }
    let storeys = cut.storeys.clone();
    let want = args.floor.unwrap_or(0) as usize;
    if want >= storeys.len() {
        return Some(AgentReply::refuse(format!(
            "It has {} storey(s) (0–{}): {}.",
            storeys.len(),
            storeys.len() - 1,
            storeys.join(", ")
        )));
    }
    // Picked after the cut took this building: `resolve_cutaway` resets the
    // pick whenever the cut building changes.
    if w.screen.cutaway.picked != Some(want) {
        w.screen.cutaway.picked = Some(want);
    }
    Some(AgentReply::ok(
        format!(
            "Showing {} of {}: {}. `view_here` photographs it; `stop` closes it.",
            storeys[want],
            storeys.len(),
            storeys.join(", ")
        ),
        json!({"looking": true, "building": &*id, "floor": want, "storeys": storeys}),
    ))
}

// ---------------------------------------------------------------------------
// The sky
// ---------------------------------------------------------------------------

/// Minutes past midnight in `"HH:MM"`.
fn parse_clock(s: &str) -> Option<f32> {
    let (h, m) = s.trim().split_once(':')?;
    let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then(|| (h * 60 + m) as f32)
}

/// The day of the year (1-based) of `"YYYY-MM-DD"` or `"MM-DD"`.
fn parse_day(s: &str) -> Option<f32> {
    let parts: Vec<&str> = s.trim().split('-').collect();
    let (year, month, day) = match parts.as_slice() {
        [y, m, d] => (
            y.parse::<i32>().ok(),
            m.parse::<u32>().ok()?,
            d.parse::<u32>().ok()?,
        ),
        [m, d] => (None, m.parse::<u32>().ok()?, d.parse::<u32>().ok()?),
        _ => return None,
    };
    const DAYS: [u32; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if !(1..=12).contains(&month)
        || day == 0
        || day > DAYS[month as usize - 1] + u32::from(month == 2)
    {
        return None;
    }
    let leap = year.is_some_and(|y| (y % 4 == 0 && y % 100 != 0) || y % 400 == 0);
    let before: u32 = DAYS[..month as usize - 1].iter().sum();
    Some((before + day + u32::from(leap && month > 2)) as f32)
}

fn set_sky(w: &mut WorldWorld, anchor: (f64, f64, u32), args: &SetSkyArgs) -> AgentReply {
    let s = &mut w.screen;
    if args.live.unwrap_or(false) {
        if s.dev.sun_mode != SunMode::Auto {
            s.dev.sun_mode = SunMode::Auto;
        }
        if s.dev.sun_day_of_year.is_some() {
            s.dev.sun_day_of_year = None;
        }
        if !s.weather.live {
            s.weather.live = true;
        }
        return AgentReply::ok(
            "Back to the real time of day and the live weather.",
            json!({"live": true}),
        );
    }
    let mut said = Vec::new();
    // Checked whole before anything is written: half a change is worse than
    // none.
    let time = match args.time.as_deref().map(|t| (t, parse_clock(t))) {
        Some((t, None)) => {
            return AgentReply::refuse(format!("time wants \"HH:MM\", not {t:?}"));
        }
        Some((_, Some(m))) => Some(m),
        None => None,
    };
    let day = match args.date.as_deref().map(|d| (d, parse_day(d))) {
        Some((d, None)) => {
            return AgentReply::refuse(format!(
                "date wants \"YYYY-MM-DD\" or \"MM-DD\", not {d:?}"
            ));
        }
        Some((_, Some(day))) => Some(day),
        None => None,
    };

    if let Some(minutes) = time {
        // The sky runs on local *solar* time (`utils::sun`): UTC plus the
        // longitude. A clock time is the place's own, so take its zone off to
        // get UTC first; with no zone learned yet, the clock time is taken as
        // solar time, and said.
        let solar = match s.clock.local_offset_secs {
            Some(off) => {
                let utc = minutes / 60.0 - off as f32 / 3600.0;
                (utc + anchor.1 as f32 / 15.0).rem_euclid(24.0)
            }
            None => {
                said.push(
                    "No time zone is known here yet, so that is read as sun time.".to_string(),
                );
                minutes / 60.0
            }
        };
        if s.dev.sun_mode != SunMode::Manual {
            s.dev.sun_mode = SunMode::Manual;
        }
        if (s.dev.time_of_day - solar).abs() > 1e-3 {
            s.dev.time_of_day = solar;
        }
        said.insert(
            0,
            format!("Lit for {}.", args.time.as_deref().unwrap_or_default()),
        );
    }
    if let Some(day) = day {
        if s.dev.sun_day_of_year != Some(day) {
            s.dev.sun_day_of_year = Some(day);
        }
        said.push(format!(
            "The sun stands as on {}.",
            args.date.as_deref().unwrap_or_default()
        ));
    }
    let weather = [args.clouds, args.fog, args.rain];
    if weather.iter().any(Option::is_some) {
        // Live weather rewrites all three from its last sample every frame
        // (`weather::poll_weather`); it has to stop for these to hold.
        if s.weather.live {
            s.weather.live = false;
        }
        // Read through `Res`, written only where it differs: every write to
        // `DevPanelState` re-runs the lighting.
        let was = [s.dev.cloud_cover, s.dev.fog_density, s.dev.precipitation];
        let mut next = was;
        for (i, (value, word)) in [
            (args.clouds, "clouds"),
            (args.fog, "fog"),
            (args.rain, "rain"),
        ]
        .into_iter()
        .enumerate()
        {
            if let Some(v) = value {
                next[i] = v.clamp(0.0, 1.0);
                said.push(format!("{word} {:.2}", next[i]));
            }
        }
        if next.iter().zip(was).any(|(a, b)| (a - b).abs() > 1e-4) {
            let dev = &mut *s.dev;
            [dev.cloud_cover, dev.fog_density, dev.precipitation] = next;
        }
    }
    if said.is_empty() {
        return AgentReply::ok(
            format!(
                "Nothing changed. The sky is {} at sun time {:.2} h; clouds {:.2}, fog {:.2}, \
                 rain {:.2}; weather {}.",
                format!("{:?}", s.dev.sun_mode).to_lowercase(),
                s.dev.time_of_day,
                s.dev.cloud_cover,
                s.dev.fog_density,
                s.dev.precipitation,
                if s.weather.live { "live" } else { "held" },
            ),
            json!({"changed": false}),
        );
    }
    AgentReply::ok(
        format!(
            "{} It holds until changed; `live: true` puts the real sky back.",
            said.join(" ")
        ),
        json!({
            "solar_hours": s.dev.time_of_day,
            "day_of_year": s.dev.sun_day_of_year,
            "clouds": s.dev.cloud_cover,
            "fog": s.dev.fog_density,
            "rain": s.dev.precipitation,
            "weather_live": s.weather.live,
        }),
    )
}

// ---------------------------------------------------------------------------
// Transit and notices
// ---------------------------------------------------------------------------

/// `"14:05"` at the place's own clock.
fn clock_time(clock: &crate::systems::transit_live::WorldClock, unix: i64) -> String {
    let off = i64::from(clock.local_offset_secs.unwrap_or(0));
    let s = (unix + off).rem_euclid(86_400);
    format!("{:02}:{:02}", s / 3600, s / 60 % 60)
}

fn same_line(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

fn departures(
    w: &mut WorldWorld,
    anchor: (f64, f64, u32),
    args: &DeparturesArgs,
    first: bool,
    last: bool,
) -> Option<AgentReply> {
    use crate::systems::data_layers::TRANSIT_LAYER;
    let at = match w.point(anchor, args.lat, args.lng) {
        Ok(at) => at,
        Err(why) => return Some(AgentReply::refuse(why)),
    };
    let mut note = String::new();
    if !w.feeds.layers.layers[TRANSIT_LAYER].enabled {
        if first {
            // The stops stream only with their layer on, so asking about them
            // puts the stop pins on the person's map; said in the answer.
            w.feeds.layers.layers[TRANSIT_LAYER].enabled = true;
        }
        note = " The transit layer was off; it is on now, so the stops show on the map \
                 (`set_layer` hides it)."
            .into();
    }
    if let Some(cam) = w.camera.stream_xz() {
        let cam = geo_of(anchor, cam);
        if great_circle_m(cam, at) > 1200.0 {
            return Some(AgentReply::refuse(format!(
                "That is {} from where the map is loaded; stops and their boards load round \
                 the camera. `fly_to` it first.",
                metres(great_circle_m(cam, at))
            )));
        }
    }
    let radius = args.radius_m.unwrap_or(300.0).clamp(20.0, 800.0);
    let p = xz(anchor, at);
    let layer = &w.feeds.layers.layers[TRANSIT_LAYER];
    let mut stops: Vec<(f32, &crate::systems::data_layers::PoiInstance, Vec2)> = layer
        .pois
        .values()
        .flatten()
        .map(|poi| {
            let s = crate::grid::anchored_xz(&poi.cell, poi.anchor);
            (s.distance(p), poi, s)
        })
        .filter(|(d, _, _)| *d <= radius)
        .collect();
    stops.sort_by(|a, b| a.0.total_cmp(&b.0));
    // One stop is often two poles, one each way, with one name: keep both,
    // since their boards differ.
    // Every stop that will be listed and has a live feed must have its board
    // in, or the answer reads "no times yet" for stops a poll later would
    // have filled: the boards arrive over several polls, in batches.
    let boards_in = stops
        .iter()
        .take(8)
        .filter_map(|(_, poi, _)| poi.live_id.as_ref())
        .all(|id| w.feeds.live.boards.contains_key(id));
    if !((boards_in && !stops.is_empty()) || last) {
        return None;
    }
    if stops.is_empty() {
        return Some(AgentReply::ok(
            format!("No stops within {radius:.0} m.{note}"),
            json!({"stops": []}),
        ));
    }
    let now = w.screen.clock.now_unix as i64;
    let mut lines = Vec::new();
    let mut out = Vec::new();
    for (d, poi, s) in stops.iter().take(8) {
        let name = poi.name.clone().unwrap_or_else(|| "A stop".into());
        let (_, dir) = around::how_far(p, *s);
        let board = poi
            .live_id
            .as_ref()
            .and_then(|id| w.feeds.live.boards.get(id));
        let leaving: Vec<_> = board
            .map(|b| {
                b.departures
                    .iter()
                    .filter(|d| !d.passed && d.expected_unix >= now - 30)
                    .filter(|d| args.line.as_deref().is_none_or(|l| same_line(l, &d.line)))
                    .take(6)
                    .collect()
            })
            .unwrap_or_default();
        let head = format!("{name} ({d:.0} m {dir})");
        if board.is_none() {
            lines.push(format!(
                "{head}: {}",
                if poi.live_id.is_some() {
                    "no times have arrived yet."
                } else {
                    "no live times for this stop."
                }
            ));
        } else if leaving.is_empty() {
            lines.push(format!("{head}: nothing leaving soon."));
        } else {
            let each: Vec<String> = leaving
                .iter()
                .map(|d| {
                    let late = (d.expected_unix - d.target_unix) / 60;
                    format!(
                        "{} {} to {} at {}{}",
                        d.transport.to_lowercase(),
                        d.line,
                        d.destination,
                        clock_time(&w.screen.clock, d.expected_unix),
                        if d.cancelled {
                            " — cancelled".to_string()
                        } else if late > 0 {
                            format!(" ({late} min late)")
                        } else {
                            String::new()
                        }
                    )
                })
                .collect();
            lines.push(format!("{head}:\n  {}", each.join("\n  ")));
        }
        let (lat, lng) = geo_of(anchor, *s);
        out.push(json!({
            "stop": name, "lat": lat, "lng": lng, "distance_m": d,
            "departures": leaving.iter().map(|d| json!({
                "line": d.line, "to": d.destination, "mode": d.transport,
                "planned": clock_time(&w.screen.clock, d.target_unix),
                "expected": clock_time(&w.screen.clock, d.expected_unix),
                "in_min": (d.expected_unix - now) / 60,
                "cancelled": d.cancelled,
            })).collect::<Vec<_>>(),
        }));
    }
    Some(AgentReply::ok(
        format!("{}{note}", lines.join("\n")),
        json!({"stops": out}),
    ))
}

fn vehicles(
    w: &mut WorldWorld,
    anchor: (f64, f64, u32),
    args: &VehiclesArgs,
    first: bool,
    last: bool,
) -> Option<AgentReply> {
    let at = match w.point(anchor, args.lat, args.lng) {
        Ok(at) => at,
        Err(why) => return Some(AgentReply::refuse(why)),
    };
    if first && !w.feeds.vehicles.agent_wants {
        w.feeds.vehicles.agent_wants = true;
    }
    if !w.feeds.vehicles.polled() {
        return last.then(|| {
            AgentReply::refuse(
                "The vehicle feed has not answered. It is read only with the camera below \
                 20 km over the Netherlands.",
            )
        });
    }
    let p = xz(anchor, at);
    let radius = args.radius_m.unwrap_or(3000.0).max(50.0);
    let now = w.screen.clock.now_unix;
    let mut found: Vec<(f32, &str, &str, Vec2)> = w
        .feeds
        .vehicles
        .positions(now)
        .filter(|(_, line, _, _)| match (&args.line, line) {
            (Some(want), Some(line)) => same_line(want, line),
            (Some(_), None) => false,
            (None, _) => true,
        })
        .map(|(_, line, mode, pos)| (pos.distance(p), line.unwrap_or("?"), mode, pos))
        .filter(|(d, ..)| *d <= radius)
        .collect();
    found.sort_by(|a, b| a.0.total_cmp(&b.0));
    if found.is_empty() {
        return Some(AgentReply::ok(
            format!(
                "No {}vehicles within {}.",
                args.line
                    .as_deref()
                    .map(|l| format!("line {l} "))
                    .unwrap_or_default(),
                metres(f64::from(radius))
            ),
            json!({"vehicles": []}),
        ));
    }
    let lines: Vec<String> = found
        .iter()
        .take(20)
        .map(|(d, line, mode, pos)| {
            let (_, dir) = around::how_far(p, *pos);
            format!("{mode} {line}: {} {dir}", metres(f64::from(*d)))
        })
        .collect();
    let data: Vec<Value> = found
        .iter()
        .take(50)
        .map(|(d, line, mode, pos)| {
            let (lat, lng) = geo_of(anchor, *pos);
            json!({"line": line, "mode": mode, "lat": lat, "lng": lng, "distance_m": d})
        })
        .collect();
    Some(AgentReply::ok(
        format!(
            "{} within {}, nearest first:\n{}",
            found.len(),
            metres(f64::from(radius)),
            lines.join("\n")
        ),
        json!({"vehicles": data}),
    ))
}

fn notices(
    w: &mut WorldWorld,
    anchor: (f64, f64, u32),
    args: &NoticesArgs,
    first: bool,
    last: bool,
) -> Option<AgentReply> {
    let at = match w.point(anchor, args.lat, args.lng) {
        Ok(at) => at,
        Err(why) => return Some(AgentReply::refuse(why)),
    };
    if !crate::systems::plans::covers(at.0, at.1) {
        return Some(AgentReply::refuse(
            "The municipality's notices are read for Groningen only, so far.",
        ));
    }
    if first && !w.feeds.plans.agent_wants {
        w.feeds.plans.agent_wants = true;
    }
    let Some(feed) = w.feeds.plans.feed() else {
        if w.feeds.plans.failed() || last {
            return Some(AgentReply::refuse(
                "The notices could not be read just now.",
            ));
        }
        return None;
    };
    let today = cartopolis_core::plans::civil_date(w.screen.clock.now_unix.max(0.0) as u64);
    let radius = f64::from(args.radius_m.unwrap_or(300.0).max(10.0));
    let open_only = args.open_only.unwrap_or(true);
    let mut near: Vec<(f64, &cartopolis_core::plans::Notice)> = feed
        .notices
        .iter()
        .map(|n| (great_circle_m(at, (n.lat, n.lng)), n))
        .filter(|(d, n)| *d <= radius && (!open_only || n.open_on(&today)))
        .collect();
    near.sort_by(|a, b| a.0.total_cmp(&b.0));
    if near.is_empty() {
        return Some(AgentReply::ok(
            format!(
                "No {}notices within {radius:.0} m.",
                if open_only { "open " } else { "" }
            ),
            json!({"notices": []}),
        ));
    }
    let lines: Vec<String> = near
        .iter()
        .take(15)
        .map(|(d, n)| {
            format!(
                "{} ({}, {}, {d:.0} m): {}{}",
                n.place,
                n.kind.key(),
                n.date,
                n.title,
                n.respond_until
                    .as_deref()
                    .map(|u| format!(" — respond by {u}"))
                    .unwrap_or_default()
            )
        })
        .collect();
    let data: Vec<Value> = near
        .iter()
        .take(40)
        .map(|(d, n)| {
            json!({
                "id": n.id, "kind": n.kind.key(), "title": n.title, "place": n.place,
                "date": n.date, "respond_until": n.respond_until, "lat": n.lat, "lng": n.lng,
                "distance_m": d, "url": cartopolis_core::plans::notice_url(&n.id),
            })
        })
        .collect();
    Some(AgentReply::ok(
        format!(
            "{} {}notices within {radius:.0} m, nearest first:\n{}",
            near.len(),
            if open_only { "open " } else { "" },
            lines.join("\n")
        ),
        json!({"notices": data}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(centre: Vec3, half: Vec3, rot: Quat) -> Boxed {
        Boxed {
            centre,
            axes: [rot * Vec3::X, rot * Vec3::Y, rot * Vec3::Z],
            half,
        }
    }

    #[test]
    fn stacked_blocks_touch_and_overlapping_ones_overlap() {
        let a = unit(Vec3::new(0.0, 0.5, 0.0), Vec3::splat(0.5), Quat::IDENTITY);
        let on_top = unit(Vec3::new(0.0, 1.5, 0.0), Vec3::splat(0.5), Quat::IDENTITY);
        assert!(overlap_depth(&a, &on_top).is_none_or(|d| d <= OVERLAP_TOLERANCE_M));
        let into = unit(Vec3::new(0.0, 1.2, 0.0), Vec3::splat(0.5), Quat::IDENTITY);
        let d = overlap_depth(&a, &into).unwrap();
        assert!((d - 0.3).abs() < 1e-4, "{d}");
        let apart = unit(Vec3::new(3.0, 0.5, 0.0), Vec3::splat(0.5), Quat::IDENTITY);
        assert!(overlap_depth(&a, &apart).is_none());
    }

    #[test]
    fn a_turned_box_is_separated_on_a_cross_axis() {
        // Two long thin bars crossing in plan but apart in height overlap on
        // every face axis' shadow except the vertical one.
        let a = unit(Vec3::ZERO, Vec3::new(2.0, 0.1, 0.1), Quat::IDENTITY);
        let b = unit(
            Vec3::new(0.0, 0.5, 0.0),
            Vec3::new(2.0, 0.1, 0.1),
            Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
        );
        assert!(overlap_depth(&a, &b).is_none());
        let rot = Quat::from_rotation_y(0.7);
        let c = unit(Vec3::new(0.0, 0.05, 0.0), Vec3::new(2.0, 0.1, 0.1), rot);
        assert!(overlap_depth(&a, &c).is_some());
    }

    #[test]
    fn a_turned_box_reaches_lower() {
        let flat = unit(
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(1.0, 0.1, 0.1),
            Quat::IDENTITY,
        );
        let tilted = unit(
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(1.0, 0.1, 0.1),
            Quat::from_rotation_z(std::f32::consts::FRAC_PI_4),
        );
        assert!((flat.lowest() - 0.9).abs() < 1e-4);
        assert!(tilted.lowest() < 0.3);
    }

    #[test]
    fn a_square_measures_its_area() {
        // 100 m on a side at Groningen's latitude.
        let o: (f64, f64) = (53.2194, 6.5665);
        let dlat = 100.0 / 111_320.0;
        let dlng = 100.0 / (111_320.0 * o.0.to_radians().cos());
        let ring = [
            o,
            (o.0, o.1 + dlng),
            (o.0 + dlat, o.1 + dlng),
            (o.0 + dlat, o.1),
        ];
        let a = area_m2(&ring);
        assert!((a - 10_000.0).abs() < 50.0, "{a}");
        let r = measure(&MeasureArgs {
            points: ring.iter().map(|p| [p.0, p.1]).collect(),
        });
        assert!(!r.error);
        assert!((r.data["perimeter_m"].as_f64().unwrap() - 400.0).abs() < 2.0);
    }

    #[test]
    fn clock_and_day_parse() {
        assert_eq!(parse_clock("17:30"), Some(1050.0));
        assert_eq!(parse_clock("24:00"), None);
        assert_eq!(parse_day("01-01"), Some(1.0));
        assert_eq!(parse_day("2028-03-01"), Some(61.0), "leap year");
        assert_eq!(parse_day("2027-03-01"), Some(60.0));
        assert_eq!(parse_day("02-29"), Some(60.0));
        assert_eq!(parse_day("13-01"), None);
    }
}
