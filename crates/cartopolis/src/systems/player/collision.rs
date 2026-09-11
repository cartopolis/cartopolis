//! Avatar ↔ building collision: block a walking avatar from entering buildings,
//! and eject one that ends up inside (teleporting into it, or a building
//! streaming in on top of it) to just outside the wall.
//!
//! Both building streamers publish their footprints here as closed 2D polygons
//! in the anchored world-XZ frame — the same frame the avatar's absolute
//! position lives in (the anchor tile centre is the origin; see
//! `osm_buildings::lat_lng_to_local` and `geo_nav::lat_lng_to_world`, which
//! agree because `map_stream::snap_anchor` pins the anchor to a tile centre).
//! `osm_buildings` contributes the near OSM detail patch (exact footprint
//! rings); `lod22` contributes the 3DBAG cells (convex hull of each building's
//! projected vertices, since only a triangle soup survives there).
//!
//! **A footprint is solid or hollow, and the two are different shapes of rule.**
//! A solid one is a region the avatar may not be *in*: a point test answers it,
//! and anything that ends up inside is pushed back out. That is right for a
//! building we have nothing but a mass for, and it was the only rule here until
//! `systems::interiors` started putting rooms behind the doors. A building with
//! a room in it is hollow: what stops the avatar is its *wall*, a line with no
//! thickness, and the inside is somewhere to be. Keeping the solid rule for
//! those was the freeze — the doorway let the avatar into a 1.1 m disc of the
//! collider and the whole floor beyond it was still a no-go region, so a step
//! further in was refused in both axes and the avatar stood in its own doorway
//! unable to move.
//!
//! A wall is therefore tested by **crossing**, never by sampling: it has no
//! interior to be inside of, so a point test would miss it on any frame long
//! enough to step over it and the avatar would walk out through the back of the
//! house. [`Footprint::crosses_wall`] is the one rule a hollow building has.

use crate::components::Avatar;
use crate::grid::WORLD_CELL_EDGE;
use crate::resources::AvatarState;
use bevy::prelude::*;
use big_space::prelude::GridCell;
use std::collections::HashMap;

/// Half the width of the gap a door opens in a building's collider, metres.
///
/// Generous next to a real door leaf, and deliberately. The point this is
/// centred on lies *in* the wall (`building_facts::place_doors_on_facades` puts
/// it there), so the gap straddles the wall and the depth is not in question —
/// but the position *along* the wall is still only derived from the unit's own
/// point, so the doorway sits within a metre or so of the real leaf rather than
/// on it, and a tight gap would be a door you have to hunt for with the
/// keyboard. Wide enough to walk through at a jog, narrow enough that a terrace
/// of front doors does not become an open colonnade.
pub const DOOR_HALF_WIDTH: f32 = 1.1;

/// How far beyond a wall an ejected avatar is placed (metres).
pub const EJECT_MARGIN: f32 = 1.0;

/// How far ahead the walk velocity is probed for a wall (the avatar's effective
/// body radius — it stops roughly this far from a facade).
pub const COLLIDE_PROBE_M: f32 = 0.6;

/// A building footprint as a closed polygon in world-XZ metres (`x` = east,
/// `y` = south, i.e. world `z`), with a cached AABB for a cheap broad-phase.
#[derive(Clone)]
pub struct Footprint {
    ring: Vec<Vec2>,
    min: Vec2,
    max: Vec2,
    /// Where this building can be walked through: its entrance points, from the
    /// register (`building_facts::BuildingFact::doors`).
    ///
    /// A doorway is a *hole in the collider*, not a shape in the ring. Cutting a
    /// notch into the polygon would be the tidy version and it is the wrong one:
    /// the ring is also what the interior room is built from, and a notched ring
    /// would put a wedge-shaped gap in the wall of every room. So the polygon
    /// stays whole and the test below lets the avatar past within
    /// [`DOOR_HALF_WIDTH`] of a door.
    doors: Vec<Vec2>,
    /// Whether there is a room behind the door — see the module header. Hollow
    /// buildings collide on their walls only; solid ones are a no-go region.
    hollow: bool,
}

impl Footprint {
    /// Build a footprint from a ring of world-XZ points (≥ 3), or `None` if the
    /// ring is degenerate.
    pub fn new(ring: Vec<Vec2>) -> Option<Self> {
        Self::with_doors(ring, &[], false)
    }

    /// Build a footprint that can be walked through at the given entrance points.
    ///
    /// A door only opens the collider; nothing here decides whether there is
    /// anything on the other side. That is `systems::interiors`, and the two must
    /// agree — a doorway into a building with no room is a way to stand inside a
    /// shell whose walls are culled away, i.e. a hole you fall out of the world
    /// through, visually. `hollow` is the caller saying it has checked: it is
    /// exactly `interiors::can_hold_a_room`, asked at the one place both the
    /// collider and the room are derived from the same outline (`lod22`), so the
    /// building you can walk into is the building with a floor in it.
    pub fn with_doors(ring: Vec<Vec2>, doors: &[Vec2], hollow: bool) -> Option<Self> {
        if ring.len() < 3 {
            return None;
        }
        let mut min = ring[0];
        let mut max = ring[0];
        for p in &ring[1..] {
            min = min.min(*p);
            max = max.max(*p);
        }
        Some(Self {
            ring,
            min,
            max,
            doors: doors.to_vec(),
            hollow,
        })
    }

    /// Whether `p` is close enough to one of this building's doors to pass.
    ///
    /// Half a doorway either side of the point, in every direction. A radius
    /// rather than a test against the wall segment because the point is derived
    /// (`building_facts::place_doors_on_facades`) and is only approximately
    /// where the leaf is along the wall — and because a radius centred in the
    /// wall opens it from both sides at once, which is what walking in and back
    /// out again needs.
    fn in_a_doorway(&self, p: Vec2) -> bool {
        self.doors
            .iter()
            .any(|d| d.distance_squared(p) <= DOOR_HALF_WIDTH * DOOR_HALF_WIDTH)
    }

    fn aabb_contains(&self, p: Vec2) -> bool {
        p.x >= self.min.x && p.x <= self.max.x && p.y >= self.min.y && p.y <= self.max.y
    }

    /// Even-odd ray cast: true when `p` is strictly inside the ring — and not
    /// standing in one of its doorways.
    ///
    /// Always false for a hollow building, and that is the whole of what makes
    /// one walkable: nothing about its inside is a place the avatar may not be,
    /// so nothing blocks a step taken there and nothing ejects it back out. What
    /// stops it at the facade is [`Self::crosses_wall`].
    fn contains(&self, p: Vec2) -> bool {
        if self.hollow {
            return false;
        }
        if !self.aabb_contains(p) {
            return false;
        }
        if self.in_a_doorway(p) {
            return false;
        }
        let n = self.ring.len();
        let mut inside = false;
        let mut j = n - 1;
        for i in 0..n {
            let a = self.ring[i];
            let b = self.ring[j];
            if (a.y > p.y) != (b.y > p.y) {
                let x = a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x);
                if p.x < x {
                    inside = !inside;
                }
            }
            j = i;
        }
        inside
    }

    /// Whether the straight move `from → to` passes through one of this
    /// building's walls anywhere that is not a doorway.
    ///
    /// A crossing test rather than a sample, for the reason the module header
    /// gives: a wall is a line, and a line is what a sampled point test walks
    /// over. It is also what makes the online path work at all — that one probes
    /// [`COLLIDE_PROBE_M`] ahead of the avatar, which from just outside a facade
    /// lands well inside the room, so asking "is that point solid?" would answer
    /// no and walk the avatar straight through the brickwork.
    fn crosses_wall(&self, from: Vec2, to: Vec2) -> bool {
        let (lo, hi) = (from.min(to), from.max(to));
        if self.max.x < lo.x || self.min.x > hi.x || self.max.y < lo.y || self.min.y > hi.y {
            return false;
        }
        let d = to - from;
        let n = self.ring.len();
        for i in 0..n {
            let a = self.ring[i];
            let b = self.ring[(i + 1) % n];
            let e = b - a;
            let denom = d.x * e.y - d.y * e.x;
            if denom.abs() < 1e-9 {
                continue; // parallel; a move along a wall does not cross it
            }
            let ap = a - from;
            let t = (ap.x * e.y - ap.y * e.x) / denom;
            let u = (ap.x * d.y - ap.y * d.x) / denom;
            if !(0.0..=1.0).contains(&t) || !(0.0..=1.0).contains(&u) {
                continue;
            }
            // Where it went through, not where it ended up: a doorway is a hole
            // in a particular part of a particular wall.
            if !self.in_a_doorway(from + d * t) {
                return true;
            }
        }
        false
    }

    /// Nearest point on the polygon boundary to `p`.
    fn nearest_boundary(&self, p: Vec2) -> Vec2 {
        let n = self.ring.len();
        let mut best = self.ring[0];
        let mut best_d = f32::MAX;
        for i in 0..n {
            let a = self.ring[i];
            let b = self.ring[(i + 1) % n];
            let ab = b - a;
            let len2 = ab.length_squared();
            let t = if len2 > 1e-9 {
                ((p - a).dot(ab) / len2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let q = a + ab * t;
            let d = q.distance_squared(p);
            if d < best_d {
                best_d = d;
                best = q;
            }
        }
        best
    }
}

/// All building footprints available for collision, split by their source so
/// each streamer can replace only its own slice as tiles come and go.
#[derive(Resource, Default)]
pub struct BuildingColliders {
    /// The near OSM detail patch (whole set replaced on each rebuild).
    detail: Vec<Footprint>,
    /// 3DBAG LoD2.2 footprints keyed by their cell, dropped with the cell.
    lod22: HashMap<(i64, i64), Vec<Footprint>>,
    /// Per-source gates mirrored from the Layers panel's Buildings checkboxes
    /// (`data_layers::apply_feature_toggles`), so a hidden source's footprints
    /// don't act as invisible walls. Negated (`_off`) so `derive(Default)`
    /// leaves both sources active.
    detail_off: bool,
    lod22_off: bool,
    /// Street furniture as `(centre, radius)` circles — benches, bins, shelters.
    ///
    /// Circles rather than rings because that is genuinely the right shape here:
    /// these are small, roughly symmetric objects, and the only question ever
    /// asked of them is "can the avatar be here". Mirrored from
    /// `world_places::WorldPlaces` each time it changes rather than published by
    /// the streamer directly, because the same extraction feeds the interaction
    /// prompt and the two must not disagree about where a bench is.
    props: Vec<(Vec2, f32)>,
    /// Street-detail footprints (walls, fences, hedges, bollards — the Overpass
    /// layer, `systems::street_detail`), keyed by its z14 cell and dropped with
    /// it. Same lifetime contract as `lod22`: the streamer that draws the mesh
    /// publishes the collider, so a wall you can see is a wall you cannot cross.
    features: HashMap<(i64, i64), Vec<Footprint>>,
}

impl BuildingColliders {
    /// Replace the OSM detail-patch footprints.
    pub fn set_detail(&mut self, fps: Vec<Footprint>) {
        self.detail = fps;
    }
    /// Drop the OSM detail-patch footprints (re-anchor).
    pub fn clear_detail(&mut self) {
        self.detail.clear();
    }
    /// Install (or replace) one LoD2.2 cell's footprints.
    pub fn set_cell(&mut self, cell: (i64, i64), fps: Vec<Footprint>) {
        self.lod22.insert(cell, fps);
    }
    /// Drop one LoD2.2 cell's footprints (cell despawned).
    pub fn remove_cell(&mut self, cell: (i64, i64)) {
        self.lod22.remove(&cell);
    }
    /// Drop every LoD2.2 cell's footprints (re-anchor).
    pub fn clear_lod22(&mut self) {
        self.lod22.clear();
    }

    /// Gate each source on its Layers-panel checkbox (collision matches what's
    /// drawn). A disabled source keeps its footprints — the streamers go on
    /// publishing — but `iter` skips them.
    pub fn set_sources(&mut self, detail_on: bool, lod22_on: bool) {
        self.detail_off = !detail_on;
        self.lod22_off = !lod22_on;
    }

    /// Replace the street-furniture circles.
    pub fn set_props(&mut self, props: Vec<(Vec2, f32)>) {
        self.props = props;
    }

    /// Install (or replace) one street-detail cell's footprints.
    pub fn set_feature_cell(&mut self, cell: (i64, i64), fps: Vec<Footprint>) {
        self.features.insert(cell, fps);
    }
    /// Drop one street-detail cell's footprints (cell despawned).
    pub fn remove_feature_cell(&mut self, cell: (i64, i64)) {
        self.features.remove(&cell);
    }
    /// Drop every street-detail footprint (re-anchor or layer off).
    pub fn clear_features(&mut self) {
        self.features.clear();
    }

    pub fn is_empty(&self) -> bool {
        (self.detail_off || self.detail.is_empty())
            && (self.lod22_off || self.lod22.is_empty())
            && self.props.is_empty()
            && self.features.is_empty()
    }

    /// True if `p` is inside a prop's circle. Separate from [`Self::blocked`]'s
    /// polygon test because the shapes are different, not because the rule is —
    /// every caller treats the two the same.
    fn in_prop(&self, p: Vec2) -> bool {
        self.props
            .iter()
            .any(|(c, r)| c.distance_squared(p) < r * r)
    }

    fn iter(&self) -> impl Iterator<Item = &Footprint> {
        let detail = if self.detail_off {
            &[]
        } else {
            self.detail.as_slice()
        };
        let lod22_on = !self.lod22_off;
        detail
            .iter()
            .chain(self.lod22.values().filter(move |_| lod22_on).flatten())
            .chain(self.features.values().flatten())
    }

    /// True if `p` (world XZ) lies inside any footprint or prop.
    fn blocked(&self, p: Vec2) -> bool {
        self.iter().any(|f| f.contains(p)) || self.in_prop(p)
    }

    /// True if the move `from → to` is refused — the question every mover here
    /// actually asks, and the one that has an answer for both kinds of
    /// footprint. A solid building (or a prop) refuses a move that *ends* inside
    /// it; a hollow one refuses a move that *crosses* one of its walls.
    fn blocked_move(&self, from: Vec2, to: Vec2) -> bool {
        if self.blocked(to) {
            return true;
        }
        self.iter().any(|f| f.hollow && f.crosses_wall(from, to))
    }

    /// The footprint containing `p`, if any.
    fn hit(&self, p: Vec2) -> Option<&Footprint> {
        self.iter().find(|f| f.contains(p))
    }

    /// How far along the segment `from → to` (world XZ) the first wall is, as a
    /// fraction in `0..1`, or `None` if the segment is clear.
    ///
    /// This is the follow camera's query, not the avatar's: the boom runs from the
    /// avatar out to the camera, and anything it crosses is a facade about to be
    /// between you and your own avatar. It is a real segment/edge intersection
    /// rather than a sampled march because the failure mode of sampling is missing
    /// a thin wall entirely — and a camera that pops through a wall on some frames
    /// and not others is worse than one that never collides.
    ///
    /// `from` inside a footprint answers `None`: the avatar is already indoors (or
    /// a building streamed in on top of it), and pulling the camera to zero there
    /// would jam it inside the avatar's head until it walked out.
    pub fn segment_fraction(&self, from: Vec2, to: Vec2) -> Option<f32> {
        if self.blocked(from) {
            return None;
        }
        let d = to - from;
        if d.length_squared() < 1e-9 {
            return None;
        }
        let (lo, hi) = (from.min(to), from.max(to));
        let mut best: Option<f32> = None;
        for f in self.iter() {
            // Broad phase: the segment's AABB against the footprint's.
            if f.max.x < lo.x || f.min.x > hi.x || f.max.y < lo.y || f.min.y > hi.y {
                continue;
            }
            let n = f.ring.len();
            for i in 0..n {
                let a = f.ring[i];
                let b = f.ring[(i + 1) % n];
                let e = b - a;
                let denom = d.x * e.y - d.y * e.x;
                if denom.abs() < 1e-9 {
                    continue; // parallel; a grazing hit is not worth a special case
                }
                let ap = a - from;
                let t = (ap.x * e.y - ap.y * e.x) / denom;
                let u = (ap.x * d.y - ap.y * d.x) / denom;
                if (0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u) {
                    best = Some(best.map_or(t, |b: f32| b.min(t)));
                }
            }
        }
        best
    }

    /// Resolve a desired walk from `from` to `to` (world XZ), sliding along walls
    /// so pushing into a building glides along the facade instead of sticking.
    /// The avatar is treated as a point.
    pub fn slide(&self, from: Vec2, to: Vec2) -> Vec2 {
        // Already inside something solid (just teleported / a building streamed
        // over us)? Don't trap the avatar — let it move; `eject` pushes it back
        // out this frame. A hollow building never answers this, which is the
        // point: being indoors is not being stuck in something.
        if self.blocked(from) {
            return to;
        }
        if !self.blocked_move(from, to) {
            return to;
        }
        // Axis separation: keep whichever single-axis component stays clear. Each
        // candidate is a move in its own right and is tested as one, or a step
        // along a wall could hop through the corner of it.
        let slide_x = Vec2::new(to.x, from.y);
        if !self.blocked_move(from, slide_x) {
            return slide_x;
        }
        let slide_z = Vec2::new(from.x, to.y);
        if !self.blocked_move(from, slide_z) {
            return slide_z;
        }
        from
    }

    /// If `p` is inside a footprint, the nearest point `margin` metres outside
    /// its wall; otherwise `p` unchanged.
    pub fn eject(&self, p: Vec2, margin: f32) -> Vec2 {
        let Some(fp) = self.hit(p) else {
            return p;
        };
        let q = fp.nearest_boundary(p);
        // `p` is interior, so `q - p` points outward through the nearest wall.
        let out = (q - p).normalize_or_zero();
        if out == Vec2::ZERO {
            return p; // degenerate (on the boundary); leave as-is
        }
        q + out * margin
    }

    /// Unit direction from `p` toward the nearest wall when `p` is inside a
    /// footprint, else `None`. Used server-side-authoritative modes, where the
    /// avatar can only be moved by steering its velocity: an avatar that ends up
    /// inside a building (teleported in — no interiors yet) is walked straight
    /// back out along this vector.
    pub fn eject_dir(&self, p: Vec2) -> Option<Vec2> {
        if let Some(fp) = self.hit(p) {
            let out = (fp.nearest_boundary(p) - p).normalize_or_zero();
            if out != Vec2::ZERO {
                return Some(out);
            }
        }
        // A prop pushes straight out from its centre — the nearest boundary point
        // of a circle is on that ray, so there is nothing to search. Without this
        // an avatar that a bench streamed in on top of would stand inside it: the
        // footprint path answers `None` for a prop, and `slide` then lets the
        // avatar move but never pushes it clear.
        self.props
            .iter()
            .find(|(c, r)| c.distance_squared(p) < r * r)
            .map(|(c, _)| (p - *c).normalize_or_zero())
            .filter(|v| *v != Vec2::ZERO)
    }

    /// Steer a desired walk velocity (`wish`, world XZ) from the avatar's current
    /// position `pos` so it slides along walls and never enters a building — the
    /// client-side prediction for the authoritative (online) movement path,
    /// where the sim integrates exactly this velocity and has no building data.
    /// Returns the (possibly redirected, possibly zeroed) velocity; if the avatar
    /// is already inside a footprint it returns a straight push outward at the
    /// wish's speed so it auto-walks clear.
    pub fn steer_velocity(&self, pos: Vec2, wish: Vec2) -> Vec2 {
        if let Some(out) = self.eject_dir(pos) {
            return out * wish.length().max(1.0);
        }
        let speed = wish.length();
        if speed < 1e-6 {
            return wish;
        }
        let probe = pos + wish / speed * COLLIDE_PROBE_M;
        let allowed = self.slide(pos, probe) - pos;
        if allowed.length_squared() < 1e-6 {
            Vec2::ZERO
        } else {
            allowed.normalize() * speed
        }
    }
}

/// Mirror the streamed street furniture into the collider index, so the props the
/// city draws are the props the avatar cannot walk through.
///
/// Keyed off `WorldPlaces::revision` rather than run every frame: the ring only
/// changes when a cell streams in or out, and rebuilding a few thousand circles
/// on every frame of a walk would be pure waste. Gated on the furniture layer's
/// own checkbox for the reason `set_sources` exists — collision must match what is
/// drawn, or a hidden bench is an invisible wall.
pub fn sync_prop_colliders(
    places: Res<crate::systems::world_places::WorldPlaces>,
    layers: Res<crate::systems::data_layers::DataLayers>,
    mut colliders: ResMut<BuildingColliders>,
    mut last: Local<Option<(u32, bool)>>,
) {
    let enabled = layers.furniture_enabled;
    let key = (places.revision, enabled);
    if *last == Some(key) {
        return;
    }
    *last = Some(key);
    colliders.set_props(if enabled {
        places.props().copied().collect()
    } else {
        Vec::new()
    });
}

/// Convex hull (counter-clockwise) of XZ points via Andrew's monotone chain.
/// Used for LoD2.2 buildings, which arrive as a triangle soup with no ordered
/// footprint ring — the hull over-approximates concave shapes but gives a solid
/// no-go polygon that respects the building's orientation (an axis-aligned box
/// would not).
pub fn convex_hull_xz(mut pts: Vec<Vec2>) -> Vec<Vec2> {
    // The points are decoded from network-fetched 3DBAG data, so a single NaN
    // coordinate must cost that vertex, not the client — `partial_cmp().unwrap()`
    // here was a remote panic. Non-finite points can't contribute to a hull
    // anyway (cross products against them are NaN and poison the winding test).
    pts.retain(|p| p.x.is_finite() && p.y.is_finite());
    if pts.len() < 3 {
        return pts;
    }
    pts.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    pts.dedup();
    if pts.len() < 3 {
        return pts;
    }
    let cross = |o: Vec2, a: Vec2, b: Vec2| (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x);
    let mut hull: Vec<Vec2> = Vec::with_capacity(pts.len() + 1);
    // Lower hull.
    for &p in &pts {
        while hull.len() >= 2 && cross(hull[hull.len() - 2], hull[hull.len() - 1], p) <= 0.0 {
            hull.pop();
        }
        hull.push(p);
    }
    // Upper hull.
    let lower = hull.len() + 1;
    for &p in pts.iter().rev() {
        while hull.len() >= lower && cross(hull[hull.len() - 2], hull[hull.len() - 1], p) <= 0.0 {
            hull.pop();
        }
        hull.push(p);
    }
    hull.pop(); // last point equals the first
    hull
}

/// Push a walking avatar out of any building it ends up inside — the way a
/// teleport that lands inside a building (no interiors yet) resolves, and a
/// backstop for a building that streams in over the avatar. Runs after
/// movement, so it corrects the final position; a no-op when the avatar is
/// clear, off the ground, online (server-authoritative), or the buildings layer
/// is off.
pub fn eject_avatar_from_buildings(
    colliders: Res<BuildingColliders>,
    layers: Res<crate::systems::data_layers::DataLayers>,
    online: Option<Res<crate::resources::OnlineSession>>,
    terrain: Res<crate::systems::terrain::TerrainField>,
    mut avatar_state: ResMut<AvatarState>,
    mut avatars: Query<(&GridCell, &mut Transform), With<Avatar>>,
) {
    if online.is_some() || !layers.buildings_enabled || colliders.is_empty() {
        return;
    }
    let Ok((cell, mut tf)) = avatars.single_mut() else {
        return;
    };
    // Only a pedestrian is ejected. The footprints have no height, so shoving a
    // flying — or falling — avatar sideways would move it around a building it is
    // passing well above.
    //
    // The ground this is measured against is the *measured* one. Offline the
    // transform is the avatar's state rather than a display offset, so standing
    // still on a hill puts `translation.y` at `GROUND_HEIGHT + terrain.at(..)` —
    // which above 0.15 m of relief already exceeds `AIRBORNE_EPS`, and the snap-out
    // backstop then never ran. A teleport that landed inside a building stranded
    // the avatar there for good. `avatar::handle_avatar_movement` computes the same
    // ground the same way, a few lines before it calls `slide`.
    //
    // The storey counts as ground for the same reason, and it has to: a first
    // floor stands 3.2 m up, so without it every avatar upstairs reads as flying
    // and neither this backstop nor the slide that runs before it does anything
    // at all — which is a storey whose walls are a picture.
    let ground_y = crate::systems::avatar::GROUND_HEIGHT
        + terrain.at(tf.translation.x, tf.translation.z)
        + avatar_state.floor_lift;
    if avatar_state.is_flying || tf.translation.y > ground_y + crate::systems::avatar::AIRBORNE_EPS
    {
        return;
    }
    let base = Vec2::new(
        cell.x as f32 * WORLD_CELL_EDGE,
        cell.z as f32 * WORLD_CELL_EDGE,
    );
    let p = base + Vec2::new(tf.translation.x, tf.translation.z);
    let fixed = colliders.eject(p, EJECT_MARGIN);
    if fixed != p {
        tf.translation.x = fixed.x - base.x;
        tf.translation.z = fixed.y - base.y;
        avatar_state.position = tf.translation;
        avatar_state.display_position = tf.translation;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 10×10 m square footprint centred on the origin.
    fn unit_square() -> Footprint {
        Footprint::new(vec![
            Vec2::new(-5.0, -5.0),
            Vec2::new(5.0, -5.0),
            Vec2::new(5.0, 5.0),
            Vec2::new(-5.0, 5.0),
        ])
        .unwrap()
    }

    /// The same square, hollow, with a door in the middle of its south wall.
    fn hollow_square() -> Footprint {
        Footprint::with_doors(
            vec![
                Vec2::new(-5.0, -5.0),
                Vec2::new(5.0, -5.0),
                Vec2::new(5.0, 5.0),
                Vec2::new(-5.0, 5.0),
            ],
            &[Vec2::new(0.0, -5.0)],
            true,
        )
        .unwrap()
    }

    fn colliders_with(square: Footprint) -> BuildingColliders {
        let mut c = BuildingColliders::default();
        c.set_detail(vec![square]);
        c
    }

    #[test]
    fn contains_inside_and_outside() {
        let sq = unit_square();
        assert!(sq.contains(Vec2::ZERO));
        assert!(sq.contains(Vec2::new(4.9, -4.9)));
        assert!(!sq.contains(Vec2::new(5.1, 0.0)));
        assert!(!sq.contains(Vec2::new(0.0, 100.0)));
    }

    #[test]
    fn degenerate_rings_rejected() {
        assert!(Footprint::new(vec![Vec2::ZERO, Vec2::X]).is_none());
    }

    #[test]
    fn convex_hull_recovers_square() {
        // Corners plus interior points; the hull must be the 4 corners.
        let pts = vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(10.0, 0.0),
            Vec2::new(10.0, 10.0),
            Vec2::new(0.0, 10.0),
            Vec2::new(5.0, 5.0),
            Vec2::new(3.0, 7.0),
        ];
        let hull = convex_hull_xz(pts);
        assert_eq!(hull.len(), 4, "hull should drop the two interior points");
    }

    #[test]
    fn slide_blocks_head_on() {
        let c = colliders_with(unit_square());
        // Walking straight into the west wall from outside: X move is blocked,
        // and with no Z component there's nothing to slide to → stay put.
        let from = Vec2::new(-6.0, 0.0);
        let to = Vec2::new(-4.0, 0.0); // would land inside
        assert_eq!(c.slide(from, to), from);
    }

    #[test]
    fn slide_glides_along_wall() {
        let c = colliders_with(unit_square());
        // Approaching the west wall diagonally: the X component is blocked but the
        // Z component is free, so the avatar slides north along the facade.
        let from = Vec2::new(-6.0, 0.0);
        let to = Vec2::new(-4.0, 2.0);
        let got = c.slide(from, to);
        assert_eq!(got, Vec2::new(-6.0, 2.0));
    }

    #[test]
    fn slide_clear_path_unchanged() {
        let c = colliders_with(unit_square());
        let from = Vec2::new(-20.0, -20.0);
        let to = Vec2::new(-18.0, -19.0);
        assert_eq!(c.slide(from, to), to);
    }

    #[test]
    fn slide_from_inside_is_not_trapped() {
        let c = colliders_with(unit_square());
        // If we somehow start inside, movement is unrestricted (the ejector fixes
        // the final position) rather than freezing the avatar in place.
        let from = Vec2::ZERO;
        let to = Vec2::new(1.0, 1.0);
        assert_eq!(c.slide(from, to), to);
    }

    #[test]
    fn eject_pushes_outside_with_margin() {
        let c = colliders_with(unit_square());
        // Just inside the east wall (nearest boundary is x=5).
        let inside = Vec2::new(4.0, 0.0);
        let out = c.eject(inside, EJECT_MARGIN);
        assert!(
            !c.blocked(out),
            "ejected point must be outside every building"
        );
        // Nearest wall is +X, so it should exit eastward past x = 5 + margin.
        assert!(out.x >= 5.0 + EJECT_MARGIN - 1e-3, "got {out:?}");
        assert!(
            (out.y - 0.0).abs() < 1e-3,
            "should not drift along the wall: {out:?}"
        );
    }

    #[test]
    fn steer_stops_head_on() {
        let c = colliders_with(unit_square());
        // Walking straight east into the west wall from just outside the probe:
        // velocity is zeroed (nothing to slide to).
        let pos = Vec2::new(-5.0 - COLLIDE_PROBE_M * 0.5, 0.0);
        let v = c.steer_velocity(pos, Vec2::new(1.0, 0.0));
        assert!(v.length() < 1e-6, "expected zeroed velocity, got {v:?}");
    }

    #[test]
    fn steer_slides_along_wall() {
        let c = colliders_with(unit_square());
        // Pushing diagonally into the west wall: the blocked east component drops,
        // the free north component survives at full speed.
        let pos = Vec2::new(-5.0 - COLLIDE_PROBE_M * 0.5, 0.0);
        let wish = Vec2::new(1.0, 1.0);
        let v = c.steer_velocity(pos, wish);
        assert!(
            (v.length() - wish.length()).abs() < 1e-4,
            "speed preserved: {v:?}"
        );
        assert!(
            v.y > 0.5 && v.x.abs() < 1e-4,
            "should slide north only: {v:?}"
        );
    }

    #[test]
    fn steer_pushes_out_from_inside() {
        let c = colliders_with(unit_square());
        // Inside, near the east wall: pushed east regardless of (zero) input.
        let v = c.steer_velocity(Vec2::new(4.0, 0.0), Vec2::ZERO);
        assert!(v.x > 0.5 && v.y.abs() < 1e-4, "should push east out: {v:?}");
    }

    #[test]
    fn steer_clear_path_unchanged() {
        let c = colliders_with(unit_square());
        let wish = Vec2::new(1.0, 0.0);
        assert_eq!(c.steer_velocity(Vec2::new(-50.0, -50.0), wish), wish);
    }

    /// The bug this whole split exists for: the doorway let the avatar into a
    /// 1.1 m disc and the floor past it was still a no-go region, so the next
    /// step was refused in both axes and the avatar stood in its own doorway.
    #[test]
    fn a_hollow_building_can_be_walked_around_inside() {
        let c = colliders_with(hollow_square());
        // In through the door, then on across the room.
        let door_step = c.slide(Vec2::new(0.0, -6.0), Vec2::new(0.0, -4.0));
        assert_eq!(door_step, Vec2::new(0.0, -4.0), "the door must open");
        let across = c.slide(Vec2::new(0.0, -4.0), Vec2::new(3.0, 3.0));
        assert_eq!(across, Vec2::new(3.0, 3.0), "the room must be walkable");
        // And back out the way we came.
        assert_eq!(
            c.slide(Vec2::new(0.0, -4.0), Vec2::new(0.0, -6.0)),
            Vec2::new(0.0, -6.0)
        );
    }

    /// Only the walls. From either side of them.
    #[test]
    fn a_hollow_buildings_walls_still_stop_the_avatar() {
        let c = colliders_with(hollow_square());
        // Outside, walking at a blank wall (the door is on the south side).
        let outside = Vec2::new(-6.0, 0.0);
        assert_eq!(c.slide(outside, Vec2::new(-4.0, 0.0)), outside);
        // Inside, walking at the same wall from the room.
        let inside = Vec2::new(-4.0, 0.0);
        assert_eq!(c.slide(inside, Vec2::new(-6.0, 0.0)), inside);
    }

    /// A wall has no thickness, so a point test would step over it. Crossing the
    /// whole building in one move must be refused at the far wall even though the
    /// near one it entered by was a doorway.
    #[test]
    fn a_long_step_cannot_tunnel_through_the_far_wall() {
        let c = colliders_with(hollow_square());
        let from = Vec2::new(0.0, -6.0);
        assert_eq!(
            c.slide(from, Vec2::new(0.0, 6.0)),
            from,
            "a step from outside to outside crosses the back wall"
        );
    }

    /// The online path probes `COLLIDE_PROBE_M` ahead, which from outside a wall
    /// lands inside the room — so a point test answers "clear" and walks the
    /// avatar through the brickwork.
    #[test]
    fn steer_stops_at_a_hollow_wall_the_probe_overshoots() {
        let c = colliders_with(hollow_square());
        let pos = Vec2::new(-5.0 - COLLIDE_PROBE_M * 0.5, 0.0);
        let v = c.steer_velocity(pos, Vec2::new(1.0, 0.0));
        assert!(v.length() < 1e-6, "expected zeroed velocity, got {v:?}");
    }

    /// Being indoors is not being stuck in something, so nothing pushes the
    /// avatar back out — neither the offline backstop nor the online auto-walk.
    #[test]
    fn nothing_ejects_an_avatar_that_is_simply_indoors() {
        let c = colliders_with(hollow_square());
        assert_eq!(c.eject(Vec2::ZERO, EJECT_MARGIN), Vec2::ZERO);
        assert_eq!(c.eject_dir(Vec2::ZERO), None);
        // And a walk inside is steered by nothing at all.
        let wish = Vec2::new(1.0, 0.0);
        assert_eq!(c.steer_velocity(Vec2::ZERO, wish), wish);
    }

    /// A building with no room behind the door stays solid, and so does one with
    /// no door: walking into a bare shell is a hole in the world, not a room.
    #[test]
    fn a_solid_building_is_unchanged_by_any_of_this() {
        let c = colliders_with(unit_square());
        let from = Vec2::new(0.0, -6.0);
        assert_eq!(c.slide(from, Vec2::new(0.0, -4.0)), from, "still no way in");
        let out = c.eject(Vec2::ZERO, EJECT_MARGIN);
        assert!(!c.blocked(out), "still ejected from inside");
    }

    #[test]
    fn eject_noop_when_outside() {
        let c = colliders_with(unit_square());
        let outside = Vec2::new(50.0, 50.0);
        assert_eq!(c.eject(outside, EJECT_MARGIN), outside);
    }
}
