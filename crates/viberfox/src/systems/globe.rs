//! Planetary globe / space tier (ADR-025).
//!
//! A true-scale WGS84 Earth built in the sim anchor's ENU tangent frame
//! (`vibe_core::geo`), so the flat streamed-tile city view sits on the sphere's
//! surface at the anchor and Earth's centre lands at ≈ `(0, −R, 0)` in Bevy-local
//! metres. The mesh is one entity under the world `BigSpace`, so `big_space`
//! rebases it for render precision at any camera distance (ADR-019).
//!
//! The camera side (orbiting Earth's centre from space, and the continuous
//! surface↔space handoff) lives in `free_camera`; this module owns the globe
//! geometry, its OSM-baked skin, and the Earth-centre / radius the camera reads.

use std::sync::{Arc, Mutex};

use bevy::math::DVec3;
use bevy::prelude::*;
use bevy::render::render_asset::RenderAssetUsages;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::render::view::NoFrustumCulling;
use bevy_atmosphere::plugin::AtmosphereSkyBox;
use bevy_image::{Image, ImageSampler};
use big_space::prelude::{BigSpace, GridCell};

use crate::resources::OsmTileUrlTemplate;
use crate::systems::map_stream::{lat_lng_to_tile_frac, MapStream};
use crate::systems::tile_loader::load_tile_image;
use vibe_core::geo::TangentFrame;

/// Latitude/longitude tessellation of the globe mesh. Coarse is fine — from the
/// only distance it's seen (space) a degree is sub-pixel, and it keeps the mesh
/// well within f32 precision when rebased.
const GLOBE_RINGS: usize = 64;
const GLOBE_SEGMENTS: usize = 128;

/// OSM zoom the globe skin is baked from. z3 = 64 tiles = a whole-world image at
/// 2048 px wide — recognisable coastlines without a heavy cold-cache fetch.
const GLOBE_SKIN_ZOOM: u32 = 3;
/// Baked equirectangular skin resolution.
const SKIN_W: u32 = 2048;
const SKIN_H: u32 = 1024;
/// Web-Mercator can't reach the poles; clamp the bake here (its native limit).
const MERCATOR_MAX_LAT: f64 = 85.051_128_78;

/// A finished skin bake handed back from the worker thread.
struct SkinBake {
    rgba: Vec<u8>,
}

// ── Mid-zoom spherical caps (ADR-025, first slice of spherical streaming) ────
//
// Between the z3 whole-earth skin and the flat detail pyramid (≥z10, hidden
// above the space threshold) no imagery existed, so the 50 km – 3000 km band
// was a blur. These clipmap-style caps drape re-projected windows of mid-zoom
// tiles onto the sphere around the camera's sub-point, re-baking as it roams.

/// Cap levels, coarse → fine. Windows are sized to keep each bake at ~60–100
/// tiles (all through the shared on-disk tile cache).
const CAP_LEVELS: [CapSpec; 2] = [
    CapSpec { zoom: 6, half_deg: 20.0, offset_m: 400.0 },
    CapSpec { zoom: 9, half_deg: 3.0, offset_m: 800.0 },
];
/// Baked cap texture edge (px).
const CAP_TEX: u32 = 1024;
/// Mesh tessellation of a cap (segments per axis).
const CAP_SEGS: usize = 48;
/// Re-bake when the sub-point drifts this fraction of the half-window from the
/// baked centre.
const CAP_REBAKE_FRAC: f64 = 0.35;
/// Caps are only maintained from this altitude up (below it the flat detail
/// tiles own the view).
const CAP_MIN_ALT: f64 = 20_000.0;

#[derive(Clone, Copy)]
struct CapSpec {
    zoom: u32,
    /// Half-size of the lat/lng window (degrees).
    half_deg: f64,
    /// Lift above the base sphere so the finer cap draws on top (visually
    /// negligible at cap viewing distances).
    offset_m: f32,
}

/// A finished cap bake from the worker thread. An empty `rgba` marks a failed
/// bake (recorded so the level doesn't refetch in a loop; it retries on the
/// next sub-point move).
struct CapBake {
    center: (f64, f64),
    rgba: Vec<u8>,
}

#[derive(Default)]
pub struct CapLevel {
    entity: Option<Entity>,
    /// Sub-point the current mesh/texture was baked around.
    built_center: Option<(f64, f64)>,
    in_flight: bool,
    result: Arc<Mutex<Option<CapBake>>>,
}

#[derive(Resource, Default)]
pub struct GlobeState {
    /// The globe entity, once the anchor exists and it has been spawned.
    entity: Option<Entity>,
    /// Earth's centre in Bevy-local metres (what the space camera orbits).
    pub center_local: Vec3,
    /// Geocentric radius at the anchor (m) — the sphere's local radius, used for
    /// the camera's altitude and zoom clamps.
    pub radius: f32,
    /// The (lat, lng) the globe is currently built around (the local origin).
    pub anchor_lat: f64,
    pub anchor_lng: f64,
    /// True once the mesh is placed and `center_local`/`radius` are valid.
    pub ready: bool,
    /// The `MapStream::anchor_generation` the current mesh was built for; a change
    /// triggers a rebuild in the new origin's frame.
    built_gen: u32,
    /// The globe's material, so the baked skin can be dropped in when it lands.
    material: Option<Handle<StandardMaterial>>,
    /// Shared cell the skin-bake worker writes into; polled by `apply_globe_skin`.
    skin: Arc<Mutex<Option<SkinBake>>>,
    skin_requested: bool,
    skin_applied: bool,
    /// Mid-zoom detail caps around the camera sub-point (see `CAP_LEVELS`).
    caps: [CapLevel; 2],
}

impl GlobeState {
    /// The camera's **geocentric** altitude (m): distance from the Earth's centre
    /// minus the surface radius. This is the only correct "how high am I" once
    /// the camera can orbit the planet — the local-frame `y` is only altitude
    /// near the anchor and goes *negative* on the far side of the globe, which
    /// made every y-based check (tile freeze, globe visibility, fog, skybox)
    /// misfire in space. Falls back to local `y` until the globe exists.
    pub fn altitude_m(&self, cell: &GridCell, tf: &Transform) -> f64 {
        let edge = crate::systems::free_camera::WORLD_CELL_EDGE as f64;
        let cam = DVec3::new(
            cell.x as f64 * edge + tf.translation.x as f64,
            cell.y as f64 * edge + tf.translation.y as f64,
            cell.z as f64 * edge + tf.translation.z as f64,
        );
        if !self.ready {
            return cam.y;
        }
        (cam - self.center_local.as_dvec3()).length() - self.radius as f64
    }

    /// Whether the camera is high enough that the globe is shown and the local
    /// ground props are hidden — the shared test for every system that flips
    /// per-tier visibility (single source of truth; two systems disagreeing on
    /// this was a real fight, see `apply_feature_toggles`).
    pub fn in_space(&self, cell: &GridCell, tf: &Transform) -> bool {
        self.ready && self.altitude_m(cell, tf) > GLOBE_SHOW_ALT as f64
    }
}

/// Geographic sub-point (lat, lng) of a Bevy-local direction pointing out from
/// the Earth centre, evaluated in the given anchor's frame. Used by the globe
/// descent to learn *where* the camera is dropping toward so it can re-anchor
/// there (ADR-025). `dir_bevy` should be `(cam - earth_centre).normalize()`.
pub fn sub_point(anchor_lat: f64, anchor_lng: f64, dir_bevy: Vec3) -> (f64, f64) {
    let frame = TangentFrame::new(anchor_lat, anchor_lng, 0.0);
    // Bevy (E, U, −N) → ENU (E, N, U), then rotate ENU → ECEF by the frame basis.
    let enu = DVec3::new(dir_bevy.x as f64, -dir_bevy.z as f64, dir_bevy.y as f64);
    let ecef_dir = frame.east * enu.x + frame.north * enu.y + frame.up * enu.z;
    let (lat, lng, _) =
        vibe_core::ecef_to_geodetic(ecef_dir.normalize_or_zero() * 6_378_137.0);
    (lat, lng)
}

/// Bevy-local metres (x = East, y = Up, z = South) of a geodetic point, in the
/// anchor's ENU frame. ENU is (E, N, U); Bevy is (E, U, −N).
fn geodetic_to_local(frame: &TangentFrame, lat: f64, lng: f64, height: f64) -> Vec3 {
    let enu = frame.geodetic_to_local(lat, lng, height);
    Vec3::new(enu.x as f32, enu.z as f32, -enu.y as f32)
}

/// Once the map anchor exists, build and place the globe (and kick off the skin
/// bake). Runs once.
pub fn setup_globe(
    mut commands: Commands,
    mut globe: ResMut<GlobeState>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    stream: Res<MapStream>,
    bigspace: Query<Entity, With<BigSpace>>,
    tile_url: Res<OsmTileUrlTemplate>,
) {
    let Some((clat, clng, _)) = stream.geo_anchor() else {
        return;
    };
    // (Re)build whenever the anchor moves — the sphere is built in the anchor's
    // tangent frame, so a new origin needs a new mesh (the baked skin is reused).
    let gen = stream.anchor_generation;
    if globe.ready && globe.built_gen == gen {
        return;
    }
    let Ok(root) = bigspace.single() else {
        return;
    };

    let frame = TangentFrame::new(clat, clng, 0.0);
    // Earth centre (ECEF origin) expressed in the anchor's Bevy-local frame.
    let center_enu = frame.world_to_local(DVec3::ZERO);
    let center = Vec3::new(center_enu.x as f32, center_enu.z as f32, -center_enu.y as f32);
    globe.center_local = center;
    globe.radius = center.length();

    // Build the sphere: a lat/lng grid placed through the ENU frame, so it is the
    // real ellipsoid and the anchor sits on its surface at the local origin.
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    for i in 0..=GLOBE_RINGS {
        let v = i as f32 / GLOBE_RINGS as f32; // 0 at north pole
        let lat = 90.0 - v as f64 * 180.0;
        for j in 0..=GLOBE_SEGMENTS {
            let u = j as f32 / GLOBE_SEGMENTS as f32;
            let lng = -180.0 + u as f64 * 360.0;
            let p = geodetic_to_local(&frame, lat, lng, 0.0);
            positions.push([p.x, p.y, p.z]);
            let n = (p - center).normalize_or_zero();
            normals.push([n.x, n.y, n.z]);
            uvs.push([u, v]);
        }
    }
    let stride = (GLOBE_SEGMENTS + 1) as u32;
    for i in 0..GLOBE_RINGS as u32 {
        for j in 0..GLOBE_SEGMENTS as u32 {
            let a = i * stride + j;
            let b = a + stride;
            indices.extend_from_slice(&[a, a + 1, b, b, a + 1, b + 1]);
        }
    }

    let mut mesh = Mesh::new(
        bevy::render::mesh::PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(bevy::render::mesh::Indices::U32(indices));

    // Reuse the material across re-anchors so the baked skin persists; on the
    // first build it's a plain ocean tone until the OSM skin lands. Lit by the
    // scene sun (so the day/night terminator falls across the globe); double-sided
    // since a coarse sphere viewed from just outside can show a back face at the limb.
    let material = globe.material.clone().unwrap_or_else(|| {
        materials.add(StandardMaterial {
            base_color: Color::srgb(0.10, 0.16, 0.28),
            perceptual_roughness: 1.0,
            reflectance: 0.02,
            double_sided: true,
            cull_mode: None,
            ..default()
        })
    });
    globe.material = Some(material.clone());

    // Replace the old sphere (a re-anchor rebuilds it in the new frame), and the
    // detail caps with it — their meshes were built in the old frame too. A bake
    // still in flight stays valid: its texture is frame-independent, and the
    // mesh is built in the current frame at harvest time.
    if let Some(old) = globe.entity.take() {
        commands.entity(old).despawn();
    }
    for cap in globe.caps.iter_mut() {
        if let Some(e) = cap.entity.take() {
            commands.entity(e).despawn();
        }
        cap.built_center = None;
    }

    // One entity at grid cell 0 (its vertices already carry the world offset);
    // big_space rebases it relative to the camera at any distance.
    let entity = commands
        .spawn((
            Mesh3d(meshes.add(mesh)),
            MeshMaterial3d(material),
            Transform::default(),
            GridCell::default(),
            ChildOf(root),
            // The whole globe is one huge mesh straddling the origin; big_space
            // rebasing confuses frustum culling for it, as with the map tiles.
            NoFrustumCulling,
            bevy::pbr::NotShadowCaster,
            bevy::pbr::NotShadowReceiver,
            Visibility::Hidden, // shown by the camera only in/near space
            GlobeMesh,
        ))
        .id();
    globe.entity = Some(entity);
    globe.anchor_lat = clat;
    globe.anchor_lng = clng;
    globe.built_gen = gen;
    globe.ready = true;

    // Kick off the skin bake on a worker thread (once — the whole-Earth texture
    // is location-independent, so re-anchors reuse it).
    if !globe.skin_requested {
        globe.skin_requested = true;
        let template = tile_url.0.lock().map(|g| g.clone()).unwrap_or_default();
        let out = globe.skin.clone();
        std::thread::spawn(move || {
            if let Some(rgba) = bake_skin(&template) {
                if let Ok(mut g) = out.lock() {
                    *g = Some(SkinBake { rgba });
                }
            }
        });
    }

    tracing::info!(radius = globe.radius, "globe placed");
}

/// Marker for the globe mesh entity.
#[derive(Component)]
pub struct GlobeMesh;

/// Local-scene ground props (the horizon disc, region quads, streamed map tiles)
/// that should be hidden once the globe is in view — from space they read as
/// bright specks/discs floating over the anchor. Attached where each is spawned.
#[derive(Component)]
pub struct HideInSpace;

/// Poll the skin-bake worker and apply the finished texture to the globe material.
pub fn apply_globe_skin(
    mut globe: ResMut<GlobeState>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    if globe.skin_applied {
        return;
    }
    let bake = globe.skin.lock().ok().and_then(|mut g| g.take());
    let Some(bake) = bake else {
        return;
    };
    let Some(mat_handle) = globe.material.clone() else {
        return;
    };
    let mut image = Image::new(
        Extent3d {
            width: SKIN_W,
            height: SKIN_H,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        bake.rgba,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    image.sampler = ImageSampler::linear();
    let handle = images.add(image);
    if let Some(mat) = materials.get_mut(&mat_handle) {
        mat.base_color = Color::WHITE;
        mat.base_color_texture = Some(handle);
    }
    globe.skin_applied = true;
    tracing::info!("globe skin applied");
}

/// Altitude (m) above which the globe is drawn. Below it the detailed flat-tile
/// city view fully covers the sky-facing hemisphere, so the globe stays hidden
/// (and can't z-fight the tiles that sit on its surface).
pub const GLOBE_SHOW_ALT: f32 = 50_000.0;

/// Show the globe only once high enough that it's the thing you're looking at, so
/// normal ground play never pays for it or sees it poke through the tiles. At the
/// same altitude the atmosphere skybox is hidden: it's a ~300 km depth-writing
/// shell around the camera and would otherwise occlude the globe 6,400 km away,
/// so above the threshold the sky becomes the black-of-space clear colour (with
/// the star field showing through).
#[allow(clippy::type_complexity)]
pub fn update_globe_visibility(
    globe: Res<GlobeState>,
    camera: Query<(&GridCell, &Transform), With<crate::systems::free_camera::FreeCamera>>,
    mut globe_vis: Query<
        &mut Visibility,
        (With<GlobeMesh>, Without<AtmosphereSkyBox>, Without<HideInSpace>),
    >,
    mut sky_vis: Query<
        &mut Visibility,
        (With<AtmosphereSkyBox>, Without<GlobeMesh>, Without<HideInSpace>),
    >,
    // Only the ground props NOT owned by the Layers toggles (i.e. the horizon
    // disc): tiles, region quads, buildings and windows are excluded because
    // `data_layers::apply_feature_toggles` is their single visibility writer —
    // it combines the panel toggle with `GlobeState::in_space`. Two systems
    // writing the same entities raced nondeterministically (tiles sometimes
    // stayed visible from orbit).
    mut hide_vis: Query<
        &mut Visibility,
        (
            With<HideInSpace>,
            Without<GlobeMesh>,
            Without<AtmosphereSkyBox>,
            Without<crate::systems::map_stream::StreamTile>,
            Without<crate::systems::rendering::RegionMesh>,
            Without<crate::systems::osm_buildings::OsmBuildingMesh>,
            Without<crate::systems::osm_buildings::WindowGlow>,
        ),
    >,
) {
    if !globe.ready {
        return;
    }
    let Ok((cell, tf)) = camera.single() else {
        return;
    };
    let show_globe = globe.in_space(cell, tf);

    let want_globe = if show_globe { Visibility::Visible } else { Visibility::Hidden };
    for mut v in globe_vis.iter_mut() {
        if *v != want_globe {
            *v = want_globe;
        }
    }
    // Hide the skybox and the local ground props while the globe is up (so the
    // sky reads as black space and no bright disc floats over the anchor); show
    // them again on the way back down.
    let want_local = if show_globe { Visibility::Hidden } else { Visibility::Inherited };
    for mut v in sky_vis.iter_mut() {
        if *v != want_local {
            *v = want_local;
        }
    }
    for mut v in hide_vis.iter_mut() {
        if *v != want_local {
            *v = want_local;
        }
    }
}

/// Maintain the mid-zoom detail caps: while high enough that the globe is (or is
/// about to be) the view, keep each cap level baked around the camera's current
/// sub-point, re-baking on a worker as it drifts (see `CAP_LEVELS`).
pub fn update_globe_caps(
    mut commands: Commands,
    mut globe: ResMut<GlobeState>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    camera: Query<(&GridCell, &Transform), With<crate::systems::free_camera::FreeCamera>>,
    bigspace: Query<Entity, With<BigSpace>>,
    tile_url: Res<OsmTileUrlTemplate>,
) {
    if !globe.ready {
        return;
    }
    let Ok((cell, tf)) = camera.single() else {
        return;
    };
    let Ok(root) = bigspace.single() else {
        return;
    };

    // Sub-point under the camera (where detail is needed).
    let edge = crate::systems::free_camera::WORLD_CELL_EDGE as f64;
    let cam = DVec3::new(
        cell.x as f64 * edge + tf.translation.x as f64,
        cell.y as f64 * edge + tf.translation.y as f64,
        cell.z as f64 * edge + tf.translation.z as f64,
    );
    let altitude = (cam - globe.center_local.as_dvec3()).length() - globe.radius as f64;
    let dir = (cam - globe.center_local.as_dvec3()).normalize_or_zero().as_vec3();
    let (slat, slng) = sub_point(globe.anchor_lat, globe.anchor_lng, dir);
    let (anchor_lat, anchor_lng) = (globe.anchor_lat, globe.anchor_lng);

    for (i, spec) in CAP_LEVELS.iter().enumerate() {
        // Kick off a (re)bake when the sub-point has drifted from the baked
        // centre — but only while the cap can actually be seen.
        let need = altitude >= CAP_MIN_ALT
            && match globe.caps[i].built_center {
                None => true,
                Some((blat, blng)) => {
                    let dlng = ((blng - slng + 540.0).rem_euclid(360.0)) - 180.0;
                    (blat - slat).abs() > spec.half_deg * CAP_REBAKE_FRAC
                        || dlng.abs() > spec.half_deg * CAP_REBAKE_FRAC
                }
            };
        if need && !globe.caps[i].in_flight {
            globe.caps[i].in_flight = true;
            let out = globe.caps[i].result.clone();
            let template = tile_url.0.lock().map(|g| g.clone()).unwrap_or_default();
            let spec = *spec;
            let center = (slat, slng);
            std::thread::spawn(move || {
                let rgba = bake_cap(center, &spec, &template).unwrap_or_default();
                if let Ok(mut g) = out.lock() {
                    *g = Some(CapBake { center, rgba });
                }
            });
        }

        // Harvest a finished bake: swap in a fresh cap mesh + texture.
        let bake = globe.caps[i].result.lock().ok().and_then(|mut g| g.take());
        let Some(bake) = bake else {
            continue;
        };
        globe.caps[i].in_flight = false;
        // Record even a failed bake so it doesn't refetch in a tight loop; the
        // next sub-point move retries naturally.
        globe.caps[i].built_center = Some(bake.center);
        if bake.rgba.is_empty() {
            continue;
        }

        let mut image = Image::new(
            Extent3d {
                width: CAP_TEX,
                height: CAP_TEX,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            bake.rgba,
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::default(),
        );
        image.sampler = ImageSampler::linear();
        let texture = images.add(image);

        // The cap mesh: a lat/lng grid over the baked window, draped on the
        // ellipsoid in the anchor frame, lifted `offset_m` so finer caps draw
        // over coarser ones and the base sphere.
        let frame = TangentFrame::new(anchor_lat, anchor_lng, 0.0);
        let center = globe.center_local;
        let (clat, clng) = bake.center;
        let n_lat = (clat + spec.half_deg).min(MERCATOR_MAX_LAT);
        let s_lat = (clat - spec.half_deg).max(-MERCATOR_MAX_LAT);
        let w_lng = (clng - spec.half_deg).max(-180.0);
        let e_lng = (clng + spec.half_deg).min(180.0);
        let mut positions: Vec<[f32; 3]> = Vec::new();
        let mut normals: Vec<[f32; 3]> = Vec::new();
        let mut uvs: Vec<[f32; 2]> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        for iy in 0..=CAP_SEGS {
            let v = iy as f64 / CAP_SEGS as f64;
            let lat = n_lat + (s_lat - n_lat) * v;
            for ix in 0..=CAP_SEGS {
                let u = ix as f64 / CAP_SEGS as f64;
                let lng = w_lng + (e_lng - w_lng) * u;
                let p = geodetic_to_local(&frame, lat, lng, spec.offset_m as f64);
                positions.push([p.x, p.y, p.z]);
                let n = (p - center).normalize_or_zero();
                normals.push([n.x, n.y, n.z]);
                uvs.push([u as f32, v as f32]);
            }
        }
        let stride = (CAP_SEGS + 1) as u32;
        for iy in 0..CAP_SEGS as u32 {
            for ix in 0..CAP_SEGS as u32 {
                let a = iy * stride + ix;
                let b = a + stride;
                indices.extend_from_slice(&[a, a + 1, b, b, a + 1, b + 1]);
            }
        }
        let mut mesh = Mesh::new(
            bevy::render::mesh::PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
        mesh.insert_indices(bevy::render::mesh::Indices::U32(indices));

        if let Some(old) = globe.caps[i].entity.take() {
            commands.entity(old).despawn();
        }
        let entity = commands
            .spawn((
                Mesh3d(meshes.add(mesh)),
                MeshMaterial3d(materials.add(StandardMaterial {
                    base_color_texture: Some(texture),
                    perceptual_roughness: 1.0,
                    reflectance: 0.0,
                    ..default()
                })),
                Transform::default(),
                GridCell::default(),
                ChildOf(root),
                NoFrustumCulling,
                bevy::pbr::NotShadowCaster,
                bevy::pbr::NotShadowReceiver,
                // Shares the globe's visibility handling (shown only in space).
                Visibility::Hidden,
                GlobeMesh,
            ))
            .id();
        globe.caps[i].entity = Some(entity);
        tracing::info!(level = i, zoom = spec.zoom, lat = bake.center.0, lng = bake.center.1, "globe cap applied");
    }
}

/// Bake one cap: fetch the window's tiles at the cap zoom (disk-cached) and
/// re-project Web-Mercator → a plate-carrée window texture matching the cap
/// mesh's linear lat/lng UVs. Runs on a worker thread. Windows touching the
/// dateline are clamped (slightly narrower cap there — acceptable for now).
fn bake_cap(center: (f64, f64), spec: &CapSpec, template: &str) -> Option<Vec<u8>> {
    use vibe_core::TileKey;

    let (clat, clng) = center;
    let n_lat = (clat + spec.half_deg).min(MERCATOR_MAX_LAT);
    let s_lat = (clat - spec.half_deg).max(-MERCATOR_MAX_LAT);
    let w_lng = (clng - spec.half_deg).max(-180.0);
    let e_lng = (clng + spec.half_deg).min(180.0);

    // Covering tile range (y grows southward in tile space).
    let (fx0, fy0) = lat_lng_to_tile_frac(n_lat, w_lng, spec.zoom);
    let (fx1, fy1) = lat_lng_to_tile_frac(s_lat, e_lng, spec.zoom);
    let n = 1i64 << spec.zoom;
    let tx0 = (fx0.floor() as i64).clamp(0, n - 1);
    let ty0 = (fy0.floor() as i64).clamp(0, n - 1);
    let tx1 = (fx1.floor() as i64).clamp(0, n - 1);
    let ty1 = (fy1.floor() as i64).clamp(0, n - 1);
    let (tw, th) = ((tx1 - tx0 + 1) as u32, (ty1 - ty0 + 1) as u32);
    if tw * th > 200 {
        tracing::warn!(tiles = tw * th, "cap window too large; skipped");
        return None;
    }

    // Stitch the tiles into one Mercator-space buffer.
    let (sw, sh) = (tw * 256, th * 256);
    let mut src = vec![0u8; (sw * sh * 4) as usize];
    let mut got = 0usize;
    for ty in ty0..=ty1 {
        for tx in tx0..=tx1 {
            let key = TileKey::new(tx, ty, spec.zoom);
            let Ok(bytes) = load_tile_image(&key, template) else {
                continue;
            };
            let Ok(img) = image::load_from_memory(&bytes) else {
                continue;
            };
            let rgba = img.to_rgba8();
            let (iw, ih) = (rgba.width().min(256), rgba.height().min(256));
            for y in 0..ih {
                for x in 0..iw {
                    let px = rgba.get_pixel(x, y).0;
                    let dx = (tx - tx0) as u32 * 256 + x;
                    let dy = (ty - ty0) as u32 * 256 + y;
                    let di = ((dy * sw + dx) * 4) as usize;
                    src[di..di + 4].copy_from_slice(&px);
                }
            }
            got += 1;
        }
    }
    if got == 0 {
        return None;
    }

    // Re-project into the window texture (linear in lat/lng, like the mesh UVs).
    let mut out = vec![0u8; (CAP_TEX * CAP_TEX * 4) as usize];
    for py in 0..CAP_TEX {
        let lat = n_lat + (s_lat - n_lat) * ((py as f64 + 0.5) / CAP_TEX as f64);
        for px in 0..CAP_TEX {
            let lng = w_lng + (e_lng - w_lng) * ((px as f64 + 0.5) / CAP_TEX as f64);
            let (fx, fy) = lat_lng_to_tile_frac(lat, lng, spec.zoom);
            let sx = (((fx - tx0 as f64) * 256.0) as i64).clamp(0, sw as i64 - 1) as u32;
            let sy = (((fy - ty0 as f64) * 256.0) as i64).clamp(0, sh as i64 - 1) as u32;
            let si = ((sy * sw + sx) * 4) as usize;
            let di = ((py * CAP_TEX + px) * 4) as usize;
            out[di..di + 4].copy_from_slice(&src[si..si + 4]);
        }
    }
    Some(out)
}

/// Fetch the low-zoom OSM tile grid and reproject Web-Mercator → equirectangular
/// into an `SKIN_W × SKIN_H` RGBA buffer. Runs on a worker thread; returns `None`
/// if not enough tiles could be fetched to be worth showing.
fn bake_skin(template: &str) -> Option<Vec<u8>> {
    use vibe_core::TileKey;

    let n = 1usize << GLOBE_SKIN_ZOOM; // tiles per axis
    let merc = (n * 256) as u32; // stitched mercator image edge (px)
    let mut src = vec![0u8; (merc * merc * 4) as usize]; // RGBA, opaque continents
    let mut got = 0usize;
    for ty in 0..n {
        for tx in 0..n {
            let key = TileKey::new(tx as i64, ty as i64, GLOBE_SKIN_ZOOM);
            let Ok(bytes) = load_tile_image(&key, template) else {
                continue;
            };
            let Ok(img) = image::load_from_memory(&bytes) else {
                continue;
            };
            let rgba = img.to_rgba8();
            let (iw, ih) = (rgba.width().min(256), rgba.height().min(256));
            for y in 0..ih {
                for x in 0..iw {
                    let px = rgba.get_pixel(x, y).0;
                    let dx = tx as u32 * 256 + x;
                    let dy = ty as u32 * 256 + y;
                    let di = ((dy * merc + dx) * 4) as usize;
                    src[di..di + 4].copy_from_slice(&px);
                }
            }
            got += 1;
        }
    }
    if got == 0 {
        return None;
    }

    // Reproject to equirectangular: for each output row pick the latitude, map it
    // to a Mercator y, and copy that row's longitude span.
    let mut out = vec![0u8; (SKIN_W * SKIN_H * 4) as usize];
    for py in 0..SKIN_H {
        let lat = (90.0 - (py as f64 + 0.5) / SKIN_H as f64 * 180.0)
            .clamp(-MERCATOR_MAX_LAT, MERCATOR_MAX_LAT);
        for px in 0..SKIN_W {
            let lng = -180.0 + (px as f64 + 0.5) / SKIN_W as f64 * 360.0;
            let (fx, fy) = lat_lng_to_tile_frac(lat, lng, GLOBE_SKIN_ZOOM);
            let sx = ((fx * 256.0) as u32).min(merc - 1);
            let sy = ((fy * 256.0) as u32).min(merc - 1);
            let si = ((sy * merc + sx) * 4) as usize;
            let di = ((py * SKIN_W + px) * 4) as usize;
            out[di..di + 4].copy_from_slice(&src[si..si + 4]);
        }
    }
    Some(out)
}
