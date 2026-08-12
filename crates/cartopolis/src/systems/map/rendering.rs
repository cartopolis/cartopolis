use crate::components::{NeedsMeshRebuild, NeedsTextureRefresh, Prim, PrimShape, Region};
use crate::components::{OwnedMaterial, PrimMesh, RegionMesh};
use crate::grid::{WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use crate::resources::PrimTextureCache;
use crate::systems::tile_loader::{RegionTile, TileKey};
use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::math::primitives::{Cuboid, Cylinder, Sphere, Torus};
use bevy::math::Affine2;
use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology, VertexAttributeValues};
use big_space::prelude::{BigSpace, Grid};
use cartopolis_core::world::tile_to_meters;
use cartopolis_core::PrimSurface;
use std::collections::HashMap;

/// Linear-filtered sampler with repeat addressing, so texture repeats (`repeat_u/v > 1`)
/// tile instead of clamping at the edge. Used for all prim textures.
pub fn repeat_linear_sampler() -> ImageSampler {
    ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        address_mode_w: ImageAddressMode::Repeat,
        ..ImageSamplerDescriptor::linear()
    })
}

/// Convenience: build a mipmapped [`Image`] from a mip-0 RGBA8 buffer in one call
/// (mip generation + upload assembly), for on-main-thread callers like prim textures.
pub fn mipmapped_image(
    width: u32,
    height: u32,
    mip0: Vec<u8>,
    format: bevy::render::render_resource::TextureFormat,
    usage: RenderAssetUsages,
    sampler: ImageSampler,
) -> Image {
    let (data, mip_count) = crate::systems::mip::build_mip_chain(width, height, mip0);
    crate::systems::tile_loader::image_from_mip_chain(
        width, height, data, mip_count, format, usage, sampler,
    )
}

/// Subtle selection cue added on top of a surfaced material: a faint additive blue
/// emissive that signals "selected" while leaving the prim's color and texture clearly
/// visible (unlike a full base-color tint, which reads as a solid slab). The gizmo arms
/// provide the primary selection affordance; this is just a gentle wash.
pub fn apply_selection_highlight(mat: &mut StandardMaterial) {
    let e = mat.emissive;
    mat.emissive = LinearRgba::new(e.red + 0.03, e.green + 0.06, e.blue + 0.16, 1.0);
}

/// Build the UV transform for a prim's texture from its surface params
/// (repeats per face = UV scale, flip = negative scale, rotation, offset).
pub fn surface_uv_transform(s: &PrimSurface) -> Affine2 {
    let mut scale = Vec2::new(s.repeat_u, s.repeat_v);
    if s.flip_u {
        scale.x = -scale.x;
    }
    if s.flip_v {
        scale.y = -scale.y;
    }
    // Pivot repeats (scale) and rotation about the texture center (0.5, 0.5) so the
    // texture center stays put as you tile/rotate, then apply the offset.
    let center = Vec2::splat(0.5);
    Affine2::from_translation(Vec2::new(s.offset_u, s.offset_v))
        * Affine2::from_translation(center)
        * Affine2::from_angle(s.rotation.to_radians())
        * Affine2::from_scale(scale)
        * Affine2::from_translation(-center)
}

/// Apply a prim's surface params onto its material: overall transparency (base-color
/// alpha + blend mode), glow (emissive), full-bright (unlit), and the UV transform.
/// `base_color` is the prim's untinted RGB; alpha comes from the surface.
pub fn apply_surface(mat: &mut StandardMaterial, base_color: Color, s: &PrimSurface) {
    let lin = base_color.to_linear();
    mat.base_color = Color::linear_rgba(lin.red, lin.green, lin.blue, s.alpha);
    mat.alpha_mode = if s.alpha < 0.999 {
        AlphaMode::Blend
    } else {
        AlphaMode::Opaque
    };
    mat.emissive = LinearRgba::rgb(lin.red * s.glow, lin.green * s.glow, lin.blue * s.glow);
    mat.unlit = s.full_bright;
    mat.uv_transform = surface_uv_transform(s);
}

// ---------------------------------------------------------------------------
// Draw-call batching: shared mesh + material pools
//
// Bevy batches instances that share the *same* mesh and material asset handles.
// Prims are overwhelmingly duplicates (a thousand identical grey boxes), so we
// pool handles by geometry and by appearance instead of minting a fresh mesh +
// material per prim. A prim that is later individually restyled (selected,
// edited, re-textured) copies its material out of the pool on first write via
// `owned_material`, so per-entity edits never leak into their pooled siblings.
// ---------------------------------------------------------------------------

/// Geometry cache key. Two prims with identical shape + deformation share one
/// mesh handle. Float fields use their exact bit pattern, so identical inputs to
/// `prim_mesh_handle` (which is a pure function of these) hash and compare equal.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct MeshKey {
    shape: u8,
    bits: [u32; 11],
}

impl MeshKey {
    fn from_prim(p: &Prim) -> Self {
        let shape = match p.shape {
            PrimShape::Box => 0,
            PrimShape::Sphere => 1,
            PrimShape::Cylinder => 2,
            PrimShape::Cone => 3,
            PrimShape::Torus => 4,
        };
        Self {
            shape,
            bits: [
                p.path_cut_begin.to_bits(),
                p.path_cut_end.to_bits(),
                p.hollow.to_bits(),
                p.twist_begin.to_bits(),
                p.twist_end.to_bits(),
                p.taper_x.to_bits(),
                p.taper_y.to_bits(),
                p.top_shear_x.to_bits(),
                p.top_shear_y.to_bits(),
                p.slice_begin.to_bits(),
                p.slice_end.to_bits(),
            ],
        }
    }
}

/// Appearance cache key. Two prims with identical colour + texture id + surface
/// share one material handle (required, alongside a shared mesh, for the pair to
/// batch into a single draw call).
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct MaterialKey {
    color: [u32; 4],
    texture_id: Option<String>,
    alpha: u32,
    glow: u32,
    full_bright: bool,
    repeat_u: u32,
    repeat_v: u32,
    flip_u: bool,
    flip_v: bool,
    rotation: u32,
    offset_u: u32,
    offset_v: u32,
}

impl MaterialKey {
    fn from_prim(p: &Prim) -> Self {
        let c = p.color.to_linear();
        let s = &p.surface;
        Self {
            color: [
                c.red.to_bits(),
                c.green.to_bits(),
                c.blue.to_bits(),
                c.alpha.to_bits(),
            ],
            texture_id: p.texture_id.clone(),
            alpha: s.alpha.to_bits(),
            glow: s.glow.to_bits(),
            full_bright: s.full_bright,
            repeat_u: s.repeat_u.to_bits(),
            repeat_v: s.repeat_v.to_bits(),
            flip_u: s.flip_u,
            flip_v: s.flip_v,
            rotation: s.rotation.to_bits(),
            offset_u: s.offset_u.to_bits(),
            offset_v: s.offset_v.to_bits(),
        }
    }
}

/// Pool of shared prim mesh handles, keyed by geometry.
#[derive(Resource, Default)]
pub struct MeshCache(HashMap<MeshKey, Handle<Mesh>>);

/// Pool of shared prim material handles, keyed by appearance.
#[derive(Resource, Default)]
pub struct MaterialCache(HashMap<MaterialKey, Handle<StandardMaterial>>);

/// Return the pooled mesh handle for `prim`, building and caching it on first use.
fn cached_prim_mesh(prim: &Prim, meshes: &mut Assets<Mesh>, cache: &mut MeshCache) -> Handle<Mesh> {
    let key = MeshKey::from_prim(prim);
    if let Some(handle) = cache.0.get(&key) {
        return handle.clone();
    }
    let handle = prim_mesh_handle(prim, meshes);
    cache.0.insert(key, handle.clone());
    handle
}

/// Return the pooled material handle for `prim`, building and caching it on first use.
fn cached_prim_material(
    prim: &Prim,
    materials: &mut Assets<StandardMaterial>,
    cache: &mut MaterialCache,
    texture_cache: &PrimTextureCache,
) -> Handle<StandardMaterial> {
    // Only untextured prims are pooled. A texture may not be loaded yet at spawn
    // (it arrives via NeedsTextureRefresh), so a pooled entry keyed by texture id
    // could go stale and hand a later prim the pre-load (textureless) material.
    // Textured prims are the minority and drop out of any pool on first restyle
    // anyway, so give them a fresh material — matching pre-pooling behaviour.
    let poolable = prim.texture_id.is_none();
    let key = MaterialKey::from_prim(prim);
    if poolable {
        if let Some(handle) = cache.0.get(&key) {
            return handle.clone();
        }
    }
    let base_color_texture = prim
        .texture_id
        .as_ref()
        .and_then(|id| texture_cache.handles.get(id))
        .cloned();
    let mut material = StandardMaterial {
        base_color: prim.color,
        base_color_texture,
        ..default()
    };
    apply_surface(&mut material, prim.color, &prim.surface);
    let handle = materials.add(material);
    if poolable {
        cache.0.insert(key, handle.clone());
    }
    handle
}

/// Copy-on-write access to a prim's material. If the prim already owns its
/// material (`already_owned`), returns it mutably in place. Otherwise clones the
/// shared pooled material into a fresh handle, swaps that onto `entity`, tags it
/// `OwnedMaterial`, and returns the clone — so the mutation cannot leak into
/// sibling prims still sharing the pooled handle. Safe to call every frame: once
/// the entity owns its material the first branch reuses it with no new asset.
pub fn owned_material<'a>(
    commands: &mut Commands,
    materials: &'a mut Assets<StandardMaterial>,
    entity: Entity,
    handle: &Handle<StandardMaterial>,
    already_owned: bool,
) -> Option<bevy::asset::AssetMut<'a, StandardMaterial>> {
    if already_owned {
        return materials.get_mut(handle);
    }
    let base = materials.get(handle)?.clone();
    let new = materials.add(base);
    commands
        .entity(entity)
        .insert((MeshMaterial3d(new.clone()), OwnedMaterial));
    materials.get_mut(&new)
}

/// Spawn region meshes with tile loading setup
pub fn spawn_regions(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    region_query: Query<(Entity, &Region), (Without<RegionMesh>, Without<Prim>)>,
    all_regions: Query<&Region>,
    bigspace: Query<Entity, With<BigSpace>>,
) {
    let Ok(root) = bigspace.single() else {
        return; // BigSpace not spawned yet (camera setup runs first); retry next frame
    };
    // Cheap steady-state early-out: nothing un-meshed to spawn. Avoids the
    // O(regions) `all_regions` scan below on every frame once regions exist.
    if region_query.is_empty() {
        return;
    }
    // Calculate grid size based on total regions
    let total_regions = all_regions.iter().count();
    if total_regions == 0 {
        return; // No regions to spawn
    }

    let grid_size = (total_regions as f32).sqrt().ceil();

    // Collect all region IDs to calculate index
    let mut region_ids: Vec<i64> = all_regions.iter().map(|r| r.id).collect();
    region_ids.sort();

    let mut spawned_count = 0;
    for (entity, region) in region_query.iter() {
        spawned_count += 1;
        let position = if let Some(p) = region.sim_origin {
            p
        } else if total_regions == 1 {
            Vec3::new(0.0, 0.0, 0.0)
        } else {
            let index = region_ids
                .iter()
                .position(|&id| id == region.id)
                .unwrap_or(0);
            let row = (index as f32 / grid_size).floor() as i32;
            let col = index % grid_size as usize;
            let spacing = 300.0;
            Vec3::new(
                (col as f32 - grid_size / 2.0) * spacing,
                0.0,
                (row as f32 - grid_size / 2.0) * spacing,
            )
        };

        tracing::info!(name = %region.name, position = ?position, "spawning region mesh");

        // Flat box acting as the region's ground tile, sized to the tile's real
        // ground extent so it abuts the streamed map tiles (real-metre frame).
        let z = region.tile_z.clamp(0, u32::MAX as i64) as u32;
        let tile_m = tile_to_meters(z, region.latitude) as f32;
        let region_mesh = meshes.add(Cuboid::new(
            tile_m, 0.05, // Very thin - acts like a plane
            tile_m,
        ));

        // Create simple untextured material. Fully matte: default reflectance puts
        // a grazing-angle Fresnel sheen on ground planes, bleaching them at distance.
        let default_material = materials.add(StandardMaterial {
            base_color: Color::srgb(0.7, 0.7, 0.7), // Light gray
            perceptual_roughness: 1.0,
            reflectance: 0.0,
            ..default()
        });

        // Create tile key for this region
        let tile_key = TileKey::new(
            region.tile_x,
            region.tile_y,
            region.tile_z.clamp(0, u32::MAX as i64) as u32,
        );

        // Place under the BigSpace so the region rebases with the camera (ADR-019).
        let (region_cell, region_local) = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD)
            .translation_to_grid(position.as_dvec3());

        commands.entity(entity).insert((
            Mesh3d(region_mesh),
            MeshMaterial3d(default_material),
            Transform::from_translation(region_local),
            region_cell,
            ChildOf(root),
            Visibility::Visible,
            RegionMesh,
            // Hidden from orbit with the other ground props (ADR-025).
            crate::components::HideInSpace,
            RegionTile {
                tile_key,
                lod_level: 1, // Start with medium quality
            },
        ));
    }

    if spawned_count > 0 {
        tracing::info!(count = spawned_count, "spawned region meshes");
    }
}

/// Update region materials when tile textures are loaded
pub fn update_region_materials(
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut region_query: Query<
        (
            &mut MeshMaterial3d<StandardMaterial>,
            &crate::systems::tile_loader::RegionTileTexture,
        ),
        (
            With<RegionMesh>,
            Changed<crate::systems::tile_loader::RegionTileTexture>,
        ),
    >,
    images: Res<Assets<Image>>,
) {
    for (mut material, tile_texture) in region_query.iter_mut() {
        if images.get(&tile_texture.handle).is_some() {
            // Create new material with tile texture (matte, like the untextured one).
            let new_material = materials.add(StandardMaterial {
                base_color_texture: Some(tile_texture.handle.clone()),
                perceptual_roughness: 1.0,
                reflectance: 0.0,
                ..default()
            });
            *material = MeshMaterial3d(new_material);
        }
    }
}

pub fn spawn_prims(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut mesh_cache: ResMut<MeshCache>,
    mut material_cache: ResMut<MaterialCache>,
    prim_query: Query<(Entity, &Prim, &Transform), (Without<PrimMesh>, Without<RegionMesh>)>,
    texture_cache: Res<PrimTextureCache>,
) {
    for (entity, prim, transform) in prim_query.iter() {
        let mesh_handle = cached_prim_mesh(prim, &mut meshes, &mut mesh_cache);
        let material_handle =
            cached_prim_material(prim, &mut materials, &mut material_cache, &texture_cache);

        commands.entity(entity).insert((
            Mesh3d(mesh_handle),
            MeshMaterial3d(material_handle),
            *transform,
            PrimMesh,
        ));
    }
}

/// Rebuild meshes for prims whose geometry (shape/path-cut/hollow) changed.
/// Replaces the whole mesh handle (never mutates in place), so pooled siblings
/// keep the old shared handle and stay batched.
pub fn rebuild_prim_meshes(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mesh_cache: ResMut<MeshCache>,
    query: Query<(Entity, &Prim), With<NeedsMeshRebuild>>,
) {
    for (entity, prim) in query.iter() {
        let mesh_handle = cached_prim_mesh(prim, &mut meshes, &mut mesh_cache);
        commands
            .entity(entity)
            .insert(Mesh3d(mesh_handle))
            .remove::<NeedsMeshRebuild>();
    }
}

/// Swap the material texture for prims that were marked after a `TextureData` message arrived.
pub fn refresh_prim_textures(
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
    query: Query<
        (
            Entity,
            &Prim,
            &MeshMaterial3d<StandardMaterial>,
            Has<OwnedMaterial>,
        ),
        With<NeedsTextureRefresh>,
    >,
    cache: Res<PrimTextureCache>,
) {
    for (entity, prim, mat_handle, owned) in query.iter() {
        // Copy-on-write: a texture swap is a per-prim mutation, so it must not
        // touch the shared pooled material.
        if let Some(mut mat) =
            owned_material(&mut commands, &mut materials, entity, &mat_handle.0, owned)
        {
            mat.base_color_texture = prim
                .texture_id
                .as_ref()
                .and_then(|id| cache.handles.get(id))
                .cloned();
            // Also re-apply the surface so a PrimUpsert that changed transparency/glow/
            // repeats/etc. (not just the texture) is reflected.
            apply_surface(&mut mat, prim.color, &prim.surface);
        }
        commands.entity(entity).remove::<NeedsTextureRefresh>();
    }
}

// ---------------------------------------------------------------------------
// Mesh builders
// ---------------------------------------------------------------------------

fn prim_mesh_handle(prim: &Prim, meshes: &mut Assets<Mesh>) -> Handle<Mesh> {
    let begin = prim.path_cut_begin.clamp(0.0, 1.0);
    let end = prim.path_cut_end.clamp(0.0, 1.0);
    let hollow = prim.hollow.clamp(0.0, 0.95);
    let slice_begin = prim.slice_begin.clamp(0.0, 1.0);
    let slice_end = prim.slice_end.clamp(slice_begin, 1.0);
    let warp = WarpDeform::from_prim(prim);

    let has_cut = begin > 0.001 || end < 0.999 || hollow > 0.001;
    let has_slice = slice_begin > 0.001 || slice_end < 0.999;
    let has_warp = warp.is_active();

    // No deformation at all → use the cheap built-in primitive meshes.
    if !has_cut && !has_slice && !has_warp {
        return match prim.shape {
            PrimShape::Box => meshes.add(Cuboid::new(1.0, 1.0, 1.0)),
            PrimShape::Sphere => meshes.add(Sphere::new(0.5)),
            PrimShape::Cylinder => meshes.add(Cylinder::new(0.5, 1.0)),
            PrimShape::Cone => meshes.add(Cylinder::new(0.0, 1.0)),
            PrimShape::Torus => meshes.add(Torus::default()),
        };
    }

    let mut mesh = match prim.shape {
        PrimShape::Cylinder => {
            build_cylinder_like_mesh(0.5, 0.5, begin, end, hollow, slice_begin, slice_end)
        }
        PrimShape::Cone => {
            build_cylinder_like_mesh(0.0, 0.5, begin, end, hollow, slice_begin, slice_end)
        }
        PrimShape::Box => build_box_mesh(begin, end, hollow, slice_begin, slice_end),
        PrimShape::Sphere => build_sphere_mesh(begin, end, hollow, slice_begin, slice_end),
        // Torus: path-cut/hollow/slice not yet reflected, but warp still applies.
        PrimShape::Torus => Torus::default().mesh().build(),
    };

    if has_warp {
        warp.apply(&mut mesh);
    }
    meshes.add(mesh)
}

/// Twist / taper / top-shear applied as a post-process vertex deformation, so it
/// works uniformly across every base mesh. Each vertex is transformed as a
/// function of its path fraction `f = y + 0.5` (0 at the bottom, 1 at the top).
struct WarpDeform {
    twist_begin: f32, // radians at the bottom
    twist_end: f32,   // radians at the top
    taper_x: f32,
    taper_z: f32,
    shear_x: f32,
    shear_z: f32,
}

impl WarpDeform {
    fn from_prim(p: &Prim) -> Self {
        // SL's profile axes (X, Y) map to our world (X, Z); the path is Y-up.
        Self {
            twist_begin: p.twist_begin.to_radians(),
            twist_end: p.twist_end.to_radians(),
            taper_x: p.taper_x.clamp(-1.0, 1.0),
            taper_z: p.taper_y.clamp(-1.0, 1.0),
            shear_x: p.top_shear_x.clamp(-0.5, 0.5),
            shear_z: p.top_shear_y.clamp(-0.5, 0.5),
        }
    }

    fn is_active(&self) -> bool {
        self.twist_begin.abs() > 1e-4
            || self.twist_end.abs() > 1e-4
            || self.taper_x.abs() > 1e-4
            || self.taper_z.abs() > 1e-4
            || self.shear_x.abs() > 1e-4
            || self.shear_z.abs() > 1e-4
    }

    fn twist_angle(&self, f: f32) -> f32 {
        self.twist_begin + (self.twist_end - self.twist_begin) * f
    }

    fn apply(&self, mesh: &mut Mesh) {
        // Taper scale per axis: +t shrinks the top, −t shrinks the bottom.
        let taper_scale = |t: f32, f: f32| {
            let bot = 1.0 + t.min(0.0);
            let top = 1.0 - t.max(0.0);
            bot + (top - bot) * f
        };

        // Positions: taper → twist → shear (shear stays world-aligned, applied last).
        let mut ys: Vec<f32> = Vec::new();
        if let Some(VertexAttributeValues::Float32x3(pos)) =
            mesh.attribute_mut(Mesh::ATTRIBUTE_POSITION)
        {
            ys.reserve(pos.len());
            for p in pos.iter_mut() {
                let f = (p[1] + 0.5).clamp(0.0, 1.0);
                ys.push(p[1]);
                let mut x = p[0] * taper_scale(self.taper_x, f);
                let mut z = p[2] * taper_scale(self.taper_z, f);
                let (s, c) = self.twist_angle(f).sin_cos();
                let (rx, rz) = (x * c - z * s, x * s + z * c);
                x = rx + self.shear_x * f;
                z = rz + self.shear_z * f;
                p[0] = x;
                p[2] = z;
            }
        } else {
            return;
        }

        // Normals: rotate by the same twist angle (taper/shear tilt is ignored).
        if let Some(VertexAttributeValues::Float32x3(norms)) =
            mesh.attribute_mut(Mesh::ATTRIBUTE_NORMAL)
        {
            for (nrm, &y) in norms.iter_mut().zip(ys.iter()) {
                let f = (y + 0.5).clamp(0.0, 1.0);
                let (s, c) = self.twist_angle(f).sin_cos();
                let (nx, nz) = (nrm[0] * c - nrm[2] * s, nrm[0] * s + nrm[2] * c);
                let len = (nx * nx + nrm[1] * nrm[1] + nz * nz).sqrt().max(1e-6);
                nrm[0] = nx / len;
                nrm[1] /= len;
                nrm[2] = nz / len;
            }
        }
    }
}

/// Cylinder or cone mesh with optional angular path cut, hollow, and slice.
///
/// `top_r_full` = top-face radius at the full path end (0 for cone, 0.5 for cylinder).
/// `bot_r_full` = bottom-face radius at the full path start (always 0.5).
/// `slice_begin`/`slice_end` trim the extrusion along Y; a sliced cone becomes a
/// frustum because the segment radii are interpolated at the slice boundaries.
/// Angles sweep counterclockwise around the Y axis.
fn build_cylinder_like_mesh(
    top_r_full: f32,
    bot_r_full: f32,
    path_cut_begin: f32,
    path_cut_end: f32,
    hollow: f32,
    slice_begin: f32,
    slice_end: f32,
) -> Mesh {
    use std::f32::consts::TAU;

    let segments: usize = 32;
    // Slice trims the path; radii are sampled at the slice boundaries so a cut
    // cone keeps the correct (truncated) profile.
    let radius_at = |f: f32| bot_r_full + (top_r_full - bot_r_full) * f;
    let top_r = radius_at(slice_end);
    let bot_r = radius_at(slice_begin);
    let y_top = slice_end - 0.5;
    let y_bot = slice_begin - 0.5;
    let inner_top_r = top_r * hollow;
    let inner_bot_r = bot_r * hollow;
    let is_hollow = hollow > 0.001;

    let begin_a = path_cut_begin * TAU;
    let end_a = path_cut_end * TAU;
    let sweep = (end_a - begin_a).max(0.0);
    let is_full = sweep >= TAU - 0.001;

    let arc_steps = if is_full {
        segments
    } else {
        ((sweep / TAU * segments as f32).round() as usize).max(1)
    };

    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    // add_quad: two triangles (a,b,c) and (a,c,d).
    let add_quad = |indices: &mut Vec<u32>, a: u32, b: u32, c: u32, d: u32| {
        indices.extend_from_slice(&[a, b, c, a, c, d]);
    };

    // ── Outer wall ────────────────────────────────────────────────────────────
    // Vertices: pairs (top, bot) for each arc step.
    let outer_wall_base = positions.len() as u32;
    for i in 0..=arc_steps {
        let t = i as f32 / arc_steps as f32;
        let a = begin_a + t * sweep;
        let (cos_a, sin_a) = (a.cos(), a.sin());
        let r_top = top_r;
        let r_bot = bot_r;
        // top
        positions.push([r_top * cos_a, y_top, r_top * sin_a]);
        normals.push([cos_a, 0.0, sin_a]);
        uvs.push([t, 0.0]);
        // bottom
        positions.push([r_bot * cos_a, y_bot, r_bot * sin_a]);
        normals.push([cos_a, 0.0, sin_a]);
        uvs.push([t, 1.0]);
    }
    for i in 0..arc_steps {
        let b = outer_wall_base + (i * 2) as u32;
        // verified: top_i, top_{i+1}, bot_{i+1}, bot_i → outward normal
        add_quad(&mut indices, b, b + 2, b + 3, b + 1);
    }

    // ── Inner wall (hollow only) ───────────────────────────────────────────────
    if is_hollow {
        let inner_wall_base = positions.len() as u32;
        for i in 0..=arc_steps {
            let t = i as f32 / arc_steps as f32;
            let a = begin_a + t * sweep;
            let (cos_a, sin_a) = (a.cos(), a.sin());
            // top (inner normal points inward)
            positions.push([inner_top_r * cos_a, y_top, inner_top_r * sin_a]);
            normals.push([-cos_a, 0.0, -sin_a]);
            uvs.push([t, 0.0]);
            // bottom
            positions.push([inner_bot_r * cos_a, y_bot, inner_bot_r * sin_a]);
            normals.push([-cos_a, 0.0, -sin_a]);
            uvs.push([t, 1.0]);
        }
        for i in 0..arc_steps {
            let b = inner_wall_base + (i * 2) as u32;
            // reversed winding for inward normal
            add_quad(&mut indices, b, b + 1, b + 3, b + 2);
        }
    }

    // ── Top cap ───────────────────────────────────────────────────────────────
    if top_r > 0.001 {
        if !is_hollow {
            // Fan from center
            let center = positions.len() as u32;
            positions.push([0.0, y_top, 0.0]);
            normals.push([0.0, 1.0, 0.0]);
            uvs.push([0.5, 0.5]);
            let ring_start = positions.len() as u32;
            for i in 0..=arc_steps {
                let t = i as f32 / arc_steps as f32;
                let a = begin_a + t * sweep;
                let (cos_a, sin_a) = (a.cos(), a.sin());
                positions.push([top_r * cos_a, y_top, top_r * sin_a]);
                normals.push([0.0, 1.0, 0.0]);
                uvs.push([0.5 + 0.5 * cos_a, 0.5 + 0.5 * sin_a]);
            }
            for i in 0..arc_steps {
                let v1 = ring_start + i as u32;
                let v2 = ring_start + i as u32 + 1;
                // reversed winding → upward normal (verified above)
                indices.extend_from_slice(&[center, v2, v1]);
            }
        } else {
            // Annulus
            let ann_base = positions.len() as u32;
            for i in 0..=arc_steps {
                let t = i as f32 / arc_steps as f32;
                let a = begin_a + t * sweep;
                let (cos_a, sin_a) = (a.cos(), a.sin());
                positions.push([top_r * cos_a, y_top, top_r * sin_a]);
                normals.push([0.0, 1.0, 0.0]);
                uvs.push([0.5 + 0.5 * cos_a, 0.5 + 0.5 * sin_a]);
                positions.push([inner_top_r * cos_a, y_top, inner_top_r * sin_a]);
                normals.push([0.0, 1.0, 0.0]);
                uvs.push([0.5 + inner_top_r * cos_a, 0.5 + inner_top_r * sin_a]);
            }
            for i in 0..arc_steps {
                let b = ann_base + (i * 2) as u32;
                // outer_i, inner_i, inner_{i+1}, outer_{i+1} → upward normal (verified)
                add_quad(&mut indices, b, b + 1, b + 3, b + 2);
            }
        }
    }

    // ── Bottom cap ────────────────────────────────────────────────────────────
    {
        if !is_hollow {
            let center = positions.len() as u32;
            positions.push([0.0, y_bot, 0.0]);
            normals.push([0.0, -1.0, 0.0]);
            uvs.push([0.5, 0.5]);
            let ring_start = positions.len() as u32;
            for i in 0..=arc_steps {
                let t = i as f32 / arc_steps as f32;
                let a = begin_a + t * sweep;
                let (cos_a, sin_a) = (a.cos(), a.sin());
                positions.push([bot_r * cos_a, y_bot, bot_r * sin_a]);
                normals.push([0.0, -1.0, 0.0]);
                uvs.push([0.5 + 0.5 * cos_a, 0.5 + 0.5 * sin_a]);
            }
            for i in 0..arc_steps {
                let v1 = ring_start + i as u32;
                let v2 = ring_start + i as u32 + 1;
                // forward winding → downward normal (verified)
                indices.extend_from_slice(&[center, v1, v2]);
            }
        } else {
            let ann_base = positions.len() as u32;
            for i in 0..=arc_steps {
                let t = i as f32 / arc_steps as f32;
                let a = begin_a + t * sweep;
                let (cos_a, sin_a) = (a.cos(), a.sin());
                positions.push([bot_r * cos_a, y_bot, bot_r * sin_a]);
                normals.push([0.0, -1.0, 0.0]);
                uvs.push([0.5 + 0.5 * cos_a, 0.5 + 0.5 * sin_a]);
                positions.push([inner_bot_r * cos_a, y_bot, inner_bot_r * sin_a]);
                normals.push([0.0, -1.0, 0.0]);
                uvs.push([0.5 + inner_bot_r * cos_a, 0.5 + inner_bot_r * sin_a]);
            }
            for i in 0..arc_steps {
                let b = ann_base + (i * 2) as u32;
                // reversed for downward normal
                add_quad(&mut indices, b, b + 2, b + 3, b + 1);
            }
        }
    }

    // ── Cut caps (only when path is not a full circle) ────────────────────────
    if !is_full {
        // Start cap at begin_a — normal = [sin(a), 0, -cos(a)]
        push_cut_cap(
            &mut positions,
            &mut normals,
            &mut uvs,
            &mut indices,
            begin_a,
            top_r,
            bot_r,
            inner_top_r,
            inner_bot_r,
            y_top,
            y_bot,
            is_hollow,
            true,
        );
        // End cap at end_a — normal = [-sin(a), 0, cos(a)]
        push_cut_cap(
            &mut positions,
            &mut normals,
            &mut uvs,
            &mut indices,
            end_a,
            top_r,
            bot_r,
            inner_top_r,
            inner_bot_r,
            y_top,
            y_bot,
            is_hollow,
            false,
        );
    }

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

/// Push one cut-face quad (or triangle if top_r≈0) at a given angle.
fn push_cut_cap(
    positions: &mut Vec<[f32; 3]>,
    normals: &mut Vec<[f32; 3]>,
    uvs: &mut Vec<[f32; 2]>,
    indices: &mut Vec<u32>,
    angle: f32,
    top_r: f32,
    bot_r: f32,
    inner_top_r: f32,
    inner_bot_r: f32,
    y_top: f32,
    y_bot: f32,
    is_hollow: bool,
    is_start: bool,
) {
    let (cos_a, sin_a) = (angle.cos(), angle.sin());
    let (nx, nz) = if is_start {
        (sin_a, -cos_a)
    } else {
        (-sin_a, cos_a)
    };
    let n = [nx, 0.0, nz];

    let base = positions.len() as u32;

    // 4 vertices: outer_top, outer_bot, inner_top, inner_bot
    // (inner = center axis when not hollow)
    let (it_x, it_z) = if is_hollow {
        (inner_top_r * cos_a, inner_top_r * sin_a)
    } else {
        (0.0, 0.0)
    };
    let (ib_x, ib_z) = if is_hollow {
        (inner_bot_r * cos_a, inner_bot_r * sin_a)
    } else {
        (0.0, 0.0)
    };
    let (ot_x, ot_z) = (top_r * cos_a, top_r * sin_a);
    let (ob_x, ob_z) = (bot_r * cos_a, bot_r * sin_a);

    // outer_top (0)
    positions.push([ot_x, y_top, ot_z]);
    normals.push(n);
    uvs.push([1.0, 0.0]);
    // outer_bot (1)
    positions.push([ob_x, y_bot, ob_z]);
    normals.push(n);
    uvs.push([1.0, 1.0]);
    // inner_bot (2)
    positions.push([ib_x, y_bot, ib_z]);
    normals.push(n);
    uvs.push([0.0, 1.0]);
    // inner_top (3)
    positions.push([it_x, y_top, it_z]);
    normals.push(n);
    uvs.push([0.0, 0.0]);

    // Winding verified for start cap: add_quad(outer_top, outer_bot, inner_bot, inner_top)
    // For end cap (reversed direction): add_quad(inner_top, inner_bot, outer_bot, outer_top)
    if is_start {
        // [base, base+1, base+2] and [base, base+2, base+3]
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    } else {
        // [base+3, base+2, base+1] and [base+3, base+1, base]
        indices.extend_from_slice(&[base + 3, base + 2, base + 1, base + 3, base + 1, base]);
    }
}

/// Box mesh with optional angular path cut (a wedge sliced through the vertical
/// axis) and hollow (an inner square shell). The square cross-section is clipped
/// to the angular sector `[begin, end]` (turns, 0..1) measured CCW around the Y
/// axis — the same path-cut convention used for the cylinder.
fn build_box_mesh(
    path_cut_begin: f32,
    path_cut_end: f32,
    hollow: f32,
    slice_begin: f32,
    slice_end: f32,
) -> Mesh {
    use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, TAU};

    let is_hollow = hollow > 0.001;
    let begin_a = path_cut_begin * TAU;
    let end_a = path_cut_end * TAU;
    let sweep = (end_a - begin_a).max(0.0);
    let is_full = sweep >= TAU - 0.001;

    let outer_half = 0.5_f32;
    let inner_half = outer_half * hollow;

    // Point where a ray at `angle` from the origin hits a square of the given
    // half-extent, in the XZ plane.
    let square_pt = |angle: f32, half: f32| -> Vec3 {
        let (s, c) = angle.sin_cos();
        let m = c.abs().max(s.abs()).max(1e-6);
        Vec3::new(half * c / m, 0.0, half * s / m)
    };

    // Sample angles: the cut start/end plus every square corner strictly inside
    // the sweep. Corners sit on the diagonals (PI/4 + k·PI/2); splitting there
    // keeps each wall segment flat on a single face.
    let mut angles: Vec<f32> = vec![begin_a];
    for k in 0..4 {
        let corner = FRAC_PI_4 + k as f32 * FRAC_PI_2;
        if corner > begin_a + 1e-4 && corner < end_a - 1e-4 {
            angles.push(corner);
        }
    }
    angles.push(end_a);

    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    // Slice trims the extrusion along Y (the box is uniform, so only the caps move).
    let (y_top, y_bot) = (slice_end - 0.5, slice_begin - 0.5);

    // Outward normal of the square edge that a wall segment lies on.
    let face_normal = |mid: Vec3| -> Vec3 {
        if mid.x.abs() >= mid.z.abs() {
            Vec3::new(mid.x.signum(), 0.0, 0.0)
        } else {
            Vec3::new(0.0, 0.0, mid.z.signum())
        }
    };

    // One wall + cap pair per consecutive angle segment.
    for w in angles.windows(2) {
        let (a0, a1) = (w[0], w[1]);
        let o0 = square_pt(a0, outer_half);
        let o1 = square_pt(a1, outer_half);
        let ot0 = Vec3::new(o0.x, y_top, o0.z);
        let ot1 = Vec3::new(o1.x, y_top, o1.z);
        let ob0 = Vec3::new(o0.x, y_bot, o0.z);
        let ob1 = Vec3::new(o1.x, y_bot, o1.z);

        let n_out = face_normal((o0 + o1) * 0.5);
        push_quad(
            &mut positions,
            &mut normals,
            &mut uvs,
            &mut indices,
            [ot0, ot1, ob1, ob0],
            n_out,
        );

        if is_hollow {
            let i0 = square_pt(a0, inner_half);
            let i1 = square_pt(a1, inner_half);
            let it0 = Vec3::new(i0.x, y_top, i0.z);
            let it1 = Vec3::new(i1.x, y_top, i1.z);
            let ib0 = Vec3::new(i0.x, y_bot, i0.z);
            let ib1 = Vec3::new(i1.x, y_bot, i1.z);

            // Inner wall (normal points inward) + top/bottom rings.
            push_quad(
                &mut positions,
                &mut normals,
                &mut uvs,
                &mut indices,
                [it0, it1, ib1, ib0],
                -n_out,
            );
            push_quad(
                &mut positions,
                &mut normals,
                &mut uvs,
                &mut indices,
                [ot0, it0, it1, ot1],
                Vec3::Y,
            );
            push_quad(
                &mut positions,
                &mut normals,
                &mut uvs,
                &mut indices,
                [ob0, ib0, ib1, ob1],
                Vec3::NEG_Y,
            );
        } else {
            // Solid caps: fan from the central axis.
            let ct = Vec3::new(0.0, y_top, 0.0);
            let cb = Vec3::new(0.0, y_bot, 0.0);
            push_tri(
                &mut positions,
                &mut normals,
                &mut uvs,
                &mut indices,
                [ct, ot0, ot1],
                Vec3::Y,
            );
            push_tri(
                &mut positions,
                &mut normals,
                &mut uvs,
                &mut indices,
                [cb, ob0, ob1],
                Vec3::NEG_Y,
            );
        }
    }

    // Cut faces close the wedge along the begin/end meridians.
    if !is_full {
        for (angle, sign) in [(begin_a, 1.0_f32), (end_a, -1.0_f32)] {
            let (s, c) = angle.sin_cos();
            let n = Vec3::new(s * sign, 0.0, -c * sign);
            let o = square_pt(angle, outer_half);
            let ot = Vec3::new(o.x, y_top, o.z);
            let ob = Vec3::new(o.x, y_bot, o.z);
            if is_hollow {
                let i = square_pt(angle, inner_half);
                let it = Vec3::new(i.x, y_top, i.z);
                let ib = Vec3::new(i.x, y_bot, i.z);
                push_quad(
                    &mut positions,
                    &mut normals,
                    &mut uvs,
                    &mut indices,
                    [ot, ob, ib, it],
                    n,
                );
            } else {
                let ct = Vec3::new(0.0, y_top, 0.0);
                let cb = Vec3::new(0.0, y_bot, 0.0);
                push_quad(
                    &mut positions,
                    &mut normals,
                    &mut uvs,
                    &mut indices,
                    [ot, ob, cb, ct],
                    n,
                );
            }
        }
    }

    finish_mesh(positions, normals, uvs, indices)
}

/// UV-sphere mesh with optional angular path cut (a longitudinal wedge), hollow
/// (an inner sphere shell), and slice (a latitude trim / dimple). Longitude is
/// swept over `[begin, end]` turns CCW about the Y axis; latitude is trimmed to
/// the slice band (`slice` 0 = south pole, 1 = north pole).
fn build_sphere_mesh(
    path_cut_begin: f32,
    path_cut_end: f32,
    hollow: f32,
    slice_begin: f32,
    slice_end: f32,
) -> Mesh {
    use std::f32::consts::{PI, TAU};

    let stacks: usize = 16; // latitude divisions
    let r = 0.5_f32;
    let inner_r = r * hollow;
    let is_hollow = hollow > 0.001;

    // Slice trims latitude. Path fraction 0 = south pole (lat = PI), 1 = north
    // pole (lat = 0), matching the box/cylinder convention (0 = bottom).
    let is_sliced = slice_begin > 0.001 || slice_end < 0.999;
    let lat_min = PI * (1.0 - slice_end); // top boundary (smaller latitude)
    let lat_max = PI * (1.0 - slice_begin); // bottom boundary
    let lat_span = (lat_max - lat_min).max(0.0);

    let begin_a = path_cut_begin * TAU;
    let end_a = path_cut_end * TAU;
    let sweep = (end_a - begin_a).max(0.0);
    let is_full = sweep >= TAU - 0.001;
    let slices = if is_full {
        32
    } else {
        ((sweep / TAU * 32.0).round() as usize).max(1)
    };

    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    // Build one sphere shell over the swept region (outward or inward facing).
    let surface = |positions: &mut Vec<[f32; 3]>,
                   normals: &mut Vec<[f32; 3]>,
                   uvs: &mut Vec<[f32; 2]>,
                   indices: &mut Vec<u32>,
                   rad: f32,
                   outward: bool| {
        let base = positions.len() as u32;
        for i in 0..=stacks {
            let v = i as f32 / stacks as f32;
            let lat = lat_min + v * lat_span;
            let (slat, clat) = lat.sin_cos();
            for j in 0..=slices {
                let t = j as f32 / slices as f32;
                let lon = begin_a + t * sweep;
                let (slon, clon) = lon.sin_cos();
                let dir = Vec3::new(slat * clon, clat, slat * slon);
                positions.push((dir * rad).to_array());
                normals.push(if outward { dir } else { -dir }.to_array());
                uvs.push([t, v]);
            }
        }
        let row = (slices + 1) as u32;
        for i in 0..stacks as u32 {
            for j in 0..slices as u32 {
                let v00 = base + i * row + j;
                let v01 = v00 + 1;
                let v10 = v00 + row;
                let v11 = v10 + 1;
                if outward {
                    indices.extend_from_slice(&[v00, v01, v11, v00, v11, v10]);
                } else {
                    indices.extend_from_slice(&[v00, v11, v01, v00, v10, v11]);
                }
            }
        }
    };

    surface(
        &mut positions,
        &mut normals,
        &mut uvs,
        &mut indices,
        r,
        true,
    );
    if is_hollow {
        surface(
            &mut positions,
            &mut normals,
            &mut uvs,
            &mut indices,
            inner_r,
            false,
        );
    }

    // Cut faces: a meridian half-disc (or half-annulus when hollow) at each end.
    if !is_full {
        for (angle, sign) in [(begin_a, 1.0_f32), (end_a, -1.0_f32)] {
            let (s, c) = angle.sin_cos();
            let n = Vec3::new(s * sign, 0.0, -c * sign);
            let merid = |rad: f32, i: usize| -> Vec3 {
                let lat = lat_min + i as f32 / stacks as f32 * lat_span;
                let (slat, clat) = lat.sin_cos();
                Vec3::new(slat * c, clat, slat * s) * rad
            };
            for i in 0..stacks {
                let o0 = merid(r, i);
                let o1 = merid(r, i + 1);
                let (inner0, inner1) = if is_hollow {
                    (merid(inner_r, i), merid(inner_r, i + 1))
                } else {
                    // Collapse to the central axis at the matching latitude.
                    (Vec3::new(0.0, o0.y, 0.0), Vec3::new(0.0, o1.y, 0.0))
                };
                push_quad(
                    &mut positions,
                    &mut normals,
                    &mut uvs,
                    &mut indices,
                    [o0, o1, inner1, inner0],
                    n,
                );
            }
        }
    }

    // Slice caps: flat disc (or annulus when hollow) closing each trimmed
    // latitude boundary, swept over the cut longitude range.
    if is_sliced {
        for (lat, up) in [(lat_min, true), (lat_max, false)] {
            let (slat, clat) = lat.sin_cos();
            let ring_r = r * slat;
            if ring_r <= 1e-4 {
                continue; // boundary is a pole — nothing to cap
            }
            let y = r * clat;
            let inner_ring_r = inner_r * slat;
            let n = if up { Vec3::Y } else { Vec3::NEG_Y };
            for k in 0..slices {
                let a0 = begin_a + k as f32 / slices as f32 * sweep;
                let a1 = begin_a + (k + 1) as f32 / slices as f32 * sweep;
                let o0 = Vec3::new(ring_r * a0.cos(), y, ring_r * a0.sin());
                let o1 = Vec3::new(ring_r * a1.cos(), y, ring_r * a1.sin());
                if is_hollow {
                    let i0 = Vec3::new(inner_ring_r * a0.cos(), y, inner_ring_r * a0.sin());
                    let i1 = Vec3::new(inner_ring_r * a1.cos(), y, inner_ring_r * a1.sin());
                    push_quad(
                        &mut positions,
                        &mut normals,
                        &mut uvs,
                        &mut indices,
                        [o0, o1, i1, i0],
                        n,
                    );
                } else {
                    let center = Vec3::new(0.0, y, 0.0);
                    push_tri(
                        &mut positions,
                        &mut normals,
                        &mut uvs,
                        &mut indices,
                        [center, o0, o1],
                        n,
                    );
                }
            }
        }
    }

    finish_mesh(positions, normals, uvs, indices)
}

/// Push a flat quad (4 coplanar verts) auto-wound so its front face faces `n`.
fn push_quad(
    positions: &mut Vec<[f32; 3]>,
    normals: &mut Vec<[f32; 3]>,
    uvs: &mut Vec<[f32; 2]>,
    indices: &mut Vec<u32>,
    verts: [Vec3; 4],
    n: Vec3,
) {
    let facing = (verts[1] - verts[0]).cross(verts[2] - verts[0]).dot(n) >= 0.0;
    let order = if facing { [0, 1, 2, 3] } else { [0, 3, 2, 1] };
    let uv = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
    let base = positions.len() as u32;
    for &k in &order {
        positions.push(verts[k].to_array());
        normals.push(n.to_array());
        uvs.push(uv[k]);
    }
    indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

/// Push a flat triangle auto-wound so its front face faces `n`.
fn push_tri(
    positions: &mut Vec<[f32; 3]>,
    normals: &mut Vec<[f32; 3]>,
    uvs: &mut Vec<[f32; 2]>,
    indices: &mut Vec<u32>,
    verts: [Vec3; 3],
    n: Vec3,
) {
    let facing = (verts[1] - verts[0]).cross(verts[2] - verts[0]).dot(n) >= 0.0;
    let order = if facing { [0, 1, 2] } else { [0, 2, 1] };
    let uv = [[0.5, 0.0], [0.0, 1.0], [1.0, 1.0]];
    let base = positions.len() as u32;
    for &k in &order {
        positions.push(verts[k].to_array());
        normals.push(n.to_array());
        uvs.push(uv[k]);
    }
    indices.extend_from_slice(&[base, base + 1, base + 2]);
}

fn finish_mesh(
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<u32>,
) -> Mesh {
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

#[cfg(test)]
mod tests {}
