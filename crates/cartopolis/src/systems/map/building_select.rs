//! Picking a building, and the glass shell that answers.
//!
//! Three things the city could not do until this existed: say *which* building
//! you mean, show you what the registers know about it, and let you see an avatar
//! that has walked inside one. They are one module because they are one question
//! — "which building is this?" — asked by a cursor, by the card, and by the
//! avatar's own feet.
//!
//! **The pick is a ray against footprints, not against the mesh.** A LoD2.2 cell
//! is thousands of buildings merged into a handful of meshes, so there is no
//! per-building entity to hit and no way to make one that does not cost the
//! upload budget this lane is already the largest consumer of. What there *is* is
//! a footprint per building — the register's own outline, already streamed for
//! the collider — and a shell height, already measured
//! ([`lod22::ShellMeasure`]). A ray against that extruded prism is exact enough
//! that nobody can tell, because the prism is the building's own outline and its
//! own ridge height; the only case it gets wrong is a ray that passes through the
//! notch of a roof overhang, which is centimetres.
//!
//! It is brute force over the loaded facts on the frame of a click, which is the
//! same stance `picking::prim_picking` takes and for the same reason: tens of
//! thousands of AABB rejections once, against a hit test per frame forever.
//!
//! **The highlight and the ghost are the same prism drawn twice over.** Selecting
//! a building lays glass over its shell; walking into one takes the shell's walls
//! *out* of the opaque pass (`building_windows.wgsl`'s `in_ghost`) and puts glass
//! where they were. Both are one translucent mesh built from the outline, and
//! that is deliberate: a mesh cut to the shell's real triangles would have to be
//! recovered from a merged buffer, and a fragment tint on the shell itself cannot
//! be translucent at all while the whole lane shares one opaque material.
//!
//! **Which building the avatar is in is a rendering question here, and only
//! that.** [`track_indoors`] asks `interiors::InteriorFloors` and the answer
//! picks the building to ghost. It has nothing to do with getting *through* the
//! door: `collision::Footprint` calls a building with a room in it **hollow** and
//! stops the avatar at its walls rather than at its area, so walking in — and the
//! follow camera's boom coming in after it — already works without anyone naming
//! the building.

use std::sync::Arc;

use bevy::prelude::*;
use big_space::prelude::{BigSpace, Grid, GridCell};

use crate::components::{FreeCamera, HideInSpace};
use crate::grid::{WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use crate::resources::{AvatarState, MapAnchor};
use crate::systems::building_facts::BuildingFacts;
use crate::systems::building_windows::{BuildingMaterial, GHOST_MAX_POINTS, GHOST_RING_VEC4S};
use crate::systems::egui_manager::EguiManager;
use crate::systems::geo_nav::{lat_lng_to_world, world_to_lat_lng};
use crate::systems::interiors::InteriorFloors;
use crate::systems::lod22::{Lod22Buildings, ShellMeasure};
use crate::systems::map_stream::MapStream;
use crate::systems::terrain::TerrainField;

/// How far a click will reach for a building, metres.
///
/// Past this a facade is a few pixels of a distant block and a click on it is not
/// a choice anybody made. It is generous next to `data_layers`' 500 m POI reach
/// because a building is a large target and picking one across a square is an
/// ordinary thing to want.
const MAX_PICK_M: f32 = 900.0;

/// How far the pick will consider candidates either side of the ray's ground
/// crossing, metres. The broad phase for [`pick_building`]: a ray from a camera
/// can only hit a building whose outline is within this of the line it traces
/// over the ground, and a building is tens of metres across.
const PICK_CORRIDOR_M: f64 = 120.0;

/// How far outside its own wall the highlight's glass stands, metres.
///
/// Enough to win the depth fight with the shell it is laid over, and small enough
/// that it is not a second building. The shell and this prism are built from the
/// *same* ring, so at zero they are coplanar down every facade in the selection —
/// the exact failure `interiors::WALL_INSET_M` exists to avoid indoors.
const HIGHLIGHT_INFLATE_M: f32 = 0.08;

/// And how far above the ridge its cap sits, for the same reason.
const HIGHLIGHT_LIFT_M: f32 = 0.10;

/// How far outside the outline the *discard* test reaches, metres.
///
/// The shell is 3DBAG's soup and the outline is the BAG's ring, simplified at
/// 10 cm; they are derived from the same footprint but they are not the same
/// numbers, so a wall fragment can sit a few centimetres outside the ring that is
/// supposed to remove it. What that leaves is a bright opaque sliver down the edge
/// of an otherwise transparent building, which reads as a rendering fault.
///
/// Kept small on purpose. A terrace shares its party wall with the neighbour, and
/// this reaches into that neighbour by the same amount — under the simplification
/// tolerance it is a centimetres-deep nick in a wall nobody is looking at; a
/// generous value would take a visible bite out of the house next door.
const GHOST_GROW_M: f32 = 0.15;

/// The selected building's glass, as linear RGBA.
///
/// The brand accent at the alpha the UI's own overlays sit at, so a selected
/// building reads as part of the same surface as the card describing it rather
/// than as a render-debug colour.
const HIGHLIGHT_COLOR: Color = Color::srgba(0.99, 0.38, 0.07, 0.34);

/// The ghosted building's glass. Cool, pale and fainter than the highlight: this
/// one is a wall you are meant to see *through*, not a thing being pointed at.
const GHOST_COLOR: Color = Color::srgba(0.72, 0.82, 0.88, 0.22);

/// Marker for the two glass shells, so a re-anchor teardown can find them.
#[derive(Component)]
pub struct BuildingShell;

/// What is selected, what is being stood in, and what was measured about each.
#[derive(Resource, Default)]
pub struct BuildingSelection {
    /// The building the user picked, if any. Cleared by a click on empty world,
    /// by the card's close button, and by a re-anchor.
    pub selected: Option<Arc<str>>,
    /// The building the avatar is standing in, from
    /// [`InteriorFloors::building_at`].
    pub indoors: Option<Arc<str>>,
    /// The two glass entities, so they can be replaced rather than accumulated.
    highlight: Option<Entity>,
    ghost: Option<Entity>,
    /// What the last built shells were built from, so a stationary selection does
    /// not rebuild a mesh every frame. `(id, anchor generation, terrain revision)`
    /// — the last two because a re-anchor moves the ring and the terrain moves the
    /// base it stands on.
    built: (Option<Arc<str>>, Option<Arc<str>>, u32, u32),
    material: Option<Handle<StandardMaterial>>,
    ghost_material: Option<Handle<StandardMaterial>>,
}

impl BuildingSelection {
    /// Is a building selected? For `--dump-state` and the card.
    pub fn is_selected(&self) -> bool {
        self.selected.is_some()
    }

    /// Is the avatar standing inside a building? For `--dump-state`.
    pub fn is_indoors(&self) -> bool {
        self.indoors.is_some()
    }

    /// Forget the selection — the card's ✕, and the re-anchor teardown.
    pub fn clear(&mut self) {
        self.selected = None;
    }
}

/// Everything one building's prism is built from, resolved once.
struct Prism {
    /// The outline in anchored world XZ.
    ring: Vec<Vec2>,
    /// Ground level under it, metres.
    base: f32,
    /// Height above the ground, metres.
    top: f32,
}

/// Resolve a building's outline and height into the anchored world frame.
///
/// `None` when the register has no outline for it (fewer than three points) — the
/// convex hull `lod22` falls back to for the *collider* deliberately does not
/// reach here, because a hull drawn as glass fills in the inner corner of every
/// L-shaped building with a pane nobody can explain.
fn prism(
    id: &str,
    facts: &BuildingFacts,
    lod: &Lod22Buildings,
    ground: &TerrainField,
    geo: (f64, f64, u32),
    height: fn(ShellMeasure) -> f32,
) -> Option<Prism> {
    let fact = facts.building(id)?;
    if fact.footprint.len() < 3 {
        return None;
    }
    let ring: Vec<Vec2> = fact
        .footprint
        .iter()
        .map(|&(la, lo)| {
            let (x, z) = lat_lng_to_world(geo, la, lo);
            Vec2::new(x as f32, z as f32)
        })
        .collect();
    // One height for the whole building, the way every other consumer of the
    // terrain treats one: a building's vertices are heights above its own base,
    // so sampling the field per vertex racks the walls out of plumb
    // (`docs/notes/terrain-relief.md`).
    let base = ground.level_over(ring.iter().map(|p| [p.x, p.y]));
    // A shell that has not landed yet has no measured height. A nominal storey is
    // the honest stand-in — the same answer `interiors` gives — and it is replaced
    // the moment the cell arrives, because the rebuild key carries the id.
    let top = lod.shell_measure(id).map_or(3.2, height);
    Some(Prism { ring, base, top })
}

// ─── Picking ─────────────────────────────────────────────────────────────────

/// Left-click a building to select it; click empty ground to deselect. **On
/// touch it takes two taps**, and a single one leaves the selection alone.
///
/// Runs **after** `picking::prim_picking` and stands down when that took the
/// click: a prim is a thing somebody placed and a building is the scenery behind
/// it, so a click that resolves to both is the prim's. Shift is likewise left
/// alone — it is the prim marquee's modifier.
///
/// **Why a finger is asked twice.** The emulated mouse presses `Left` the
/// instant a finger lands (`systems::touch`), because a tap has to be a click
/// somewhere — but so does the first frame of a pan, of a pinch and of a
/// long press, and every one of those was opening the card over the map and
/// laying glass on a building nobody had chosen. A cursor has a hover and a
/// still hand to say "this one"; a finger has neither, so the second tap is
/// what says it. The same reasoning makes a *single* tap inert rather than a
/// deselect: a card dismissed by the first frame of a pan is the same accident
/// the other way round, and the card has a ✕.
pub fn pick_building(
    mouse: Res<ButtonInput<MouseButton>>,
    keyboard: Res<ButtonInput<KeyCode>>,
    touch: Res<crate::systems::touch::TouchGestures>,
    egui: Res<EguiManager>,
    mut selection: ResMut<BuildingSelection>,
    facts: Res<BuildingFacts>,
    lod: Res<Lod22Buildings>,
    anchor: Res<MapAnchor>,
    layers: Res<crate::systems::data_layers::DataLayers>,
    prim_click: Res<crate::systems::picking::PrimClick>,
    windows: Query<&Window>,
    cameras: Query<(&Camera, &GlobalTransform, &GridCell), With<FreeCamera>>,
) {
    if !mouse.just_pressed(MouseButton::Left) || keyboard.pressed(KeyCode::ShiftLeft) {
        return;
    }
    // A finger's first tap is not an answer — see the doc above.
    if touch.touch_click() && !touch.double_tap() {
        return;
    }
    // The click belongs to egui, or to the prim it landed on — see
    // `picking::PrimClick`, and note this system's `.after(prim_picking)` edge,
    // which is what makes that answer about *this* press.
    if egui.ctx.egui_wants_pointer_input() || prim_click.took() {
        return;
    }
    // Collision matches what is drawn and so does selection: with the 3DBAG layer
    // off there are no buildings on screen to have clicked.
    if !layers.buildings_enabled {
        return;
    }
    let Some(geo) = anchor.geo else {
        return;
    };
    let Some(cursor) = windows.iter().next().and_then(|w| w.cursor_position()) else {
        return;
    };
    let Ok((camera, cam_gt, cam_cell)) = cameras.single() else {
        return;
    };
    let Ok(ray) = camera.viewport_to_world(cam_gt, cursor) else {
        return;
    };
    // Render space → the anchored world frame the outlines live in.
    let offset = cell_offset(cam_cell);
    let origin = ray.origin + offset;
    let dir = *ray.direction;

    selection.selected = nearest_building_along(&facts, &lod, geo, origin, dir);
}

/// The offset between render space and the anchored world frame: the floating
/// origin's grid cell, in metres.
///
/// Every `big_space` entity's `GlobalTransform` is its anchored position minus
/// this, which is the jump `building_windows.wgsl`'s lattice note is about.
fn cell_offset(cell: &GridCell) -> Vec3 {
    Vec3::new(cell.x as f32, cell.y as f32, cell.z as f32) * WORLD_CELL_EDGE
}

/// The first building the ray enters, or `None`.
///
/// Both arguments are in the anchored world frame. The broad phase is the ray's
/// crossing of the ground plane: a building the ray can hit stands within
/// [`PICK_CORRIDOR_M`] of the line it traces there, which takes tens of thousands
/// of candidates down to a handful before any prism is tested.
fn nearest_building_along(
    facts: &BuildingFacts,
    lod: &Lod22Buildings,
    geo: (f64, f64, u32),
    origin: Vec3,
    dir: Vec3,
) -> Option<Arc<str>> {
    // Where to look. A ray aimed at the horizon never meets the ground, so it
    // searches the corridor ahead of the camera instead of nowhere at all.
    let ground_t = if dir.y < -1e-4 {
        (-origin.y / dir.y).min(MAX_PICK_M)
    } else {
        MAX_PICK_M
    };
    let mid = origin + dir * (ground_t * 0.5);
    let (lat, lng) = world_to_lat_lng(geo, f64::from(mid.x), f64::from(mid.z));
    let reach = f64::from(ground_t) * 0.5 + PICK_CORRIDOR_M;

    let mut best: Option<(f32, Arc<str>)> = None;
    for (id, fact) in facts.near(lat, lng, reach) {
        if fact.footprint.len() < 3 {
            continue;
        }
        let ring: Vec<Vec2> = fact
            .footprint
            .iter()
            .map(|&(la, lo)| {
                let (x, z) = lat_lng_to_world(geo, la, lo);
                Vec2::new(x as f32, z as f32)
            })
            .collect();
        // The ridge, not the eaves: a click on a pitched roof is a click on the
        // building under it.
        let top = lod.shell_measure(id).map_or(3.2, |m| m.top_m);
        let Some(t) = ray_prism(origin, dir, &ring, 0.0, top) else {
            continue;
        };
        if t > MAX_PICK_M {
            continue;
        }
        if best.as_ref().is_none_or(|(b, _)| t < *b) {
            best = Some((t, Arc::from(id)));
        }
    }
    best.map(|(_, id)| id)
}

/// Distance along the ray at which it first enters the prism `ring × [base,
/// top]`, or `None`.
///
/// The y slab and the polygon are solved separately and intersected, which is
/// what makes this exact for a prism rather than approximate for a box: the slab
/// gives an interval in `t`, every ring edge the ray crosses gives another
/// crossing in `t`, and the answer is the first point inside both. A ray that
/// starts inside answers 0 — the camera is in the building, which is what the
/// follow camera does indoors, and a click there means this building.
fn ray_prism(origin: Vec3, dir: Vec3, ring: &[Vec2], base: f32, top: f32) -> Option<f32> {
    // The y slab.
    let (mut lo, mut hi) = (0.0f32, f32::MAX);
    if dir.y.abs() < 1e-6 {
        if origin.y < base || origin.y > top {
            return None;
        }
    } else {
        let a = (base - origin.y) / dir.y;
        let b = (top - origin.y) / dir.y;
        lo = lo.max(a.min(b));
        hi = hi.min(a.max(b));
        if lo > hi {
            return None;
        }
    }
    // The polygon, as the interval between the ray's first and last crossing of
    // its edges. Even-odd in `t`: the crossings alternate in/out along the line,
    // so with the ray's own start classified, the first entry is the first
    // crossing at or after `lo` when the start is outside, and `lo` itself when it
    // is inside.
    let o = Vec2::new(origin.x, origin.z);
    let d = Vec2::new(dir.x, dir.z);
    if d.length_squared() < 1e-12 {
        return point_in_ring(ring, o).then_some(lo);
    }
    let mut crossings: Vec<f32> = Vec::new();
    let n = ring.len();
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        let e = b - a;
        let denom = d.x * e.y - d.y * e.x;
        if denom.abs() < 1e-9 {
            continue;
        }
        let ap = a - o;
        let t = (ap.x * e.y - ap.y * e.x) / denom;
        let u = (ap.x * d.y - ap.y * d.x) / denom;
        if t >= 0.0 && (0.0..=1.0).contains(&u) {
            crossings.push(t);
        }
    }
    crossings.sort_by(f32::total_cmp);
    // The interval of `t` over which the ray is inside the polygon. Starting
    // inside means the first crossing is the way *out*; starting outside means the
    // crossings pair up and the first pair is the span. An unpaired crossing (the
    // ray leaves through a vertex, or grazes an edge that was skipped as parallel)
    // is treated as open-ended rather than as a miss — the slab below closes it.
    let (poly_in, poly_out) = if point_in_ring(ring, o) {
        (0.0, crossings.first().copied().unwrap_or(f32::MAX))
    } else if crossings.is_empty() {
        return None;
    } else {
        (crossings[0], crossings.get(1).copied().unwrap_or(f32::MAX))
    };
    // Inside the prism is inside both. Intersect, and the first point of what is
    // left is the hit.
    let enter = lo.max(poly_in);
    let exit = hi.min(poly_out);
    (enter <= exit).then_some(enter)
}

/// Even-odd point-in-ring, in the same shape `collision::Footprint::contains`
/// uses. Kept local because that one is private and is about collision.
fn point_in_ring(ring: &[Vec2], p: Vec2) -> bool {
    let n = ring.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (a, b) = (ring[i], ring[j]);
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

// ─── Indoors ─────────────────────────────────────────────────────────────────

/// Name the building the avatar is standing in, every frame.
///
/// Cheap on purpose: [`InteriorFloors`] holds the rooms within 70 m of the avatar
/// — a few dozen — and this is a point-in-ring over them.
///
/// The answer feeds the glass and nothing else. Collision does not need it:
/// `collision::Footprint` distinguishes a hollow building from a solid one and
/// stops the avatar at a wall rather than at an area, so nothing has to be told
/// which building anybody is in for a door to work.
pub fn track_indoors(
    floors: Res<InteriorFloors>,
    avatar: Res<AvatarState>,
    mut selection: ResMut<BuildingSelection>,
) {
    let here = Vec2::new(avatar.position.x, avatar.position.z);
    let now = floors.building_at(here).cloned();
    if now != selection.indoors {
        selection.indoors = now;
    }
}

// ─── The glass ───────────────────────────────────────────────────────────────

/// Keep a glass shell standing over the selected building and around the one the
/// avatar is inside, and tell the building material which walls to stop drawing.
///
/// Rebuilt only when the answer changes — a selection held for a minute costs one
/// comparison a frame. The re-anchor and terrain revisions are in the key because
/// both move the ring the glass is built from, and a shell left standing over a
/// re-anchored world stands in the wrong street.
#[allow(clippy::too_many_arguments)]
pub fn update_building_shells(
    mut commands: Commands,
    mut selection: ResMut<BuildingSelection>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut building_materials: ResMut<Assets<BuildingMaterial>>,
    lod: Res<Lod22Buildings>,
    facts: Res<BuildingFacts>,
    ground: Res<TerrainField>,
    anchor: Res<MapAnchor>,
    stream: Res<MapStream>,
    cameras: Query<&GridCell, With<FreeCamera>>,
    bigspace: Query<Entity, With<BigSpace>>,
) {
    let ghost_ring = selection
        .indoors
        .clone()
        .zip(anchor.geo)
        .and_then(|(id, geo)| prism(&id, &facts, &lod, &ground, geo, |m| m.datum_m))
        .map(|p| offset_ring(&p.ring, GHOST_GROW_M));

    // The shader's ring, every frame: it is in render space and the floating
    // origin moves under it. Cheap enough to be unconditional — one material, one
    // ring — and unconditional is what keeps it from being wrong for a frame after
    // a grid crossing.
    if let Ok(cell) = cameras.single() {
        write_ghost_uniform(&mut building_materials, &lod, ghost_ring.as_deref(), cell);
    }

    let key = (
        selection.selected.clone(),
        selection.indoors.clone(),
        stream.anchor_generation,
        ground.revision,
    );
    if key == selection.built {
        return;
    }
    selection.built = key;

    let Ok(root) = bigspace.single() else {
        return;
    };
    let Some(geo) = anchor.geo else {
        return;
    };

    // Two entities rather than one mesh with two colours: they are different
    // materials (different alpha and tint) and they come and go independently.
    for (entity, id, height, slot) in [
        (
            selection.highlight.take(),
            selection.selected.clone(),
            (|m: ShellMeasure| m.top_m) as fn(ShellMeasure) -> f32,
            0usize,
        ),
        (
            selection.ghost.take(),
            selection.indoors.clone(),
            (|m: ShellMeasure| m.datum_m) as fn(ShellMeasure) -> f32,
            1usize,
        ),
    ] {
        if let Some(old) = entity {
            // `try_despawn`: a re-anchor teardown can retire it first, and a
            // command outlives the entity it names (CLAUDE.md).
            commands.entity(old).try_despawn();
        }
        let Some(id) = id else {
            continue;
        };
        // The highlight stands outside the shell it is laid over; the ghost
        // stands where the shell's walls *were*, so it must not be inflated —
        // the walls it replaces have been discarded and there is nothing left to
        // z-fight with.
        let inflate = if slot == 0 { HIGHLIGHT_INFLATE_M } else { 0.0 };
        let lift = if slot == 0 { HIGHLIGHT_LIFT_M } else { 0.0 };
        let Some(p) = prism(&id, &facts, &lod, &ground, geo, height) else {
            continue;
        };
        let ring = if inflate > 0.0 {
            offset_ring(&p.ring, inflate)
        } else {
            p.ring.clone()
        };
        let Some(mesh) = prism_mesh(&ring, p.base, p.base + p.top + lift) else {
            continue;
        };
        let handle = if slot == 0 {
            selection
                .material
                .get_or_insert_with(|| materials.add(glass(HIGHLIGHT_COLOR)))
                .clone()
        } else {
            selection
                .ghost_material
                .get_or_insert_with(|| materials.add(glass(GHOST_COLOR)))
                .clone()
        };
        let centre = ring.iter().fold(Vec2::ZERO, |a, b| a + *b) / ring.len() as f32;
        let (grid_cell, _) = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD)
            .translation_to_grid(Vec3::new(centre.x, 0.0, centre.y).as_dvec3());
        let ox = grid_cell.x as f32 * WORLD_CELL_EDGE;
        let oz = grid_cell.z as f32 * WORLD_CELL_EDGE;
        let spawned = commands
            .spawn((
                Mesh3d(meshes.add(mesh)),
                MeshMaterial3d(handle),
                Transform::from_translation(Vec3::new(-ox, 0.0, -oz)),
                grid_cell,
                ChildOf(root),
                BuildingShell,
                // Explicit, and not redundant with the one `Mesh3d` requires:
                // `globe`'s space-tier hide is a `Query<&mut Visibility, With<
                // HideInSpace>>`, so an entity that does not carry the component
                // in its own right is silently never matched and the marker below
                // does nothing at all.
                Visibility::Inherited,
                // A glass box over a city seen from orbit is a pane the size of a
                // province. Every other near-ground layer carries this.
                HideInSpace,
                // The vertices are anchored-world metres while the transform is
                // the grid cell's offset, so the derived AABB is not the volume
                // this mesh occupies and the frustum test on it answers no from
                // directly overhead. `systems::particles` turns culling off for
                // exactly this reason. One building's worth of glass is nothing
                // to draw unconditionally.
                bevy::camera::visibility::NoFrustumCulling,
            ))
            .id();
        if slot == 0 {
            selection.highlight = Some(spawned);
        } else {
            selection.ghost = Some(spawned);
        }
    }
}

/// The glass material: translucent, unlit-ish and double-sided.
///
/// **Double-sided is not a detail.** The ghost is a shell the avatar is standing
/// *inside*, so half of what has to be seen is its far wall from behind; culling
/// back faces would leave the building open on the side you are looking through.
/// Depth write is off (bevy's `Blend`), so it sorts against the world rather than
/// occluding the room it contains.
fn glass(color: Color) -> StandardMaterial {
    StandardMaterial {
        base_color: color,
        alpha_mode: AlphaMode::Blend,
        cull_mode: None,
        // Barely lit: a pane that took the sun would read as a solid wall from
        // whichever side the sun is on, which is exactly the view this exists to
        // open up.
        perceptual_roughness: 0.35,
        metallic: 0.0,
        emissive: LinearRgba::from(color) * 0.25,
        ..default()
    }
}

/// Push a closed ring outward by `by` metres.
///
/// An **edge** offset with a corner join, not a vertex push along the bisector —
/// the same rule `interiors::inset_ring` states, and for the same reason: pushing
/// vertices collapses a narrow canal house into a bow tie. Sharing that function
/// is not possible (it is private, insets rather than offsets, and is about a
/// room), so the geometry is repeated and the reason is stated twice.
fn offset_ring(ring: &[Vec2], by: f32) -> Vec<Vec2> {
    let n = ring.len();
    if n < 3 {
        return ring.to_vec();
    }
    // Outward is whichever side the ring is *not* on, found by probing rather
    // than derived from the winding — the signed-area rule is the tidy answer and
    // is wrong half the time in a frame where +z runs south (`docs/notes/
    // doors-and-rooms.md`).
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let prev = ring[(i + n - 1) % n];
        let here = ring[i];
        let next = ring[(i + 1) % n];
        let na = edge_out(ring, prev, here);
        let nb = edge_out(ring, here, next);
        let sum = na + nb;
        let push = if sum.length_squared() < 1e-6 {
            na
        } else {
            // Scale the bisector so the offset *edges* land `by` out, not the
            // vertex: a sharp corner otherwise pulls in.
            let unit = sum.normalize();
            unit / unit.dot(na).abs().max(0.35)
        };
        out.push(here + push * by);
    }
    out
}

/// The outward unit normal of the edge `a → b`, by probing which side of it lies
/// outside the ring.
fn edge_out(ring: &[Vec2], a: Vec2, b: Vec2) -> Vec2 {
    let e = b - a;
    let len = e.length();
    if len < 1e-6 {
        return Vec2::ZERO;
    }
    let n = Vec2::new(e.y, -e.x) / len;
    let mid = (a + b) * 0.5;
    if point_in_ring(ring, mid + n * 0.05) {
        -n
    } else {
        n
    }
}

/// A closed prism: walls all round, a cap on top, no floor.
///
/// No floor because the ground is already drawn under it and a pane lying in the
/// pavement is a z-fight the length of the building. Normals point outward and
/// the material draws both sides, so winding decides the shading and never
/// visibility.
fn prism_mesh(ring: &[Vec2], base: f32, top: f32) -> Option<Mesh> {
    if ring.len() < 3 || top <= base {
        return None;
    }
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    let n = ring.len();
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        let out = edge_out(ring, a, b);
        let normal = [out.x, 0.0, out.y];
        let first = positions.len() as u32;
        for (p, y) in [(a, base), (b, base), (b, top), (a, top)] {
            positions.push([p.x, y, p.y]);
            normals.push(normal);
        }
        indices.extend([first, first + 1, first + 2, first, first + 2, first + 3]);
    }
    // The cap. Earcut, like every other slab in this tree — a fan about the
    // centroid fills a concave outline in, and a Dutch building is routinely an L.
    let flat: Vec<f64> = ring
        .iter()
        .flat_map(|p| [f64::from(p.x), f64::from(p.y)])
        .collect();
    if let Ok(tris) = earcutr::earcut(&flat, &[], 2) {
        let first = positions.len() as u32;
        for p in ring {
            positions.push([p.x, top, p.y]);
            normals.push([0.0, 1.0, 0.0]);
        }
        for t in tris.chunks_exact(3) {
            indices.extend([
                first + t[0] as u32,
                first + t[1] as u32,
                first + t[2] as u32,
            ]);
        }
    }

    let mut mesh = Mesh::new(
        bevy::render::mesh::PrimitiveTopology::TriangleList,
        bevy::asset::RenderAssetUsages::RENDER_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_indices(bevy::render::mesh::Indices::U32(indices));
    Some(mesh)
}

/// Write the ghosted ring into every LoD2.2 material, in render space.
///
/// A ring longer than the shader can hold is **decimated, not truncated**: a
/// truncated ring is an open polygon and the even-odd test on one is noise — half
/// the building's walls would flicker between drawn and discarded as the camera
/// moved. Dropping every other point keeps it closed and moves the boundary by
/// centimetres.
fn write_ghost_uniform(
    materials: &mut Assets<BuildingMaterial>,
    lod: &Lod22Buildings,
    ring: Option<&[Vec2]>,
    origin: &GridCell,
) {
    let offset = cell_offset(origin);
    let mut ghost = Vec4::ZERO;
    let mut bounds = Vec4::ZERO;
    let mut packed = [Vec4::ZERO; GHOST_RING_VEC4S];
    if let Some(ring) = ring.filter(|r| r.len() >= 3) {
        let mut pts: Vec<Vec2> = ring
            .iter()
            .map(|p| Vec2::new(p.x - offset.x, p.y - offset.z))
            .collect();
        while pts.len() > GHOST_MAX_POINTS {
            pts = pts
                .iter()
                .enumerate()
                .filter(|(i, _)| i % 2 == 0)
                .map(|(_, p)| *p)
                .collect();
        }
        let (mut min, mut max) = (pts[0], pts[0]);
        for p in &pts {
            min = min.min(*p);
            max = max.max(*p);
        }
        ghost.x = pts.len() as f32;
        bounds = Vec4::new(min.x, min.y, max.x, max.y);
        for (i, p) in pts.iter().enumerate() {
            let slot = &mut packed[i / 2];
            if i % 2 == 0 {
                slot.x = p.x;
                slot.y = p.y;
            } else {
                slot.z = p.x;
                slot.w = p.y;
            }
        }
    }
    for handle in lod.material_handles() {
        let Some(mut material) = materials.get_mut(&handle) else {
            continue;
        };
        material.extension.ghost = ghost;
        material.extension.ghost_bounds = bounds;
        material.extension.ghost_ring = packed;
    }
}

/// Drop the selection and its glass when the world re-anchors under it.
///
/// The glass entities are retired by [`update_building_shells`]'s own key (the
/// anchor generation is in it), but the *selection* has to go too: a BAG id
/// survives a re-anchor perfectly well and the building it names may be a
/// thousand kilometres away.
pub fn clear_selection_on_reanchor(
    stream: Res<MapStream>,
    mut selection: ResMut<BuildingSelection>,
    mut generation: Local<u32>,
) {
    if *generation != stream.anchor_generation {
        *generation = stream.anchor_generation;
        selection.selected = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(size: f32) -> Vec<Vec2> {
        vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(size, 0.0),
            Vec2::new(size, size),
            Vec2::new(0.0, size),
        ]
    }

    #[test]
    fn a_ray_from_above_enters_the_prism_at_its_roof() {
        let ring = square(10.0);
        let t = ray_prism(Vec3::new(5.0, 50.0, 5.0), Vec3::NEG_Y, &ring, 0.0, 8.0);
        assert!((t.unwrap() - 42.0).abs() < 1e-3, "{t:?}");
    }

    #[test]
    fn a_ray_beside_the_prism_misses_it() {
        let ring = square(10.0);
        assert!(ray_prism(Vec3::new(-5.0, 4.0, 5.0), Vec3::NEG_X, &ring, 0.0, 8.0).is_none());
    }

    /// The ray descends past the building without ever crossing its outline.
    ///
    /// It is the ordinary case — a click on the street beside a building — and it
    /// crosses the *height* band of every building in the corridor, so answering
    /// it from the y slab alone selects whatever happened to be nearest. An
    /// earlier draft of `ray_prism` did exactly that.
    #[test]
    fn a_ray_that_only_crosses_the_height_band_misses() {
        let ring = square(10.0);
        let from = Vec3::new(-40.0, 30.0, 5.0);
        // Down and away: it passes the building's height band on the wrong side.
        let dir = Vec3::new(-1.0, -1.0, 0.0).normalize();
        assert!(ray_prism(from, dir, &ring, 0.0, 8.0).is_none());
    }

    /// Looking down into a building from above its eaves and in front of it: the
    /// ray is over the roof when the height band opens and only reaches the
    /// outline further along. The answer is where it crosses the *facade*, not
    /// where the band began.
    #[test]
    fn a_descending_ray_enters_at_the_facade() {
        let ring = square(10.0);
        let from = Vec3::new(-10.0, 18.0, 5.0);
        let dir = Vec3::new(1.0, -1.0, 0.0).normalize();
        // Height band opens at y = 8, i.e. 10 m along each axis → x = 0, exactly
        // the near wall, so the two constraints meet there.
        let t = ray_prism(from, dir, &ring, 0.0, 8.0).expect("hit");
        assert!((t - 10.0 * 2f32.sqrt()).abs() < 1e-3, "{t}");
    }

    #[test]
    fn a_ray_over_the_roof_misses_it() {
        let ring = square(10.0);
        // Level, above the top of the prism: the y slab never opens.
        assert!(ray_prism(Vec3::new(-5.0, 20.0, 5.0), Vec3::X, &ring, 0.0, 8.0).is_none());
    }

    #[test]
    fn a_ray_through_a_facade_enters_at_the_wall() {
        let ring = square(10.0);
        let t = ray_prism(Vec3::new(-5.0, 4.0, 5.0), Vec3::X, &ring, 0.0, 8.0);
        assert!((t.unwrap() - 5.0).abs() < 1e-3, "{t:?}");
    }

    /// The camera standing inside the building — which is where the follow camera
    /// is once the avatar walks in, and a click there means *this* building.
    #[test]
    fn a_ray_starting_inside_hits_at_zero() {
        let ring = square(10.0);
        let t = ray_prism(Vec3::new(5.0, 1.7, 5.0), Vec3::X, &ring, 0.0, 8.0);
        assert_eq!(t, Some(0.0));
    }

    /// Both windings, because a register promises neither and the outward normal
    /// derived from a signed area is the bug `docs/notes/doors-and-rooms.md`
    /// records twice.
    #[test]
    fn the_offset_ring_grows_on_either_winding() {
        for reversed in [false, true] {
            let mut ring = square(10.0);
            if reversed {
                ring.reverse();
            }
            let out = offset_ring(&ring, 0.5);
            // Every original corner is now strictly inside the offset ring.
            for p in &ring {
                assert!(
                    point_in_ring(&out, *p),
                    "corner {p:?} left outside (reversed={reversed})"
                );
            }
        }
    }

    #[test]
    fn the_prism_mesh_is_closed_walls_and_a_cap() {
        let mesh = prism_mesh(&square(10.0), 0.0, 6.0).expect("mesh");
        // Four walls of two triangles, plus a four-point cap earcut into two.
        assert_eq!(mesh.count_vertices(), 4 * 4 + 4);
        assert_eq!(
            mesh.indices().map(bevy::render::mesh::Indices::len),
            Some(30)
        );
    }

    #[test]
    fn a_degenerate_prism_builds_nothing() {
        assert!(prism_mesh(&square(10.0), 4.0, 4.0).is_none());
        assert!(prism_mesh(&[Vec2::ZERO, Vec2::X], 0.0, 3.0).is_none());
    }
}
