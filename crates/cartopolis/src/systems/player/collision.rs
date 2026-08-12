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
//! projected vertices, since only a triangle soup survives there). Interiors
//! aren't modelled, so the avatar is kept outside the footprint entirely.

use crate::components::Avatar;
use crate::grid::WORLD_CELL_EDGE;
use crate::resources::AvatarState;
use bevy::prelude::*;
use big_space::prelude::GridCell;
use std::collections::HashMap;

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
}

impl Footprint {
    /// Build a footprint from a ring of world-XZ points (≥ 3), or `None` if the
    /// ring is degenerate.
    pub fn new(ring: Vec<Vec2>) -> Option<Self> {
        if ring.len() < 3 {
            return None;
        }
        let mut min = ring[0];
        let mut max = ring[0];
        for p in &ring[1..] {
            min = min.min(*p);
            max = max.max(*p);
        }
        Some(Self { ring, min, max })
    }

    fn aabb_contains(&self, p: Vec2) -> bool {
        p.x >= self.min.x && p.x <= self.max.x && p.y >= self.min.y && p.y <= self.max.y
    }

    /// Even-odd ray cast: true when `p` is strictly inside the ring.
    fn contains(&self, p: Vec2) -> bool {
        if !self.aabb_contains(p) {
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

    pub fn is_empty(&self) -> bool {
        (self.detail_off || self.detail.is_empty())
            && (self.lod22_off || self.lod22.is_empty())
            && self.props.is_empty()
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
    }

    /// True if `p` (world XZ) lies inside any footprint or prop.
    fn blocked(&self, p: Vec2) -> bool {
        self.iter().any(|f| f.contains(p)) || self.in_prop(p)
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
    /// The avatar is treated as a point; buildings are solid.
    pub fn slide(&self, from: Vec2, to: Vec2) -> Vec2 {
        // Already inside (just teleported / a building streamed over us)? Don't
        // trap the avatar — let it move; `eject` pushes it back out this frame.
        if self.blocked(from) {
            return to;
        }
        if !self.blocked(to) {
            return to;
        }
        // Axis separation: keep whichever single-axis component stays clear.
        let slide_x = Vec2::new(to.x, from.y);
        if !self.blocked(slide_x) {
            return slide_x;
        }
        let slide_z = Vec2::new(from.x, to.y);
        if !self.blocked(slide_z) {
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
    /// client-side prediction for the authoritative (solo/split) movement path,
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
    if pts.len() < 3 {
        return pts;
    }
    pts.sort_by(|a, b| {
        a.x.partial_cmp(&b.x)
            .unwrap()
            .then(a.y.partial_cmp(&b.y).unwrap())
    });
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
    let ground_y =
        crate::systems::avatar::GROUND_HEIGHT + terrain.at(tf.translation.x, tf.translation.z);
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

    #[test]
    fn eject_noop_when_outside() {
        let c = colliders_with(unit_square());
        let outside = Vec2::new(50.0, 50.0);
        assert_eq!(c.eject(outside, EJECT_MARGIN), outside);
    }
}
