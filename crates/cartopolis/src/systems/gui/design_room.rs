//! The design room: the map's ring offers **Design here**, and the world fades
//! to a white room around that spot, where a group of objects is built at scale
//! and kept in the account's inventory as one design
//! (`docs/mockups/design-room/README.md`, `docs/notes/design-room.md`).
//!
//! # The world is taken away by render layer, not by visibility
//!
//! Twenty-odd systems write the visibility of what stands in a street —
//! buildings, tiles, trees, furniture, traffic, crows — and each is the *only*
//! writer of its own, because two writers on one entity race
//! (`globe::update_globe_visibility`'s note). A room that hid them would be a
//! twenty-first writer of every one. So nothing is hidden: while the room is
//! open the camera draws only [`DESIGN_LAYER`], and what belongs in the room
//! is put on it — the room's floor, grid and walls, every prim, and the sun
//! (bevy draws a directional light only into a view whose layers it shares,
//! and casts shadows only from entities on its own layers, so a facade left
//! on layer 0 casts no shadow onto the white floor either).
//!
//! # What a part is
//!
//! A part is any object standing within [`PARTS_RADIUS_M`] of the spot; the
//! server keeps only the caller's own when a design is saved
//! (`simulator::designs`). The objects are ordinary prims, built with the
//! ordinary editor, and others see them appear as they are built — the room
//! is a way of looking, not a draft held apart from the world.

use std::sync::{Arc, Mutex};

use bevy::camera::visibility::RenderLayers;
use bevy::math::DVec3;
use bevy::prelude::*;
use bevy::tasks::IoTaskPool;
use big_space::prelude::{BigSpace, Grid, GridCell};
use serde::Deserialize;

use crate::components::{FreeCamera, Prim, PrimGeo};
use crate::grid::{WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use crate::resources::{ContextMenuState, EditDialogState, GameState, MapAnchor};
use crate::systems::egui_manager::EguiMut;
use crate::systems::model_assets;
use crate::systems::{icons, theme};

/// The render layer the room is drawn on. Layer 0 is everything else.
pub const DESIGN_LAYER: usize = 7;

/// Radius of the white room around the spot, in metres: walls and floor.
const ROOM_RADIUS_M: f32 = 40.0;

/// How far from the spot an object counts as part of the design, in metres.
/// The mockup's bound on a design is twenty metres across.
pub const PARTS_RADIUS_M: f64 = 10.0;

/// Half the side of the metre grid drawn on the floor.
const GRID_HALF_M: i32 = 40;

/// Where the grid starts to dissolve into the white, as a fraction of
/// [`GRID_HALF_M`]: crisp across the room, gone before the walls.
const GRID_FADE_FROM: f32 = 0.35;

/// The room's white, in linear light.
const ROOM_WHITE: f32 = 1.6;

/// How far up the camera arrives, and at what pitch it looks down at the spot.
const ARRIVE_ALTITUDE_M: f64 = 7.0;
const ARRIVE_PITCH_RAD: f32 = 0.42;

/// One design in the inventory, as `GET /api/inventory/designs` lists it.
#[derive(Debug, Clone, Deserialize)]
pub struct DesignItem {
    pub id: i64,
    pub name: String,
    pub parts: i64,
}

enum Reply {
    Designs(Vec<DesignItem>),
    /// A sentence for the bar, and a nudge to fetch the list again.
    Done(String),
    Failed(String),
}

type Inbox = Arc<Mutex<Vec<Reply>>>;

/// Whether the room is open, where, and the bar's state.
#[derive(Resource, Default)]
pub struct DesignRoom {
    /// The spot the room stands around; `None` while it is closed.
    pub spot: Option<(f64, f64)>,
    /// What the design is called, as typed in the bar.
    pub name: String,
    pub designs: Vec<DesignItem>,
    /// The last thing that happened, shown in the bar.
    pub status: String,
    pub busy: bool,
    /// The prims within [`PARTS_RADIUS_M`] of the spot, counted each frame.
    pub parts: Vec<i64>,
    /// Those of [`Self::parts`] that are still drafts: only this account sees
    /// them, until the bar's Place puts them in the street.
    pub drafts: Vec<i64>,
    /// The parts as the outliner lists them: name and group chain, by id.
    pub tree: Vec<PartRow>,
    /// What is ticked in the outliner, for Group.
    pub picked: std::collections::BTreeSet<i64>,
    /// The name a new group gets, as typed in the outliner.
    pub group_name: String,
    /// The person paused the agent from the room: its building tools answer
    /// that it is paused until they resume it (`mcp::agent`).
    pub ai_paused: bool,
    /// Counts each way in and out; each one whites the screen out and fades
    /// it back ([`draw_veil`]). The second is the one the veil has started.
    flash: u32,
    flash_started: u32,
    loaded: bool,
    inbox: Inbox,
}

impl DesignRoom {
    /// Open the room around a spot, without moving the camera.
    pub fn open_at(&mut self, lat: f64, lng: f64) {
        self.spot = Some((lat, lng));
        self.flash += 1;
        self.status.clear();
        self.loaded = false;
    }

    /// Close the room. What was built in it stays standing.
    pub fn close(&mut self) {
        self.spot = None;
        self.parts.clear();
        self.drafts.clear();
        self.tree.clear();
        self.picked.clear();
        self.flash += 1;
    }
}

/// One part in the outliner.
#[derive(Debug, Clone, PartialEq)]
pub struct PartRow {
    pub id: i64,
    pub name: String,
    pub groups: Vec<cartopolis_core::GroupLevel>,
    pub draft: bool,
}

/// The groups a prim is in, outermost first (`PrimDto::groups`, protocol 51),
/// kept beside the prim by `network` for the outliner.
#[derive(Component, Debug, Clone, Default)]
pub struct Groups(pub Vec<cartopolis_core::GroupLevel>);

/// The room's own pieces: floor, grid and walls.
#[derive(Component)]
pub struct RoomPiece;

/// Metres between two points a few metres apart, on the sphere `geo_nav` uses.
#[must_use]
pub fn metres_between(a: (f64, f64), b: (f64, f64)) -> f64 {
    const M_PER_DEG: f64 = 6_378_137.0 * std::f64::consts::PI / 180.0;
    let north = (b.0 - a.0) * M_PER_DEG;
    let east = (b.1 - a.1) * M_PER_DEG * a.0.to_radians().cos();
    north.hypot(east)
}

fn push(inbox: &Inbox, reply: Reply) {
    if let Ok(mut queue) = inbox.lock() {
        queue.push(reply);
    }
}

fn fetch_designs(inbox: Inbox) {
    IoTaskPool::get()
        .spawn(async move {
            let reply = match model_assets::api_get("/api/inventory/designs").await {
                Ok(body) => match serde_json::from_str::<Vec<DesignItem>>(&body) {
                    Ok(items) => Reply::Designs(items),
                    Err(e) => Reply::Failed(format!("could not read your objects: {e}")),
                },
                Err(e) => Reply::Failed(e),
            };
            push(&inbox, reply);
        })
        .detach();
}

fn save_design(inbox: Inbox, name: String, spot: (f64, f64), prim_ids: Vec<i64>) {
    let body = serde_json::json!({
        "name": name,
        "latitude": spot.0,
        "longitude": spot.1,
        "prim_ids": prim_ids,
    })
    .to_string();
    IoTaskPool::get()
        .spawn(async move {
            let reply = match model_assets::api_post("/api/inventory/designs", Some(body)).await {
                Ok(_) => Reply::Done(format!("Kept \u{201c}{name}\u{201d} in your objects.")),
                Err(e) => Reply::Failed(e),
            };
            push(&inbox, reply);
        })
        .detach();
}

fn place_design(inbox: Inbox, id: i64, name: String, spot: (f64, f64)) {
    // Into the room as drafts, like everything else built there.
    let body =
        serde_json::json!({ "latitude": spot.0, "longitude": spot.1, "draft": true }).to_string();
    IoTaskPool::get()
        .spawn(async move {
            let path = format!("/api/inventory/designs/{id}/place");
            let reply = match model_assets::api_post(&path, Some(body)).await {
                Ok(_) => Reply::Done(format!(
                    "Put a copy of \u{201c}{name}\u{201d} here, as a draft."
                )),
                Err(e) => Reply::Failed(e),
            };
            push(&inbox, reply);
        })
        .detach();
}

fn place_drafts(inbox: Inbox, prim_ids: Vec<i64>) {
    let count = prim_ids.len();
    let body = serde_json::json!({ "prim_ids": prim_ids }).to_string();
    IoTaskPool::get()
        .spawn(async move {
            let reply = match model_assets::api_post("/api/inventory/drafts/place", Some(body))
                .await
            {
                Ok(_) => Reply::Done(match count {
                    1 => "Placed: it stands in the street for everybody now.".to_string(),
                    n => format!("Placed {n} objects: they stand in the street for everybody now."),
                }),
                Err(e) => Reply::Failed(e),
            };
            push(&inbox, reply);
        })
        .detach();
}

fn make_group(inbox: Inbox, name: String, prim_ids: Vec<i64>) {
    let body = serde_json::json!({ "name": name, "prim_ids": prim_ids }).to_string();
    IoTaskPool::get()
        .spawn(async move {
            let reply = match model_assets::api_post("/api/inventory/groups", Some(body)).await {
                Ok(_) => Reply::Done(format!("Grouped as \u{201c}{name}\u{201d}.")),
                Err(e) => Reply::Failed(e),
            };
            push(&inbox, reply);
        })
        .detach();
}

fn ungroup(inbox: Inbox, id: i64, name: String) {
    IoTaskPool::get()
        .spawn(async move {
            let path = format!("/api/inventory/groups/{id}");
            let reply = match model_assets::api_delete(&path).await {
                Ok(()) => Reply::Done(format!("Ungrouped \u{201c}{name}\u{201d}.")),
                Err(e) => Reply::Failed(e),
            };
            push(&inbox, reply);
        })
        .detach();
}

fn forget_design(inbox: Inbox, id: i64, name: String) {
    IoTaskPool::get()
        .spawn(async move {
            let path = format!("/api/inventory/designs/{id}");
            let reply = match model_assets::api_delete(&path).await {
                Ok(()) => Reply::Done(format!("Removed \u{201c}{name}\u{201d} from your objects.")),
                Err(e) => Reply::Failed(e),
            };
            push(&inbox, reply);
        })
        .detach();
}

/// Open the room at a spot and bring the camera there: the ring's Design here,
/// and the agent's `design_room` `open`, one way in.
pub fn enter(
    room: &mut DesignRoom,
    nav: &mut crate::systems::geo_nav::GeoNavState,
    lat: f64,
    lng: f64,
) {
    use crate::systems::geo_nav::{CameraPose, Teleport};
    room.open_at(lat, lng);
    room.name.clear();
    // Earth mode, orbiting the spot from a passer-by's distance: the
    // mockup's Orbit view, and the one camera that turns round a point.
    nav.pending_teleport = Some(Teleport {
        lat,
        lng,
        altitude: Some(ARRIVE_ALTITUDE_M),
        pose: Some(CameraPose {
            mode: "earth".to_string(),
            orbit_pitch: ARRIVE_PITCH_RAD,
            orbit_yaw: 0.0,
            orbit_distance: 0.0, // apply_teleport derives it from the altitude
            ..Default::default()
        }),
        silent: true,
        look: true,
    });
}

/// Take the ring's Design here, bring the camera to the spot, count the parts
/// and drain finished requests.
pub fn drive_design_room(
    mut room: ResMut<DesignRoom>,
    mut menu: ResMut<ContextMenuState>,
    mut nav: ResMut<crate::systems::geo_nav::GeoNavState>,
    online: Option<Res<crate::resources::OnlineSession>>,
    prims: Query<(&Prim, &PrimGeo, Has<Draft>, Option<&Groups>)>,
) {
    if let Some((lat, lng)) = menu.design_here.take() {
        enter(&mut room, &mut nav, lat, lng);
    }
    let Some(spot) = room.spot else {
        return;
    };
    let mut tree: Vec<PartRow> = prims
        .iter()
        .filter(|(_, g, ..)| metres_between(spot, (g.latitude, g.longitude)) <= PARTS_RADIUS_M)
        .map(|(p, _, draft, groups)| PartRow {
            id: p.id,
            name: p.name.clone(),
            groups: groups.map(|g| g.0.clone()).unwrap_or_default(),
            draft,
        })
        .collect();
    // Grouped parts together, in their groups' order, then by name.
    tree.sort_by(|a, b| {
        let key = |r: &PartRow| r.groups.iter().map(|g| g.name.clone()).collect::<Vec<_>>();
        key(a)
            .cmp(&key(b))
            .then(a.name.cmp(&b.name))
            .then(a.id.cmp(&b.id))
    });
    let parts: Vec<i64> = tree.iter().map(|r| r.id).collect();
    let drafts: Vec<i64> = tree.iter().filter(|r| r.draft).map(|r| r.id).collect();
    if tree != room.tree {
        let ids: std::collections::BTreeSet<i64> = parts.iter().copied().collect();
        room.picked.retain(|id| ids.contains(id));
        room.tree = tree;
    }
    if parts != room.parts {
        room.parts = parts;
    }
    if drafts != room.drafts {
        room.drafts = drafts;
    }
    if online.is_some() && model_assets::have_endpoint() && !room.loaded {
        room.loaded = true;
        room.busy = true;
        fetch_designs(room.inbox.clone());
    }
    let replies: Vec<Reply> = match room.inbox.lock() {
        Ok(mut queue) => queue.drain(..).collect(),
        Err(_) => Vec::new(),
    };
    if replies.is_empty() {
        return;
    }
    room.busy = false;
    let mut refresh = false;
    for reply in replies {
        match reply {
            Reply::Designs(items) => room.designs = items,
            Reply::Done(message) => {
                room.status = message;
                refresh = true;
            }
            Reply::Failed(message) => room.status = message,
        }
    }
    if refresh {
        room.busy = true;
        fetch_designs(room.inbox.clone());
    }
}

/// Put the room's pieces, the camera, the sun and the prims on the room's
/// layer while it is open, and take them off when it closes.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn layer_design_room(
    mut commands: Commands,
    room: Res<DesignRoom>,
    map_anchor: Res<MapAnchor>,
    terrain: Res<crate::systems::terrain::TerrainField>,
    bigspace: Query<Entity, With<BigSpace>>,
    camera: Query<(Entity, Has<RenderLayers>), With<FreeCamera>>,
    lights: Query<(Entity, Has<RenderLayers>), With<DirectionalLight>>,
    mut pieces: Query<(Entity, &mut Transform, &mut GridCell), With<RoomPiece>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let (Some((lat, lng)), Some(anchor)) = (room.spot, map_anchor.geo) else {
        // Closed: the camera and the lights back on layer 0, the room gone.
        // Prims keep their layers ([`layer_prims`]), which layer 0 alone draws.
        for (entity, has) in camera.iter().chain(lights.iter()) {
            if has {
                commands.entity(entity).try_remove::<RenderLayers>();
            }
        }
        for (entity, ..) in &pieces {
            commands.entity(entity).try_despawn();
        }
        return;
    };

    for (entity, has) in &camera {
        if !has {
            commands
                .entity(entity)
                .try_insert(RenderLayers::layer(DESIGN_LAYER));
        }
    }
    for (entity, has) in &lights {
        if !has {
            commands
                .entity(entity)
                .try_insert(RenderLayers::layer(DESIGN_LAYER));
        }
    }
    // Where the room stands: the spot in the anchored frame, on the ground.
    let (wx, wz) = crate::systems::geo_nav::lat_lng_to_world(anchor, lat, lng);
    let y = f64::from(terrain.ground_at(wx as f32, wz as f32));
    let grid = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD);
    let (cell, local) = grid.translation_to_grid(DVec3::new(wx, y, wz));

    if !pieces.is_empty() {
        for (_, mut transform, mut at) in &mut pieces {
            if transform.translation != local {
                transform.translation = local;
            }
            if *at != cell {
                *at = cell;
            }
        }
        return;
    }
    let Ok(root) = bigspace.single() else {
        return;
    };
    let layer = RenderLayers::layer(DESIGN_LAYER);
    // One white for floor, grid and walls, unlit, so there is no horizon where
    // the floor meets the walls and no tint from the time of day: a lit floor
    // took the sun's colour (peach at mid-morning) against grey walls. Above
    // 1.0 because the tonemapper takes 1.0 to a light grey.
    let white = Color::linear_rgb(ROOM_WHITE, ROOM_WHITE, ROOM_WHITE);
    let floor = materials.add(StandardMaterial {
        base_color: white,
        unlit: true,
        fog_enabled: false,
        ..default()
    });
    let lines = materials.add(StandardMaterial {
        base_color: white,
        unlit: true,
        fog_enabled: false,
        ..default()
    });
    let walls = materials.add(StandardMaterial {
        base_color: white,
        unlit: true,
        fog_enabled: false,
        cull_mode: None,
        ..default()
    });
    let piece =
        |commands: &mut Commands, mesh: Handle<Mesh>, material: Handle<StandardMaterial>| {
            commands.spawn((
                RoomPiece,
                Mesh3d(mesh),
                MeshMaterial3d(material),
                Transform::from_translation(local),
                cell,
                ChildOf(root),
                layer.clone(),
                bevy::light::NotShadowCaster,
            ));
        };
    piece(
        &mut commands,
        meshes.add(
            Plane3d::new(Vec3::Y, Vec2::splat(ROOM_RADIUS_M))
                .mesh()
                .build(),
        ),
        floor,
    );
    piece(&mut commands, meshes.add(grid_mesh()), lines);
    piece(
        &mut commands,
        meshes.add(Sphere::new(ROOM_RADIUS_M).mesh().uv(48, 24)),
        walls,
    );
}

/// A soft dark patch on the floor under a part that stands on it.
#[derive(Component)]
pub struct ContactShadow;

/// How far above the floor a part's underside may be and still get a shadow.
const CONTACT_GAP_M: f32 = 0.3;

/// Ground the parts on the room's floor with a contact shadow each.
///
/// The floor is unlit, so it takes no shadow from the sun: lit, it took the
/// sun's colour too (peach at mid-morning against white walls), and a white
/// room should not change with the time of day. A real second light for the
/// room was ruled out by `directional-lights-share-one-cascade-config.md`.
/// So each part of the room whose underside is within [`CONTACT_GAP_M`] of the floor gets
/// a feathered patch the size of its footprint, as a child — it turns and
/// scales with the part — drawn on the room's layer alone.
#[allow(clippy::type_complexity)]
pub fn shade_design_room(
    mut commands: Commands,
    room: Res<DesignRoom>,
    map_anchor: Res<MapAnchor>,
    terrain: Res<crate::systems::terrain::TerrainField>,
    prims: Query<(
        Entity,
        &Prim,
        &Transform,
        Option<&GridCell>,
        &crate::components::PrimLift,
    )>,
    shadows: Query<(Entity, &ChildOf), With<ContactShadow>>,
    mut mesh: Local<Option<(Handle<Mesh>, Handle<StandardMaterial>)>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let (Some((lat, lng)), Some(anchor)) = (room.spot, map_anchor.geo) else {
        for (entity, _) in &shadows {
            commands.entity(entity).try_despawn();
        }
        return;
    };
    // The floor is flat at the ground under the spot; a part stands on the
    // ground under itself, which differs by centimetres even on a square.
    let (wx, wz) = crate::systems::geo_nav::lat_lng_to_world(anchor, lat, lng);
    let floor = terrain.ground_at(wx as f32, wz as f32);
    let (mesh, material) = mesh
        .get_or_insert_with(|| {
            (
                meshes.add(shadow_mesh()),
                materials.add(StandardMaterial {
                    base_color: Color::WHITE,
                    unlit: true,
                    fog_enabled: false,
                    alpha_mode: AlphaMode::Blend,
                    cull_mode: None,
                    ..default()
                }),
            )
        })
        .clone();
    let shaded: std::collections::HashSet<Entity> =
        shadows.iter().map(|(_, parent)| parent.parent()).collect();
    for (entity, prim, transform, cell, lift) in &prims {
        if !room.parts.contains(&prim.id) {
            continue;
        }
        // Height above its own ground, whether or not the terrain lift is on
        // this frame (`prim_geo`: put on late in the frame, taken off first).
        let world = crate::systems::prim_geo::world_position(cell, transform);
        let above = world.y as f32 - crate::systems::prim_geo::lift_to_strip(lift, world);
        let half = transform.scale.y.abs() / 2.0;
        let gap = above - half;
        let wants = (-CONTACT_GAP_M..=CONTACT_GAP_M).contains(&gap);
        if wants && !shaded.contains(&entity) {
            // In the part's own units: a centimetre over the room's floor,
            // against where the part is drawn (its own ground plus `above`),
            // a little wider than its footprint.
            let up = transform.scale.y.abs().max(1e-3);
            let drawn = terrain.ground_at(world.x as f32, world.z as f32) + above;
            let local_y = (floor + 0.01 - drawn) / up;
            commands.entity(entity).with_child((
                ContactShadow,
                Mesh3d(mesh.clone()),
                MeshMaterial3d(material.clone()),
                Transform::from_xyz(0.0, local_y, 0.0).with_scale(Vec3::new(1.5, 1.0, 1.5)),
                RenderLayers::layer(DESIGN_LAYER),
                bevy::light::NotShadowCaster,
            ));
        } else if !wants && shaded.contains(&entity) {
            for (shadow, parent) in &shadows {
                if parent.parent() == entity {
                    commands.entity(shadow).try_despawn();
                }
            }
        }
    }
}

/// A unit disc, dark at the centre and clear at the rim, scaled to a part's
/// footprint by its parent.
fn shadow_mesh() -> Mesh {
    use bevy::asset::RenderAssetUsages;
    use bevy::mesh::{Indices, PrimitiveTopology};
    const RIM: u32 = 32;
    // Ring radii (of the unit half-size) and their darkness.
    // Strong, because the room is brighter than 1.0 and the tonemapper
    // squeezes everything above that: 28% darker read as no change at all.
    const RINGS: [(f32, f32); 3] = [(0.0, 0.62), (0.36, 0.45), (0.5, 0.0)];
    let mut positions = vec![[0.0, 0.0, 0.0]];
    let mut colours = vec![[0.0, 0.0, 0.0, RINGS[0].1]];
    for &(r, a) in &RINGS[1..] {
        for k in 0..RIM {
            let t = k as f32 / RIM as f32 * std::f32::consts::TAU;
            // A rounded square rather than a circle: most parts are boxes.
            let (s, c) = t.sin_cos();
            let n = 4.0;
            let k = (s.abs().powf(n) + c.abs().powf(n)).powf(-1.0 / n);
            positions.push([c * k * r, 0.0, s * k * r]);
            colours.push([0.0, 0.0, 0.0, a]);
        }
    }
    let mut indices: Vec<u32> = Vec::new();
    for k in 0..RIM {
        let next = (k + 1) % RIM;
        indices.extend([0, 1 + next, 1 + k]);
        let (a, b) = (1 + k, 1 + next);
        let (c, d) = (1 + RIM + k, 1 + RIM + next);
        indices.extend([a, b, d, a, d, c]);
    }
    let normals = vec![[0.0, 1.0, 0.0]; positions.len()];
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colours)
    .with_inserted_indices(Indices::U32(indices))
}

/// A design-room draft (`PrimDto::draft`, protocol 51): only its maker is sent
/// it, and it is drawn in the room alone, never in the street, until placed.
#[derive(Component)]
pub struct Draft;

/// Every prim and what hangs off it (a model's scene, an emitter) on the
/// layers it is drawn in: a [`Draft`] in the room alone, anything else in the
/// street and the room alike. A subtree is walked only when its root is on the
/// wrong layers or its children changed, so a still world costs one compare a
/// prim.
#[allow(clippy::type_complexity)]
pub fn layer_prims(
    mut commands: Commands,
    prims: Query<(Entity, Has<Draft>, Option<Ref<Children>>), With<Prim>>,
    children: Query<&Children>,
    layers: Query<&RenderLayers>,
    own: Query<(), With<ContactShadow>>,
) {
    let room_only = RenderLayers::layer(DESIGN_LAYER);
    let both = RenderLayers::from_layers(&[0, DESIGN_LAYER]);
    for (root, draft, kids) in &prims {
        let want = if draft { &room_only } else { &both };
        let right = layers.get(root).ok() == Some(want);
        if right && !kids.is_some_and(|k| k.is_changed()) {
            continue;
        }
        let mut stack = vec![root];
        while let Some(entity) = stack.pop() {
            // A contact shadow keeps the room's layer whatever its part is.
            if own.contains(entity) {
                continue;
            }
            if layers.get(entity).ok() != Some(want) {
                commands.entity(entity).try_insert(want.clone());
            }
            if let Ok(k) = children.get(entity) {
                stack.extend(k.iter());
            }
        }
    }
}

/// Draw the editing handles (`systems::gizmo`, bevy gizmos) on both layers, so
/// a part can be moved in the room as in the street. Once, at startup.
pub fn let_gizmos_into_the_room(store: Option<ResMut<bevy::gizmos::config::GizmoConfigStore>>) {
    let Some(mut store) = store else {
        return;
    };
    let (config, _) = store.config_mut::<bevy::gizmos::config::DefaultGizmoConfigGroup>();
    config.render_layers = RenderLayers::from_layers(&[0, DESIGN_LAYER]);
}

/// The metre grid on the floor: thin quads a centimetre above it, every fifth
/// line darker, the one through the spot darker still, each fading into the
/// floor's white towards the edge so the room has no line where it ends. Cut
/// into metre segments, because a vertex colour only fades between vertices.
fn grid_mesh() -> Mesh {
    use bevy::asset::RenderAssetUsages;
    use bevy::mesh::{Indices, PrimitiveTopology};
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut colours: Vec<[f32; 4]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    let half = GRID_HALF_M as f32;
    // 1.0 is the floor itself (the lines' material is the floor's white).
    // `tint` is the line's colour at the spot; the axes have one (Blender's:
    // red X, east; blue Z, which runs south here), every other line a cool grey.
    let shade = |tint: [f32; 3], x: f32, z: f32| {
        let r = x.hypot(z) / half;
        let t = ((r - GRID_FADE_FROM) / (1.0 - GRID_FADE_FROM)).clamp(0.0, 1.0);
        let fade = t * t * (3.0 - 2.0 * t);
        let [r, g, b] = tint.map(|c| c + (1.0 - c) * fade);
        [r, g, b, 1.0]
    };
    // The two axes through the spot, in the room's white's units (1.0 is the
    // floor): dark enough to read against a white the tonemapper squeezes.
    const X_AXIS: [f32; 3] = [0.62, 0.12, 0.14];
    const Z_AXIS: [f32; 3] = [0.14, 0.3, 0.62];
    for i in -GRID_HALF_M..=GRID_HALF_M {
        let (width, grey) = match i {
            0 => (0.03, 0.25),
            _ if i % 5 == 0 => (0.022, 0.42),
            _ => (0.012, 0.6),
        };
        let at = i as f32;
        let w = width / 2.0;
        for s in -GRID_HALF_M..GRID_HALF_M {
            let (a, b) = (s as f32, s as f32 + 1.0);
            // One segment along x (the X axis when `i` is 0) and one along z.
            for (quad, axis) in [
                [[a, at - w], [b, at - w], [b, at + w], [a, at + w]],
                [[at - w, a], [at + w, a], [at + w, b], [at - w, b]],
            ]
            .into_iter()
            .zip([X_AXIS, Z_AXIS])
            {
                let tint = if i == 0 {
                    axis
                } else {
                    [grey * 0.96, grey * 0.98, grey]
                };
                let base = positions.len() as u32;
                for [x, z] in quad {
                    positions.push([x, 0.01, z]);
                    colours.push(shade(tint, x, z));
                }
                indices.extend([base, base + 2, base + 1, base, base + 3, base + 2]);
            }
        }
    }
    let normals = vec![[0.0, 1.0, 0.0]; positions.len()];
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colours)
    .with_inserted_indices(Indices::U32(indices))
}

/// The room's white as the screen shows it, for the veil.
const VEIL: egui::Color32 = egui::Color32::from_gray(247);

/// The way in and the way out: the screen goes white and the new view comes
/// up through it, so the street and its controls dissolve into the room
/// rather than vanish (and the camera's jump to the spot is never seen).
///
/// A fresh animation id per way in or out: egui starts an id it has not seen
/// at its target, so the first frame asks for `true` (fully white) and every
/// later one for `false`, which eases it out over [`motion::ambient_time`] —
/// nothing under reduced motion.
fn draw_veil(ctx: &egui::Context, room: &mut DesignRoom) {
    use crate::systems::motion;
    if room.flash == 0 {
        return;
    }
    let id = egui::Id::new("design_room_veil").with(room.flash);
    let first = room.flash_started != room.flash;
    room.flash_started = room.flash;
    let alpha = motion::toggle(ctx, id, first, motion::ambient_time());
    if alpha <= 0.0 {
        return;
    }
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, id));
    painter.rect_filled(ctx.content_rect(), 0.0, VEIL.gamma_multiply(alpha));
}

/// The bar along the bottom while the room is open: add a part, keep the
/// design, place one kept earlier, leave.
#[allow(clippy::too_many_lines)]
pub fn render_design_room_bar(
    egui: EguiMut,
    mut room: ResMut<DesignRoom>,
    mut edit_dialog: ResMut<EditDialogState>,
    mut game_state: ResMut<GameState>,
    map_anchor: Res<MapAnchor>,
    online: Option<Res<crate::resources::OnlineSession>>,
    selected: Query<SelectedPart, With<crate::components::Selected>>,
) {
    draw_veil(egui.ctx(), &mut room);
    let Some(spot) = room.spot else {
        return;
    };
    // The selected part and its size, always on screen: v4's "a grid you can
    // read sizes off" for the part in hand.
    let chosen = selected.single().ok();
    let mut duplicate = false;
    let signed_in = online.is_some() && model_assets::have_endpoint();
    let bottom = crate::systems::dock::reserved_bottom() + 8.0;
    let width = 420.0_f32.min(egui.viewport_pts().x - 2.0 * theme::OSD_MARGIN);

    let mut name = std::mem::take(&mut room.name);
    let parts = room.parts.len();
    let drafts = room.drafts.len();
    let mut place_all = false;
    let designs = room.designs.clone();
    let status = room.status.clone();
    let busy = room.busy;
    let mut add_part = false;
    let mut save = false;
    let mut place: Option<(i64, String)> = None;
    let mut forget: Option<(i64, String)> = None;

    egui::Area::new(egui::Id::new("design_room_bar"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -bottom))
        .show(egui.ctx(), |ui| {
            theme::overlay_frame().show(ui, |ui| {
                ui.set_width(width);
                ui.horizontal(|ui| {
                    // No close button here: the way out is one, top left
                    // (`render_design_room_outliner`), and Escape.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.label(
                                egui::RichText::new("Design room")
                                    .size(theme::SIZE_TITLE)
                                    .strong()
                                    .color(theme::p().accent_text),
                            );
                            let count = match parts {
                                0 => "nothing here yet".to_string(),
                                1 => "1 object".to_string(),
                                n => format!("{n} objects"),
                            };
                            ui.label(egui::RichText::new(count).color(theme::p().text_dim));
                        });
                    });
                });
                ui.add_space(6.0);
                if !signed_in {
                    ui.label(
                        egui::RichText::new("Sign in to build here and keep what you make.")
                            .color(theme::p().text_dim),
                    );
                    return;
                }
                ui.horizontal(|ui| {
                    if theme::secondary_button(ui, icons::ADD, "Add a part", 0.0).clicked() {
                        add_part = true;
                    }
                    ui.add(
                        egui::TextEdit::singleline(&mut name)
                            .hint_text("What is it?")
                            .desired_width(ui.available_width() - 70.0),
                    );
                    let ready = parts > 0 && !name.trim().is_empty() && !busy;
                    if ui
                        .add_enabled(ready, egui::Button::new("Keep"))
                        .on_hover_text("keep the objects here in your inventory, as one design")
                        .clicked()
                    {
                        save = true;
                    }
                });
                if let Some((prim, tf, ..)) = chosen {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        let s = tf.scale.abs();
                        ui.label(
                            egui::RichText::new(format!(
                                "{} · {:.2} × {:.2} × {:.2} m",
                                prim.name, s.x, s.z, s.y
                            ))
                            .color(theme::p().accent_text),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .button("Duplicate")
                                .on_hover_text("a copy a metre east of it (Shift-D)")
                                .clicked()
                            {
                                duplicate = true;
                            }
                        });
                    });
                }
                if drafts > 0 {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        let what = if drafts == 1 {
                            "1 object is a draft: only you see it.".to_string()
                        } else {
                            format!("{drafts} objects are drafts: only you see them.")
                        };
                        ui.label(egui::RichText::new(what).color(theme::p().text_dim));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add_enabled(!busy, egui::Button::new("Place in the street"))
                                .on_hover_text("put the drafts here in the world, for everybody")
                                .clicked()
                            {
                                place_all = true;
                            }
                        });
                    });
                }
                if !designs.is_empty() {
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new("Your objects")
                            .size(theme::SIZE_SUB)
                            .color(theme::p().text_dim),
                    );
                    egui::ScrollArea::vertical()
                        .max_height(120.0)
                        .show(ui, |ui| {
                            for d in &designs {
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new(&d.name).strong());
                                    let parts = if d.parts == 1 {
                                        "1 part".to_string()
                                    } else {
                                        format!("{} parts", d.parts)
                                    };
                                    ui.label(egui::RichText::new(parts).color(theme::p().text_dim));
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui
                                                .button(icons::DELETE)
                                                .on_hover_text(
                                                    "remove it from your objects; copies \
                                                     already placed stay",
                                                )
                                                .clicked()
                                            {
                                                forget = Some((d.id, d.name.clone()));
                                            }
                                            if ui
                                                .add_enabled(!busy, egui::Button::new("Place here"))
                                                .on_hover_text("stand a copy of it on this spot")
                                                .clicked()
                                            {
                                                place = Some((d.id, d.name.clone()));
                                            }
                                        },
                                    );
                                });
                            }
                        });
                }
                if busy {
                    ui.label(egui::RichText::new("Working…").color(theme::p().text_dim));
                } else if !status.is_empty() {
                    ui.label(egui::RichText::new(&status).color(theme::p().text_dim));
                }
            });
        });

    room.name = name;
    if duplicate {
        if let (Some(session), Some(part)) = (online.as_ref(), chosen) {
            session.send("duplicate", duplicate_message(part));
        }
    }
    if place_all {
        room.busy = true;
        place_drafts(room.inbox.clone(), room.drafts.clone());
    }
    if add_part {
        if let Some(anchor) = map_anchor.geo {
            let (wx, wz) = crate::systems::geo_nav::lat_lng_to_world(anchor, spot.0, spot.1);
            crate::systems::ui::open_new_object(
                &mut edit_dialog,
                &mut game_state,
                [wx as f32, 0.0, wz as f32],
            );
        }
    }
    if save {
        room.busy = true;
        room.status = "Keeping…".into();
        save_design(
            room.inbox.clone(),
            room.name.trim().to_string(),
            spot,
            room.parts.clone(),
        );
    }
    if let Some((id, name)) = place {
        room.busy = true;
        place_design(room.inbox.clone(), id, name, spot);
    }
    if let Some((id, name)) = forget {
        room.busy = true;
        forget_design(room.inbox.clone(), id, name);
    }
}

/// The outliner, on the left while the room is open: the parts in their
/// groups, a tick box on each to choose what Group takes, and Ungroup on each
/// group (v4's Outliner, `docs/mockups/design-room/v4-editor.png`).
pub fn render_design_room_outliner(
    egui: EguiMut,
    mut room: ResMut<DesignRoom>,
    online: Option<Res<crate::resources::OnlineSession>>,
) {
    if room.spot.is_none() {
        return;
    }
    // The one way out, always in the same place: top left, above everything
    // the room shows (v4's first principle). Escape and Back do the same
    // (`dismiss::Rung::DesignRoom`).
    let mut leave = false;
    egui::Area::new(egui::Id::new("design_room_back"))
        .order(egui::Order::Foreground)
        .anchor(
            egui::Align2::LEFT_TOP,
            egui::vec2(theme::OSD_MARGIN, theme::OSD_MARGIN),
        )
        .show(egui.ctx(), |ui| {
            if theme::secondary_button(ui, icons::BACK, "Back to the map", 0.0)
                .on_hover_text("Esc · drafts wait here for you, placed objects stay standing")
                .clicked()
            {
                leave = true;
            }
        });
    if leave {
        room.close();
        return;
    }
    if room.tree.is_empty() {
        return;
    }
    let signed_in = online.is_some() && model_assets::have_endpoint();
    let tree = room.tree.clone();
    let mut picked = std::mem::take(&mut room.picked);
    let mut group_name = std::mem::take(&mut room.group_name);
    let busy = room.busy;
    let mut make: Option<(String, Vec<i64>)> = None;
    let mut dissolve: Option<(i64, String)> = None;
    let width = 270.0_f32.min(egui.viewport_pts().x * 0.45);

    egui::Area::new(egui::Id::new("design_room_outliner"))
        .order(egui::Order::Foreground)
        .anchor(
            egui::Align2::LEFT_TOP,
            // Under the way out.
            egui::vec2(theme::OSD_MARGIN, theme::OSD_MARGIN + 56.0),
        )
        .show(egui.ctx(), |ui| {
            theme::overlay_frame().show(ui, |ui| {
                ui.set_width(width);
                ui.label(
                    egui::RichText::new("Outliner")
                        .size(theme::SIZE_TITLE)
                        .strong()
                        .color(theme::p().accent_text),
                );
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .max_height(320.0)
                    .show(ui, |ui| {
                        let mut previous: &[cartopolis_core::GroupLevel] = &[];
                        for row in &tree {
                            let shared = row
                                .groups
                                .iter()
                                .zip(previous)
                                .take_while(|(a, b)| a.id == b.id)
                                .count();
                            // A header for each group this row enters.
                            for (depth, g) in row.groups.iter().enumerate().skip(shared) {
                                let members: Vec<i64> = tree
                                    .iter()
                                    .filter(|r| r.groups.iter().any(|x| x.id == g.id))
                                    .map(|r| r.id)
                                    .collect();
                                ui.horizontal(|ui| {
                                    ui.add_space(depth as f32 * 14.0);
                                    let mut all = members.iter().all(|id| picked.contains(id));
                                    if ui.checkbox(&mut all, "").changed() {
                                        for id in &members {
                                            if all {
                                                picked.insert(*id);
                                            } else {
                                                picked.remove(id);
                                            }
                                        }
                                    }
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "{} {}",
                                            icons::LAYERS,
                                            g.name
                                        ))
                                        .strong()
                                        .color(theme::p().accent_text),
                                    );
                                    ui.label(
                                        egui::RichText::new(members.len().to_string())
                                            .small()
                                            .color(theme::p().text_dim),
                                    );
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if signed_in
                                                && ui
                                                    .add_enabled(
                                                        !busy,
                                                        egui::Button::new("Ungroup"),
                                                    )
                                                    .on_hover_text(
                                                        "dissolve this group; its parts stay in \
                                                     the groups around it",
                                                    )
                                                    .clicked()
                                            {
                                                dissolve = Some((g.id, g.name.clone()));
                                            }
                                        },
                                    );
                                });
                            }
                            ui.horizontal(|ui| {
                                ui.add_space(row.groups.len() as f32 * 14.0);
                                let mut on = picked.contains(&row.id);
                                if ui.checkbox(&mut on, &row.name).changed() {
                                    if on {
                                        picked.insert(row.id);
                                    } else {
                                        picked.remove(&row.id);
                                    }
                                }
                                if row.draft {
                                    ui.label(
                                        egui::RichText::new("draft")
                                            .small()
                                            .color(theme::p().text_dim),
                                    );
                                }
                            });
                            previous = &row.groups;
                        }
                    });
                if signed_in {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut group_name)
                                .hint_text("Group name")
                                .desired_width(ui.available_width() - 70.0),
                        );
                        let ready = picked.len() >= 1 && !group_name.trim().is_empty() && !busy;
                        if ui
                            .add_enabled(ready, egui::Button::new("Group"))
                            .on_hover_text("put the ticked parts in a new group (Ctrl-G)")
                            .clicked()
                        {
                            make = Some((
                                group_name.trim().to_string(),
                                picked.iter().copied().collect(),
                            ));
                        }
                    });
                }
            });
        });

    if let Some((name, ids)) = make {
        room.busy = true;
        make_group(room.inbox.clone(), name, ids);
        picked.clear();
        group_name.clear();
    }
    if let Some((id, name)) = dissolve {
        room.busy = true;
        ungroup(room.inbox.clone(), id, name);
    }
    room.picked = picked;
    room.group_name = group_name;
}

/// How far east of the original a duplicate stands, in metres: beside it, and
/// on the grid, so a row of copies lines up.
const DUPLICATE_OFFSET_M: f64 = 1.0;

/// The selected part, as the bar and Shift-D read it.
type SelectedPart<'a> = (
    &'a Prim,
    &'a Transform,
    Option<&'a GridCell>,
    &'a PrimGeo,
    &'a crate::components::PrimLift,
);

/// A copy of a part a metre east of it, as a draft: Shift-D (Blender's), and
/// the bar's Duplicate. A copy is the object without its script, video and
/// sound, as an agent's re-made one is: a client holds none of their source.
fn duplicate_message(part: SelectedPart) -> cartopolis_core::NetMessage {
    const M_PER_DEG: f64 = 6_378_137.0 * std::f64::consts::PI / 180.0;
    let (prim, tf, cell, geo, lift) = part;
    let world = crate::systems::prim_geo::world_position(cell, tf);
    // Its height over its own ground, with or without the terrain lift on.
    let above = world.y as f32 - crate::systems::prim_geo::lift_to_strip(lift, world);
    let (rx, ry, rz) = tf.rotation.to_euler(EulerRot::XYZ);
    let srgb = prim.color.to_srgba();
    cartopolis_core::NetMessage::CreatePrim {
        request_id: 0,
        name: prim.name.clone(),
        position: Vec3::new(0.0, above, 0.0),
        rotation: Vec3::new(rx, ry, rz),
        scale: tf.scale,
        color: [srgb.red, srgb.green, srgb.blue],
        texture_id: prim.texture_id.clone(),
        shape: format!("{:?}", prim.shape).to_lowercase(),
        surface: prim.surface,
        geometry: prim.geometry(),
        geo: cartopolis_core::GeoAnchor {
            latitude: geo.latitude,
            longitude: geo.longitude
                + DUPLICATE_OFFSET_M / (M_PER_DEG * geo.latitude.to_radians().cos()),
        },
        particles: prim.particles,
        model: prim.model.clone(),
        draft: true,
    }
}

/// Shift-D in the design room: duplicate the selected part.
pub fn design_room_keys(
    keyboard: Res<ButtonInput<KeyCode>>,
    egui: crate::systems::egui_manager::EguiRef,
    room: Res<DesignRoom>,
    online: Option<Res<crate::resources::OnlineSession>>,
    selected: Query<SelectedPart, With<crate::components::Selected>>,
) {
    if room.spot.is_none() || egui.ui_owns_keyboard(false, &keyboard) {
        return;
    }
    let shift = keyboard.pressed(KeyCode::ShiftLeft) || keyboard.pressed(KeyCode::ShiftRight);
    if !(shift && keyboard.just_pressed(KeyCode::KeyD)) {
        return;
    }
    if let (Some(session), Ok(part)) = (online.as_ref(), selected.single()) {
        session.send("duplicate", duplicate_message(part));
    }
}

/// How long after its last call the agent still counts as at work in the room.
#[cfg(mcp)]
const AI_ACTIVE_SECS: f64 = 15.0;

/// While an agent works in the room (an MCP client, or a session on the Midas
/// page through the device link): a bar at the top saying what it is doing,
/// with Pause. Paused, its building tools answer that the person paused it
/// (`mcp::agent`), so it waits and the person steers — in the room, or in the
/// session's chat. Fed by the same activity log the desktop's editor overlay
/// draws (`McpBridge::log`), so every tool shows here with no extra wiring.
#[cfg(mcp)]
pub fn render_design_room_ai(
    egui: EguiMut,
    mut room: ResMut<DesignRoom>,
    bridge: Option<Res<crate::systems::mcp::McpBridge>>,
    real: Res<Time<bevy::time::Real>>,
) {
    use crate::systems::mcp::Outcome;
    if room.spot.is_none() {
        return;
    }
    let now = real.elapsed_secs_f64();
    let latest = bridge.as_ref().and_then(|b| {
        b.log
            .iter()
            .filter(|e| e.outcome == Outcome::Working || now - e.at <= AI_ACTIVE_SECS)
            .max_by(|a, b| a.at.total_cmp(&b.at))
            .cloned()
    });
    if latest.is_none() && !room.ai_paused {
        return;
    }
    let mut paused = room.ai_paused;
    egui::Area::new(egui::Id::new("design_room_ai"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, theme::OSD_MARGIN))
        .show(egui.ctx(), |ui| {
            theme::overlay_frame().show(ui, |ui| {
                ui.horizontal(|ui| {
                    let line = if paused {
                        "Paused: the AI waits for you".to_string()
                    } else {
                        match &latest {
                            Some(e) if e.outcome == Outcome::Working => {
                                format!("The AI is building · {} {}", e.label, e.detail)
                            }
                            Some(e) => format!("The AI · {}: {}", e.label, e.detail),
                            None => String::new(),
                        }
                    };
                    let line: String = line.chars().take(80).collect();
                    ui.label(egui::RichText::new(line).color(theme::p().accent_text));
                    let (icon, label, tip) = if paused {
                        (icons::PLAY, "Resume", "let the AI carry on building")
                    } else {
                        (
                            icons::STOP,
                            "Pause",
                            "hold the AI's next change until you resume it",
                        )
                    };
                    if theme::secondary_button(ui, icon, label, 0.0)
                        .on_hover_text(tip)
                        .clicked()
                    {
                        paused = !paused;
                    }
                });
            });
        });
    if paused != room.ai_paused {
        room.ai_paused = paused;
    }
}

/// The objects of a market stall, as `(name, shape, colour, east, up, north,
/// size)` in metres from the spot: what `--ui designroom` stands in the room,
/// so a capture shows the room with something designed in it. Offline there is
/// no server to build on, so the shot seeds these as the gizmo shot seeds its
/// box — negative ids, never sent anywhere.
pub const DEMO_STALL: &[(&str, &str, [f32; 3], [f32; 3], [f32; 3])] = &[
    (
        "Counter",
        "box",
        [0.55, 0.38, 0.22],
        [0.0, 0.5, 0.0],
        [2.7, 1.0, 0.9],
    ),
    (
        "Counter top",
        "box",
        [0.80, 0.62, 0.40],
        [0.0, 1.04, 0.05],
        [2.9, 0.08, 1.1],
    ),
    (
        "Post",
        "box",
        [0.30, 0.22, 0.15],
        [-1.35, 1.2, -0.5],
        [0.1, 2.4, 0.1],
    ),
    (
        "Post",
        "box",
        [0.30, 0.22, 0.15],
        [1.35, 1.2, -0.5],
        [0.1, 2.4, 0.1],
    ),
    (
        "Post",
        "box",
        [0.30, 0.22, 0.15],
        [-1.35, 1.2, 0.5],
        [0.1, 2.4, 0.1],
    ),
    (
        "Post",
        "box",
        [0.30, 0.22, 0.15],
        [1.35, 1.2, 0.5],
        [0.1, 2.4, 0.1],
    ),
    (
        "Awning",
        "box",
        [0.80, 0.16, 0.14],
        [0.0, 2.45, 0.0],
        [3.0, 0.08, 1.6],
    ),
    (
        "Lantern",
        "box",
        [0.95, 0.80, 0.35],
        [0.6, 1.9, -0.75],
        [0.25, 0.3, 0.25],
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_part_ten_metres_off_is_ten_metres_off() {
        let spot = (53.2194, 6.5665);
        let north = (spot.0 + 10.0 / 111_319.49, spot.1);
        assert!((metres_between(spot, north) - 10.0).abs() < 0.01);
        let east = (
            spot.0,
            spot.1 + 10.0 / (111_319.49 * spot.0.to_radians().cos()),
        );
        assert!((metres_between(spot, east) - 10.0).abs() < 0.01);
    }
}
