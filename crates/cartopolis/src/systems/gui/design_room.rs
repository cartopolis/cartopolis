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
const GRID_HALF_M: i32 = 12;

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
    loaded: bool,
    inbox: Inbox,
}

impl DesignRoom {
    /// Open the room around a spot, without moving the camera.
    pub fn open_at(&mut self, lat: f64, lng: f64) {
        self.spot = Some((lat, lng));
        self.status.clear();
        self.loaded = false;
    }
}

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
    let body = serde_json::json!({ "latitude": spot.0, "longitude": spot.1 }).to_string();
    IoTaskPool::get()
        .spawn(async move {
            let path = format!("/api/inventory/designs/{id}/place");
            let reply = match model_assets::api_post(&path, Some(body)).await {
                Ok(_) => Reply::Done(format!("Placed a copy of \u{201c}{name}\u{201d} here.")),
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

/// Take the ring's Design here, bring the camera to the spot, count the parts
/// and drain finished requests.
pub fn drive_design_room(
    mut room: ResMut<DesignRoom>,
    mut menu: ResMut<ContextMenuState>,
    mut nav: ResMut<crate::systems::geo_nav::GeoNavState>,
    online: Option<Res<crate::resources::OnlineSession>>,
    prims: Query<(&Prim, &PrimGeo)>,
) {
    use crate::systems::geo_nav::{CameraPose, Teleport};
    if let Some((lat, lng)) = menu.design_here.take() {
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
    let Some(spot) = room.spot else {
        return;
    };
    let parts: Vec<i64> = prims
        .iter()
        .filter(|(_, g)| metres_between(spot, (g.latitude, g.longitude)) <= PARTS_RADIUS_M)
        .map(|(p, _)| p.id)
        .collect();
    if parts != room.parts {
        room.parts = parts;
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
    prims: Query<Entity, With<Prim>>,
    children: Query<&Children>,
    layered: Query<(), With<RenderLayers>>,
    mut pieces: Query<(Entity, &mut Transform, &mut GridCell), With<RoomPiece>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let (Some((lat, lng)), Some(anchor)) = (room.spot, map_anchor.geo) else {
        // Closed: everything back on layer 0, the room gone. Prims keep their
        // two layers, which layer 0 alone still draws.
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
    // Every prim and what hangs off it (a model's scene, an emitter), on both
    // layers: drawn in the room and in the street alike.
    let both = RenderLayers::from_layers(&[0, DESIGN_LAYER]);
    let mut stack: Vec<Entity> = prims.iter().collect();
    while let Some(entity) = stack.pop() {
        if layered.get(entity).is_err() {
            commands.entity(entity).try_insert(both.clone());
        }
        if let Ok(kids) = children.get(entity) {
            stack.extend(kids.iter());
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
    let shade = |grey: f32, x: f32, z: f32| {
        let t = (x.hypot(z) / half).clamp(0.0, 1.0);
        let fade = t * t * (3.0 - 2.0 * t);
        let g = grey + (1.0 - grey) * fade;
        [g, g, g, 1.0]
    };
    for i in -GRID_HALF_M..=GRID_HALF_M {
        let (width, grey) = match i {
            0 => (0.035, 0.35),
            _ if i % 5 == 0 => (0.025, 0.5),
            _ => (0.015, 0.66),
        };
        let at = i as f32;
        let w = width / 2.0;
        for s in -GRID_HALF_M..GRID_HALF_M {
            let (a, b) = (s as f32, s as f32 + 1.0);
            // One segment along x and one along z.
            for quad in [
                [[a, at - w], [b, at - w], [b, at + w], [a, at + w]],
                [[at - w, a], [at + w, a], [at + w, b], [at - w, b]],
            ] {
                let base = positions.len() as u32;
                for [x, z] in quad {
                    positions.push([x, 0.01, z]);
                    colours.push(shade(grey, x, z));
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
) {
    let Some(spot) = room.spot else {
        return;
    };
    let signed_in = online.is_some() && model_assets::have_endpoint();
    let bottom = crate::systems::dock::reserved_bottom() + 8.0;
    let width = 420.0_f32.min(egui.viewport_pts().x - 2.0 * theme::OSD_MARGIN);

    let mut name = std::mem::take(&mut room.name);
    let parts = room.parts.len();
    let designs = room.designs.clone();
    let status = room.status.clone();
    let busy = room.busy;
    let mut add_part = false;
    let mut save = false;
    let mut leave = false;
    let mut place: Option<(i64, String)> = None;
    let mut forget: Option<(i64, String)> = None;

    egui::Area::new(egui::Id::new("design_room_bar"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -bottom))
        .show(egui.ctx(), |ui| {
            theme::overlay_frame().show(ui, |ui| {
                ui.set_width(width);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::close_button(ui)
                            .on_hover_text("leave the room; what you built stays standing")
                            .clicked()
                        {
                            leave = true;
                        }
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
    if leave {
        room.spot = None;
        room.parts.clear();
        return;
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
