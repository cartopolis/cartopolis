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
use cartopolis_materials::design_floor::DesignFloorMaterial;

/// The render layer the room is drawn on. Layer 0 is everything else.
pub const DESIGN_LAYER: usize = 7;

/// Radius of the white room around the spot, in metres: walls and floor.
const ROOM_RADIUS_M: f32 = 40.0;

/// How far from the spot an object counts as part of the design, in metres.
/// The mockup's bound on a design is twenty metres across.
pub const PARTS_RADIUS_M: f64 = 10.0;

/// Where the grid starts to dissolve into the white, and where it is gone, in
/// metres from the spot: crisp across the room, gone before the walls.
const GRID_FADE_FROM_M: f32 = 14.0;
const GRID_FADE_TO_M: f32 = 36.0;

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
    /// Read only where the agent tools are (`mcp`): the web build has none.
    #[cfg_attr(not(mcp), allow(dead_code))]
    pub ai_paused: bool,
    /// The tool column's magnet, off: a handle drag in the room moves freely
    /// instead of in 10 cm and 15° steps (`gizmo::SNAP_M`).
    pub no_snap: bool,
    /// What the scripts in the room said and did, newest last (the Scripts
    /// panel's log).
    pub script_log: Vec<ScriptLogLine>,
    /// How many console lines the room has read (`ScriptEditorState::pushed`).
    script_seen: u64,
    /// Parts a script just touched, and until when they glow (real seconds).
    pub pulse: std::collections::HashMap<i64, f64>,
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

/// One line of the Scripts panel's log.
#[derive(Debug, Clone)]
pub struct ScriptLogLine {
    pub text: String,
    pub error: bool,
}

/// Most lines the Scripts panel keeps.
const MAX_SCRIPT_LOG: usize = 40;

/// How long a part glows after its script touched it, in seconds.
const PULSE_SECS: f64 = 0.8;

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
    room.open_at(lat, lng);
    room.name.clear();
    look_at_spot(nav, lat, lng);
}

/// Bring the camera to the room's spot, orbiting it.
fn look_at_spot(nav: &mut crate::systems::geo_nav::GeoNavState, lat: f64, lng: f64) {
    use crate::systems::geo_nav::{CameraPose, Teleport};
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
    camera: Res<crate::resources::CameraState>,
) {
    if let Some((lat, lng)) = menu.design_here.take() {
        enter(&mut room, &mut nav, lat, lng);
    }
    let Some(spot) = room.spot else {
        return;
    };
    // The room is looked at, not walked in: the avatar stays in the street.
    // Something that puts the camera back on it — the session's first
    // snapshot arriving just after an agent opened the room, seen 2026-11-19 —
    // would leave the person looking at the street through a hole where the
    // room is, so the room takes the camera back to its spot.
    if camera.mode == crate::resources::CameraMode::Avatar && nav.pending_teleport.is_none() {
        look_at_spot(&mut nav, spot.0, spot.1);
    }
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
    mut floors: ResMut<Assets<DesignFloorMaterial>>,
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
    // The floor and its grid, drawn per pixel (`design_floor.wesl`).
    let floor = commands
        .spawn((
            RoomPiece,
            Mesh3d(meshes.add(floor_mesh())),
            MeshMaterial3d(floors.add(DesignFloorMaterial {
                params: Vec4::new(ROOM_WHITE, GRID_FADE_FROM_M, GRID_FADE_TO_M, 0.0),
            })),
            Transform::from_translation(local),
            cell,
            ChildOf(root),
            layer.clone(),
            bevy::light::NotShadowCaster,
        ))
        .id();
    spawn_figure(&mut commands, floor, &mut meshes, &mut materials);
    piece(&mut commands, meshes.add(walls_mesh()), walls);
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

/// The walls: a sphere round the spot, white at the horizon and greying
/// upward to a light cool grey overhead, as the v4 sheet's backdrop does — a
/// uniform white gave no up and no far, and the floor's edge was lost in it.
/// Vertex colours, multiplying the walls' white.
fn walls_mesh() -> Mesh {
    let mut mesh = Sphere::new(ROOM_RADIUS_M).mesh().uv(48, 24);
    let colours: Vec<[f32; 4]> = match mesh.attribute(Mesh::ATTRIBUTE_POSITION) {
        Some(bevy::mesh::VertexAttributeValues::Float32x3(ps)) => ps
            .iter()
            .map(|[_, y, _]| {
                let up = (y / ROOM_RADIUS_M).clamp(0.0, 1.0);
                let t = up.powf(0.6);
                let [r, g, b] = [0.66_f32, 0.70, 0.75].map(|c| 1.0 + (c - 1.0) * t);
                [r, g, b, 1.0]
            })
            .collect(),
        _ => Vec::new(),
    };
    if !colours.is_empty() {
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colours);
    }
    mesh
}

/// The floor: one quad as wide as the room, UV0 in metres from the spot (x
/// east, y south), which is what `design_floor.wesl` draws its grid in.
fn floor_mesh() -> Mesh {
    use bevy::asset::RenderAssetUsages;
    use bevy::mesh::{Indices, PrimitiveTopology};
    let r = ROOM_RADIUS_M;
    let corners = [[-r, -r], [r, -r], [r, r], [-r, r]];
    let positions: Vec<[f32; 3]> = corners.iter().map(|[x, z]| [*x, 0.0, *z]).collect();
    let uvs: Vec<[f32; 2]> = corners.to_vec();
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0]; 4])
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
    .with_inserted_indices(Indices::U32(vec![0, 2, 1, 0, 3, 2]))
}

// ---------------------------------------------------------------------------
// The room's chrome
// ---------------------------------------------------------------------------

/// Draw one of the room's panels: dark glass over the white, whatever the
/// app's theme (v4's sheet, `docs/mockups/design-room/v4-editor.png`).
///
/// Dark because the room is white. The map's light chrome is drawn to sit on
/// a map; over a white floor its panels lost their edges and read as grey
/// smudges ("the UI panels are too plainly styled"), where a drawing tool's
/// dark panels frame the white as its viewport.
fn room_panel(
    ctx: &egui::Context,
    id: &str,
    anchor: egui::Align2,
    offset: egui::Vec2,
    width: Option<f32>,
    add: impl FnOnce(&mut egui::Ui),
) {
    theme::dark_here(|| {
        egui::Area::new(egui::Id::new(id))
            .order(egui::Order::Foreground)
            .anchor(anchor, offset)
            .show(ctx, |ui| {
                theme::use_dark_visuals(ui);
                theme::overlay_frame()
                    .shadow(theme::card_shadow())
                    .show(ui, |ui| {
                        if let Some(w) = width {
                            ui.set_width(w);
                        }
                        add(ui);
                    });
            });
    });
}

/// A panel's title row: its icon on a small accent-tinted square, the title,
/// and a dim note on the right.
fn title_row(ui: &mut egui::Ui, icon: &str, title: &str, note: &str) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(26.0, 26.0), egui::Sense::hover());
        ui.painter().rect_filled(
            rect,
            egui::CornerRadius::same(8),
            theme::p().accent.gamma_multiply(0.22),
        );
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            icon,
            egui::FontId::proportional(15.0),
            theme::p().accent_text,
        );
        ui.label(
            egui::RichText::new(title)
                .size(theme::SIZE_TITLE)
                .color(theme::p().cream),
        );
        if !note.is_empty() {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(egui::RichText::new(note).small().color(theme::p().text_dim));
            });
        }
    });
    ui.add_space(4.0);
    let r = ui.available_rect_before_wrap();
    ui.painter().line_segment(
        [
            egui::pos2(r.left(), r.top()),
            egui::pos2(r.right(), r.top()),
        ],
        egui::Stroke::new(1.0, theme::p().overlay_line),
    );
    ui.add_space(6.0);
}

/// A small status label on a tinted rounded plate: `draft`, `running`, `error`.
fn pill(ui: &mut egui::Ui, text: &str, colour: egui::Color32) {
    egui::Frame::NONE
        .fill(colour.gamma_multiply(0.18))
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(6, 1))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(text)
                    .size(theme::SIZE_TINY)
                    .color(colour),
            );
        });
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

    let note = match parts {
        0 => "nothing here yet".to_string(),
        1 => "1 object".to_string(),
        n => format!("{n} objects"),
    };
    room_panel(
        egui.ctx(),
        "design_room_bar",
        egui::Align2::CENTER_BOTTOM,
        egui::vec2(0.0, -bottom),
        Some(width),
        |ui| {
            // No close button here: the way out is one, top left
            // (`render_design_room_outliner`), and Escape.
            title_row(ui, icons::PALETTE, "Design room", &note);
            if !signed_in {
                ui.label(
                    egui::RichText::new("Sign in to build here and keep what you make.")
                        .color(theme::p().text_dim),
                );
                return;
            }
            ui.horizontal(|ui| {
                if theme::secondary_button(ui, icons::ADD, "Add a part", 0.0)
                    .on_hover_text("a new box on the spot, in the editor")
                    .clicked()
                {
                    add_part = true;
                }
                let can_copy = chosen.is_some();
                if ui
                    .add_enabled_ui(can_copy, |ui| {
                        theme::secondary_button(ui, icons::COPY, "Duplicate", 0.0)
                    })
                    .inner
                    .on_hover_text("a copy of the selected part, a metre east (Shift-D)")
                    .clicked()
                {
                    duplicate = true;
                }
            });
            if let Some((prim, tf, ..)) = chosen {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let s = tf.scale.abs();
                    ui.label(egui::RichText::new(&prim.name).color(theme::p().cream));
                    ui.label(
                        egui::RichText::new(format!("{:.2} × {:.2} × {:.2} m", s.x, s.z, s.y))
                            .color(theme::p().accent_text),
                    );
                });
            }
            if drafts > 0 {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    pill(ui, "draft", theme::p().warn);
                    let what = if drafts == 1 {
                        "1 object only you see".to_string()
                    } else {
                        format!("{drafts} objects only you see")
                    };
                    ui.label(egui::RichText::new(what).color(theme::p().text_dim));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled_ui(!busy, |ui| {
                                theme::primary_button_sized(ui, "Place in the street", 0.0)
                            })
                            .inner
                            .on_hover_text("put the drafts here in the world, for everybody")
                            .clicked()
                        {
                            place_all = true;
                        }
                    });
                });
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut name)
                        .hint_text("Name it to keep it")
                        .desired_width(ui.available_width() - 90.0),
                );
                let ready = parts > 0 && !name.trim().is_empty() && !busy;
                if ui
                    .add_enabled_ui(ready, |ui| {
                        theme::secondary_button(ui, icons::BOOKMARK_ADD, "Keep", 0.0)
                    })
                    .inner
                    .on_hover_text("keep the objects here in your inventory, as one design")
                    .clicked()
                {
                    save = true;
                }
            });
            if !designs.is_empty() {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("YOUR OBJECTS")
                        .size(theme::SIZE_TINY)
                        .color(theme::p().text_dim),
                );
                egui::ScrollArea::vertical()
                    .max_height(120.0)
                    .show(ui, |ui| {
                        for d in &designs {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(icons::CABINET)
                                        .color(theme::p().accent_text),
                                );
                                ui.label(egui::RichText::new(&d.name).color(theme::p().cream));
                                let parts = if d.parts == 1 {
                                    "1 part".to_string()
                                } else {
                                    format!("{} parts", d.parts)
                                };
                                ui.label(
                                    egui::RichText::new(parts)
                                        .small()
                                        .color(theme::p().text_dim),
                                );
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
                                            .on_hover_text("a copy of it on this spot, as a draft")
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
        },
    );

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
    theme::dark_here(|| {
        egui::Area::new(egui::Id::new("design_room_back"))
            .order(egui::Order::Foreground)
            .anchor(
                egui::Align2::LEFT_TOP,
                egui::vec2(theme::OSD_MARGIN, theme::OSD_MARGIN),
            )
            .show(egui.ctx(), |ui| {
                theme::use_dark_visuals(ui);
                if theme::secondary_button(ui, icons::BACK, "Back to the map", 0.0)
                    .on_hover_text("Esc · drafts wait here for you, placed objects stay standing")
                    .clicked()
                {
                    leave = true;
                }
            });
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

    let draft_count = tree.iter().filter(|r| r.draft).count();
    let note = if draft_count > 0 {
        format!("{} · {draft_count} draft", tree.len())
    } else {
        tree.len().to_string()
    };
    room_panel(
        egui.ctx(),
        "design_room_outliner",
        egui::Align2::LEFT_TOP,
        // Under the way out.
        egui::vec2(theme::OSD_MARGIN + TOOL_COLUMN_W, theme::OSD_MARGIN + 56.0),
        Some(width),
        |ui| {
            title_row(ui, icons::LAYERS, "Outliner", &note);
            egui::ScrollArea::vertical()
                .max_height(340.0)
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
                                ui.add_space(depth as f32 * 16.0);
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
                                ui.label(egui::RichText::new(icons::LAYERS).color(theme::p().warn));
                                ui.label(egui::RichText::new(&g.name).color(theme::p().cream));
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
                                                    egui::Button::new("Ungroup").small(),
                                                )
                                                .on_hover_text(
                                                    "dissolve this group; its parts stay in the \
                                             groups around it",
                                                )
                                                .clicked()
                                        {
                                            dissolve = Some((g.id, g.name.clone()));
                                        }
                                    },
                                );
                            });
                        }
                        let on = picked.contains(&row.id);
                        // A ticked row is lit, as the sheet's selected one is.
                        let fill = if on {
                            theme::p().accent.gamma_multiply(0.14)
                        } else {
                            egui::Color32::TRANSPARENT
                        };
                        egui::Frame::NONE
                            .fill(fill)
                            .corner_radius(egui::CornerRadius::same(6))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.add_space(row.groups.len() as f32 * 16.0);
                                    let mut tick = on;
                                    if ui.checkbox(&mut tick, "").changed() {
                                        if tick {
                                            picked.insert(row.id);
                                        } else {
                                            picked.remove(&row.id);
                                        }
                                    }
                                    ui.label(
                                        egui::RichText::new(icons::BUILD)
                                            .color(theme::p().text_dim),
                                    );
                                    ui.label(egui::RichText::new(&row.name).color(theme::p().text));
                                    if row.draft {
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| pill(ui, "draft", theme::p().warn),
                                        );
                                    }
                                });
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
                            .desired_width(ui.available_width() - 90.0),
                    );
                    let ready = !picked.is_empty() && !group_name.trim().is_empty() && !busy;
                    if ui
                        .add_enabled_ui(ready, |ui| {
                            theme::secondary_button(ui, icons::LAYERS, "Group", 0.0)
                        })
                        .inner
                        .on_hover_text("put the ticked parts in a new group")
                        .clicked()
                    {
                        make = Some((
                            group_name.trim().to_string(),
                            picked.iter().copied().collect(),
                        ));
                    }
                });
            }
        },
    );

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

/// A glow round a part its script just touched.
#[derive(Component)]
pub struct ScriptPulse;

/// Watch the scripts of the room's parts: what they print (the console every
/// script's output already lands in, `ScriptEditorState`) and when they change
/// their part (an upsert of a scripted part nobody has selected). Each is a
/// line in the Scripts panel's log and a moment's glow on the part, so a
/// script's work can be seen where it happens (v4's fifth principle).
///
/// What it cannot say is *which call* changed a part: the wire carries the
/// part's new state, not the call that made it. The line says the script
/// changed it; its own `log` lines say more, when it writes them.
#[allow(clippy::type_complexity)]
pub fn watch_scripts(
    mut room: ResMut<DesignRoom>,
    editor: Res<crate::systems::script_editor::ScriptEditorState>,
    real: Res<Time<bevy::time::Real>>,
    prims: Query<(Ref<Prim>, Has<crate::components::Selected>)>,
) {
    if room.spot.is_none() {
        if !room.script_log.is_empty() || !room.pulse.is_empty() {
            room.script_log.clear();
            room.pulse.clear();
        }
        room.script_seen = editor.pushed;
        return;
    }
    let now = real.elapsed_secs_f64();
    let name_of = |room: &DesignRoom, id: i64| {
        room.tree
            .iter()
            .find(|r| r.id == id)
            .map(|r| r.name.clone())
    };
    let new = editor.pushed.saturating_sub(room.script_seen) as usize;
    room.script_seen = editor.pushed;
    let start = editor.console.len().saturating_sub(new);
    for line in &editor.console[start..] {
        let Some(name) = name_of(&room, line.prim_id) else {
            continue;
        };
        let error = matches!(line.level, cartopolis_core::ScriptLevel::Error);
        room.script_log.push(ScriptLogLine {
            text: format!("{name}: {}", line.text),
            error,
        });
        room.pulse.insert(line.prim_id, now + PULSE_SECS);
    }
    for (prim, selected) in &prims {
        if !prim.scripted || selected || !prim.is_changed() || prim.is_added() {
            continue;
        }
        let Some(name) = name_of(&room, prim.id) else {
            continue;
        };
        // One line per burst: a script setting a colour every tick is one
        // glow and one line, not twenty a second.
        if room.pulse.get(&prim.id).is_none_or(|until| *until < now) {
            room.script_log.push(ScriptLogLine {
                text: format!("{name}: changed by its script"),
                error: false,
            });
        }
        room.pulse.insert(prim.id, now + PULSE_SECS);
    }
    let excess = room.script_log.len().saturating_sub(MAX_SCRIPT_LOG);
    if excess > 0 {
        room.script_log.drain(0..excess);
    }
}

/// Draw the glow round each part in [`DesignRoom::pulse`], and take it away
/// when its moment is over: a box a tenth larger than the part, green and
/// see-through, on the room's layer alone.
#[allow(clippy::too_many_arguments)]
pub fn draw_script_pulses(
    mut commands: Commands,
    mut room: ResMut<DesignRoom>,
    real: Res<Time<bevy::time::Real>>,
    prims: Query<(Entity, &Prim)>,
    pulses: Query<(Entity, &ChildOf), With<ScriptPulse>>,
    mut look: Local<Option<(Handle<Mesh>, Handle<StandardMaterial>)>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let now = real.elapsed_secs_f64();
    room.pulse.retain(|_, until| *until >= now);
    let glowing: std::collections::HashSet<Entity> =
        pulses.iter().map(|(_, parent)| parent.parent()).collect();
    for (pulse, parent) in &pulses {
        let keep = prims
            .get(parent.parent())
            .is_ok_and(|(_, p)| room.pulse.contains_key(&p.id));
        if !keep {
            commands.entity(pulse).try_despawn();
        }
    }
    if room.pulse.is_empty() {
        return;
    }
    let (mesh, material) = look
        .get_or_insert_with(|| {
            (
                meshes.add(Cuboid::from_length(1.1).mesh().build()),
                materials.add(StandardMaterial {
                    base_color: Color::srgba(0.19, 0.64, 0.42, 0.35),
                    unlit: true,
                    fog_enabled: false,
                    alpha_mode: AlphaMode::Blend,
                    ..default()
                }),
            )
        })
        .clone();
    for (entity, prim) in &prims {
        if room.pulse.contains_key(&prim.id) && !glowing.contains(&entity) {
            commands.entity(entity).with_child((
                ScriptPulse,
                Mesh3d(mesh.clone()),
                MeshMaterial3d(material.clone()),
                Transform::IDENTITY,
                RenderLayers::layer(DESIGN_LAYER),
                bevy::light::NotShadowCaster,
            ));
        }
    }
}

/// The Scripts panel, top right while the room is open: every scripted part,
/// whether its script runs or failed (and why), Edit to open it in the script
/// editor, New script for the selected part, and the log of what they did.
#[allow(clippy::too_many_arguments)]
pub fn render_design_room_scripts(
    egui: EguiMut,
    room: Res<DesignRoom>,
    mut editor: ResMut<crate::systems::script_editor::ScriptEditorState>,
    mut right_panel: ResMut<crate::resources::RightPanel>,
    prims: Query<&Prim>,
    selected: Query<&Prim, With<crate::components::Selected>>,
    online: Option<Res<crate::resources::OnlineSession>>,
) {
    if room.spot.is_none() {
        return;
    }
    let signed_in = online.is_some();
    // A right panel (the script editor itself) takes that side of the screen.
    if !right_panel.is(crate::resources::RightPanel::None) {
        return;
    }
    let scripted: Vec<(i64, String)> = room
        .tree
        .iter()
        .filter(|r| prims.iter().any(|p| p.id == r.id && p.scripted))
        .map(|r| (r.id, r.name.clone()))
        .collect();
    let chosen = selected
        .single()
        .ok()
        .filter(|p| room.parts.contains(&p.id));
    // The newest error a part's script reported, if its last word was one.
    let failing = |id: i64| {
        editor
            .console
            .iter()
            .rev()
            .find(|l| l.prim_id == id)
            .filter(|l| matches!(l.level, cartopolis_core::ScriptLevel::Error))
            .map(|l| l.text.clone())
    };
    let mut open: Option<i64> = None;
    let width = 300.0_f32.min(egui.viewport_pts().x * 0.45);
    let note = match scripted.len() {
        0 => String::new(),
        1 => "1 script".to_string(),
        n => format!("{n} scripts"),
    };
    room_panel(
        egui.ctx(),
        "design_room_scripts",
        egui::Align2::RIGHT_TOP,
        egui::vec2(-theme::OSD_MARGIN, theme::OSD_MARGIN + NAV_GIZMO_H),
        Some(width),
        |ui| {
            title_row(ui, icons::SCRIPT, "Scripts", &note);
            // Always there while the room is open, so it is found before it is
            // needed ("I cannot see the script panel"): empty, it says how a
            // script starts.
            if !signed_in {
                ui.label(
                    egui::RichText::new("Scripts run on the server: sign in to write one.")
                        .color(theme::p().text_dim),
                );
                return;
            }
            if scripted.is_empty() && chosen.is_none() {
                ui.label(
                    egui::RichText::new(
                        "No scripts here yet. Select a part, and a New script button \
                         appears here.",
                    )
                    .color(theme::p().text_dim),
                );
            }
            for (id, name) in &scripted {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(icons::SCRIPT).color(theme::p().accent_text));
                    ui.label(egui::RichText::new(name).color(theme::p().cream));
                    let glowing = room.pulse.contains_key(id);
                    let (state, colour) = match failing(*id) {
                        Some(_) => ("error", theme::p().error),
                        None if glowing => ("working", theme::p().accent_bright),
                        None => ("running", theme::p().accent_text),
                    };
                    pill(ui, state, colour);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .button("Edit")
                            .on_hover_text("open it in the script editor")
                            .clicked()
                        {
                            open = Some(*id);
                        }
                    });
                });
                if let Some(why) = failing(*id) {
                    ui.label(egui::RichText::new(why).small().color(theme::p().error));
                }
            }
            if let Some(p) = chosen.filter(|p| !p.scripted) {
                ui.add_space(4.0);
                if theme::secondary_button(
                    ui,
                    icons::ADD,
                    &format!("New script on {}", p.name),
                    0.0,
                )
                .on_hover_text("write JavaScript that runs on the selected part")
                .clicked()
                {
                    open = Some(p.id);
                }
            }
            if !room.script_log.is_empty() {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("WHAT THEY DID")
                        .size(theme::SIZE_TINY)
                        .color(theme::p().text_dim),
                );
                egui::Frame::NONE
                    .fill(theme::p().ink.gamma_multiply(0.5))
                    .corner_radius(egui::CornerRadius::same(8))
                    .inner_margin(egui::Margin::symmetric(8, 6))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        for line in room.script_log.iter().rev().take(6) {
                            let colour = if line.error {
                                theme::p().error
                            } else {
                                theme::p().text
                            };
                            ui.label(
                                egui::RichText::new(&line.text)
                                    .monospace()
                                    .size(theme::SIZE_TINY + 1.0)
                                    .color(colour),
                            );
                        }
                    });
            }
        },
    );
    if let Some(id) = open {
        editor.open(id);
        *right_panel = crate::resources::RightPanel::Script;
    }
}

// ---------------------------------------------------------------------------
// Tools, views, the figure and the measurements (v4's editor)
// ---------------------------------------------------------------------------

/// Width the tool column takes at the left edge, outliner beside it.
const TOOL_COLUMN_W: f32 = 64.0;

/// Height the navigation gizmo's panel takes at the top right; Scripts is
/// below it.
const NAV_GIZMO_H: f32 = 220.0;

/// The figure for scale: a person this tall, standing west of the spot.
const FIGURE_HEIGHT_M: f32 = 1.75;
const FIGURE_AT: Vec3 = Vec3::new(-2.6, 0.0, 0.6);

/// Marks the figure's root, for the measurement beside it.
#[derive(Component)]
pub struct ScaleFigure;

/// The tool column at the left: move, turn and size (G, R, S, the handles'
/// three modes), duplicate (Shift-D) and the snapping magnet. A key on a
/// desktop and a button on a phone, one each.
pub fn render_design_room_tools(
    egui: EguiMut,
    mut room: ResMut<DesignRoom>,
    mut gizmo: ResMut<crate::systems::gizmo::GizmoState>,
    online: Option<Res<crate::resources::OnlineSession>>,
    selected: Query<SelectedPart, With<crate::components::Selected>>,
) {
    use crate::systems::gizmo::GizmoMode;
    if room.spot.is_none() {
        return;
    }
    let chosen = selected.single().ok();
    let mut mode = gizmo.mode;
    let mut snap = !room.no_snap;
    let mut duplicate = false;
    room_panel(
        egui.ctx(),
        "design_room_tools",
        egui::Align2::LEFT_TOP,
        egui::vec2(theme::OSD_MARGIN, theme::OSD_MARGIN + 56.0),
        Some(TOOL_COLUMN_W - 26.0),
        |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            let tool = |ui: &mut egui::Ui, on: bool, key: &str, label: &str, tip: &str| {
                let fill = if on {
                    theme::p().accent
                } else {
                    theme::p().surface_raised
                };
                let ink = if on {
                    theme::p().on_accent
                } else {
                    theme::p().text
                };
                let (rect, resp) =
                    ui.allocate_exact_size(egui::vec2(38.0, 44.0), egui::Sense::click());
                let resp = theme::named(ui, resp, tip);
                ui.painter()
                    .rect_filled(rect, egui::CornerRadius::same(10), fill);
                ui.painter().text(
                    rect.center() - egui::vec2(0.0, 6.0),
                    egui::Align2::CENTER_CENTER,
                    key,
                    egui::FontId::proportional(16.0),
                    ink,
                );
                ui.painter().text(
                    rect.center() + egui::vec2(0.0, 11.0),
                    egui::Align2::CENTER_CENTER,
                    label,
                    egui::FontId::proportional(9.0),
                    ink,
                );
                resp.on_hover_text(tip).clicked()
            };
            if tool(
                ui,
                mode == GizmoMode::Translate,
                "G",
                "move",
                "move the selected part (G)",
            ) {
                mode = GizmoMode::Translate;
            }
            if tool(
                ui,
                mode == GizmoMode::Rotate,
                "R",
                "turn",
                "turn the selected part (R)",
            ) {
                mode = GizmoMode::Rotate;
            }
            if tool(
                ui,
                mode == GizmoMode::Scale,
                "S",
                "size",
                "size the selected part (S)",
            ) {
                mode = GizmoMode::Scale;
            }
            ui.separator();
            if tool(
                ui,
                false,
                "D",
                "copy",
                "a copy of the selected part, a metre east (Shift-D)",
            ) {
                duplicate = true;
            }
            if tool(
                ui,
                snap,
                icons::FOLLOW,
                "snap",
                "handles move in 10 cm and turn in 15° steps",
            ) {
                snap = !snap;
            }
        },
    );
    if mode != gizmo.mode {
        gizmo.mode = mode;
    }
    if snap == room.no_snap {
        room.no_snap = !snap;
    }
    if duplicate {
        if let (Some(session), Some(part)) = (online.as_ref(), chosen) {
            session.send("duplicate", duplicate_message(part));
        }
    }
}

/// The navigation gizmo at the top right: the three axes as the camera sees
/// them (red X east, green Y up, blue Z south), and the views to jump to —
/// Top, Front (looking north), Side (looking west) and back to the 3D view.
pub fn render_design_room_views(
    egui: EguiMut,
    room: Res<DesignRoom>,
    mut camera: ResMut<crate::resources::CameraState>,
) {
    if room.spot.is_none() {
        return;
    }
    let (yaw, pitch) = (camera.orbit_yaw, camera.orbit_pitch);
    let mut want: Option<(f32, f32)> = None;
    room_panel(
        egui.ctx(),
        "design_room_views",
        egui::Align2::RIGHT_TOP,
        egui::vec2(-theme::OSD_MARGIN, theme::OSD_MARGIN),
        Some(150.0),
        |ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(150.0, 104.0), egui::Sense::hover());
            let c = rect.center();
            let painter = ui.painter();
            painter.circle_filled(c, 48.0, theme::p().surface_raised);
            // The camera's basis, from the orbit angles (`free_camera`):
            // yaw 0 stands south of the spot looking north.
            let (sy, cy, sp, cp) = (yaw.sin(), yaw.cos(), pitch.sin(), pitch.cos());
            let right = Vec3::new(cy, 0.0, -sy);
            let forward = -Vec3::new(sy * cp, sp, cy * cp);
            let up = right.cross(forward);
            let mut axes = [
                (Vec3::X, "X", egui::Color32::from_rgb(229, 72, 77)),
                (Vec3::Y, "Y", egui::Color32::from_rgb(48, 164, 108)),
                (Vec3::Z, "Z", egui::Color32::from_rgb(62, 142, 208)),
            ];
            // The farthest first, so the nearest is drawn over it.
            axes.sort_by(|a, b| a.0.dot(forward).total_cmp(&b.0.dot(forward)).reverse());
            for (dir, name, colour) in axes {
                let tip = c + egui::vec2(dir.dot(right), -dir.dot(up)) * 38.0;
                painter.line_segment([c, tip], egui::Stroke::new(3.0, colour));
                painter.circle_filled(tip, 9.0, colour);
                painter.text(
                    tip,
                    egui::Align2::CENTER_CENTER,
                    name,
                    egui::FontId::proportional(11.0),
                    egui::Color32::WHITE,
                );
            }
            ui.horizontal_wrapped(|ui| {
                for (label, view) in [
                    ("Top", (0.0, 1.55)),
                    ("Front", (0.0, 0.12)),
                    ("Side", (std::f32::consts::FRAC_PI_2, 0.12)),
                    ("3D", (0.6, ARRIVE_PITCH_RAD)),
                ] {
                    if theme::chip(ui, label, false).clicked() {
                        want = Some(view);
                    }
                }
            });
        },
    );
    if let Some((y, p)) = want {
        camera.orbit_yaw = y;
        camera.orbit_pitch = p;
    }
}

/// The figure for scale: a grey person of [`FIGURE_HEIGHT_M`] standing west of
/// the spot, built from capsules and a sphere, on the room's layer. A child
/// of the floor, so it goes with the room.
fn spawn_figure(
    commands: &mut Commands,
    floor: Entity,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
) {
    let h = FIGURE_HEIGHT_M;
    let grey = materials.add(StandardMaterial {
        base_color: Color::srgb(0.33, 0.39, 0.42),
        unlit: true,
        fog_enabled: false,
        ..default()
    });
    let layer = RenderLayers::layer(DESIGN_LAYER);
    // (radius, length, centre, ) for the body's capsules, in metres.
    let limbs: [(f32, f32, Vec3); 5] = [
        (0.065, h * 0.42, Vec3::new(-0.09, h * 0.24, 0.0)),
        (0.065, h * 0.42, Vec3::new(0.09, h * 0.24, 0.0)),
        (0.15, h * 0.22, Vec3::new(0.0, h * 0.62, 0.0)),
        (0.05, h * 0.3, Vec3::new(-0.24, h * 0.6, 0.0)),
        (0.05, h * 0.3, Vec3::new(0.24, h * 0.6, 0.0)),
    ];
    commands.entity(floor).with_children(|parent| {
        parent
            .spawn((
                ScaleFigure,
                Transform::from_translation(FIGURE_AT),
                Visibility::default(),
                layer.clone(),
            ))
            .with_children(|body| {
                for (radius, length, centre) in limbs {
                    body.spawn((
                        Mesh3d(meshes.add(Capsule3d::new(radius, length).mesh().build())),
                        MeshMaterial3d(grey.clone()),
                        Transform::from_translation(centre),
                        layer.clone(),
                        bevy::light::NotShadowCaster,
                    ));
                }
                body.spawn((
                    Mesh3d(meshes.add(Sphere::new(0.11).mesh().build())),
                    MeshMaterial3d(grey.clone()),
                    Transform::from_xyz(0.0, h - 0.11, 0.0),
                    layer.clone(),
                    bevy::light::NotShadowCaster,
                ));
            });
    });
}

/// Measurements over the room: the figure's height beside it, and the
/// selected part's width and height along its edges — orange lines with end
/// ticks and the length on a white plate, as the v4 sheet draws them. Painted
/// under the panels (a background layer), so a panel is never covered.
#[allow(clippy::type_complexity)]
pub fn render_design_room_measures(
    egui: EguiMut,
    room: Res<DesignRoom>,
    camera: Query<(&Camera, &GlobalTransform), With<FreeCamera>>,
    figure: Query<&GlobalTransform, With<ScaleFigure>>,
    selected: Query<
        (&GlobalTransform, &Transform),
        (With<crate::components::Selected>, With<Prim>),
    >,
) {
    if room.spot.is_none() {
        return;
    }
    let Ok((cam, cam_gt)) = camera.single() else {
        return;
    };
    let to_ui = |p: Vec3| {
        cam.world_to_viewport(cam_gt, p)
            .ok()
            .map(|v| egui.window_to_ui(v))
    };
    let painter = egui.ctx().layer_painter(egui::LayerId::new(
        egui::Order::Background,
        egui::Id::new("design_room_measures"),
    ));
    let ink = egui::Color32::from_rgb(253, 96, 18);
    let measure = |a: Vec3, b: Vec3, metres: f32, painter: &egui::Painter| {
        let (Some(pa), Some(pb)) = (to_ui(a), to_ui(b)) else {
            return;
        };
        let stroke = egui::Stroke::new(1.6, ink);
        painter.line_segment([pa, pb], stroke);
        let along = (pb - pa).normalized();
        let tick = egui::vec2(-along.y, along.x) * 6.0;
        for p in [pa, pb] {
            painter.line_segment([p - tick, p + tick], stroke);
        }
        let mid = pa + (pb - pa) * 0.5;
        let text = format!("{metres:.2} m");
        let galley = painter.layout_no_wrap(text, egui::FontId::proportional(13.0), ink);
        let plate = egui::Rect::from_center_size(mid, galley.size() + egui::vec2(10.0, 4.0));
        painter.rect_filled(plate, egui::CornerRadius::same(5), egui::Color32::WHITE);
        painter.rect_stroke(
            plate,
            egui::CornerRadius::same(5),
            egui::Stroke::new(1.0, ink.gamma_multiply(0.5)),
            egui::StrokeKind::Inside,
        );
        painter.galley(plate.min + egui::vec2(5.0, 2.0), galley, ink);
    };
    if let Ok(fig) = figure.single() {
        let base = fig.translation() + Vec3::new(-0.45, 0.0, 0.0);
        measure(
            base,
            base + Vec3::Y * FIGURE_HEIGHT_M,
            FIGURE_HEIGHT_M,
            &painter,
        );
    }
    if let Ok((gt, tf)) = selected.single() {
        // The part's front-bottom edge and a vertical one, on the side that
        // faces the camera.
        let local_cam = gt.affine().inverse().transform_point3(cam_gt.translation());
        let z = if local_cam.z >= 0.0 { 0.5 } else { -0.5 };
        let x = if local_cam.x >= 0.0 { 0.5 } else { -0.5 };
        let at = |l: Vec3| gt.transform_point(l);
        let s = tf.scale.abs();
        measure(
            at(Vec3::new(-0.5, -0.5, z)),
            at(Vec3::new(0.5, -0.5, z)),
            s.x,
            &painter,
        );
        measure(
            at(Vec3::new(x, -0.5, z)),
            at(Vec3::new(x, 0.5, z)),
            s.y,
            &painter,
        );
    }
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
    room_panel(
        egui.ctx(),
        "design_room_ai",
        egui::Align2::CENTER_TOP,
        egui::vec2(0.0, theme::OSD_MARGIN),
        None,
        |ui| {
            ui.horizontal(|ui| {
                // A live dot: the agent at work, still when paused.
                let (dot, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                let colour = if paused {
                    theme::p().warn
                } else {
                    theme::p().accent_bright
                };
                ui.painter()
                    .circle_filled(dot.center(), 7.0, colour.gamma_multiply(0.25));
                ui.painter().circle_filled(dot.center(), 4.0, colour);
                let (head, rest) = if paused {
                    ("Paused".to_string(), "the AI waits for you".to_string())
                } else {
                    match &latest {
                        Some(e) if e.outcome == Outcome::Working => (
                            "The AI is building".to_string(),
                            format!("{} {}", e.label, e.detail),
                        ),
                        Some(e) => ("The AI".to_string(), format!("{}: {}", e.label, e.detail)),
                        None => (String::new(), String::new()),
                    }
                };
                ui.label(egui::RichText::new(head).color(theme::p().cream));
                let rest: String = rest.chars().take(56).collect();
                ui.label(egui::RichText::new(rest).color(theme::p().text_dim));
                ui.add_space(8.0);
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
        },
    );
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
