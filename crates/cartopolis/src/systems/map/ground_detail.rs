//! **Sub-metre ground**: the half-metre tier's slope, as a normal every ground
//! surface shades by.
//!
//! `systems::terrain`'s field is 3.125 m and the meshes standing on it carry a vertex
//! every [`MAX_GROUND_EDGE_M`](crate::systems::terrain::MAX_GROUND_EDGE_M). That is
//! what gives a Dutch street a gradient at all — measured over Groningen, the median
//! facet tilt goes from 0.46° at the old 25 m sampling to 1.34° — and it is *not* what
//! gives it a kerb.
//!
//! AHN measures the ground at half a metre and the fine tier already fetches it
//! (`terrain::FINE_GRID`, 1024² over 512 m, following the camera). What could not
//! follow was the geometry: `map_geometry` builds its surfaces per z14 cell, ~1471 m
//! across, so half-metre vertices over one of them is about nine million quads,
//! rebuilt on every re-anchor. There is no budget in which that is drawn.
//!
//! So this is the other half of the same answer `water.wgsl` gives: a 12 cm kerb, the
//! camber of a carriageway, the lip of a quay and a rut across a verge are **shading,
//! not silhouette**. The tier's slope goes into a texture, the ground materials
//! perturb their normal by it, and the cost is one fetch per ground fragment and no
//! triangles at all.
//!
//! # Three things are load-bearing
//!
//! **The texture is a residual.** `terrain::encode_slope` subtracts a box blur of the
//! slope at the mesh's own edge length, so what is left is exactly what the geometry
//! cannot express. Adding the raw slope would apply every gradient twice — once in the
//! vertices, once in the normal — and a hillside would shade as if half again as
//! steep.
//!
//! **The uniform is in render space and is rewritten, but rarely.** Under `big_space`
//! the render origin jumps a whole `WORLD_CELL_EDGE` as the camera crosses a grid
//! cell, which is the trap `building_windows` documents. That one rewrites its ring
//! every frame because there is a single material; here there are hundreds, so
//! [`refresh_ground_detail`] rewrites only when the box re-centres (every ~96 m of
//! walking) or the origin actually moves.
//!
//! **Strength zero is the whole off-switch.** With the Height layer off, outside the
//! Netherlands, or while the tier is in flight, `params.w` is 0 and the shader is the
//! plain PBR path plus one comparison.

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, Extent3d, TextureDimension, TextureFormat};
use bevy::shader::ShaderRef;

use crate::components::FreeCamera;
use crate::grid::WORLD_CELL_EDGE;
use crate::systems::terrain::TerrainField;

/// The ground material every flat surface and every relief tile carries.
pub type GroundMaterial = ExtendedMaterial<StandardMaterial, GroundDetailExtension>;

/// The sub-metre half of the ground, for `shaders/ground_detail.wgsl`.
#[derive(Asset, AsBindGroup, Reflect, Clone, Debug, Default)]
pub struct GroundDetailExtension {
    /// xy: the detail box's centre in **render-space** XZ. z: 1 / its half-extent.
    /// w: strength, and `0.0` means "no tier" — see the module docs.
    #[uniform(100)]
    pub params: Vec4,
    /// The tier's residual slope, `Rg8Snorm`. Shared by every ground material, so a
    /// re-bake rewrites one image rather than hundreds.
    #[texture(101)]
    #[sampler(102)]
    pub slope: Option<Handle<Image>>,
    /// The clipmap level's ring, for the level-to-level cross-fade: xy its centre in
    /// **render-space** XZ, z `1 /` its half-extent, w the mip bias to reach at its
    /// edge. [`crate::systems::map_stream::refresh_tile_lod_blend`] writes it and
    /// says what it is for.
    ///
    /// `w = 0` is the off switch and is what every ground surface that is not a
    /// clipmap tile carries — the vector surfaces, the roads, the paving, the
    /// volumetric groups. The shader takes no second sample at all there.
    #[uniform(103)]
    pub lod: Vec4,
    /// The tile's *own* basemap image, bound a second time so the shader can sample
    /// it at a mip bias.
    ///
    /// The same handle `base_color_texture` already holds, so this costs a binding
    /// and no memory. Bound here rather than reached for through
    /// `bevy_pbr::pbr_bindings`, whose base-colour binding is `#ifdef BINDLESS`-shaped
    /// and is bevy's to rearrange; this one is ours and is the same on every platform.
    #[texture(104)]
    #[sampler(105)]
    pub basemap: Option<Handle<Image>>,
}

impl MaterialExtension for GroundDetailExtension {
    fn fragment_shader() -> ShaderRef {
        "shaders/ground_detail.wgsl".into()
    }
}

/// The shared slope texture and the box it covers.
#[derive(Resource, Default)]
pub struct GroundDetail {
    /// Created on the first bake and then rewritten in place, so the handle every
    /// material holds stays valid and only [`GroundDetailExtension::params`] moves.
    pub slope: Option<Handle<Image>>,
    /// Centre of the box in the **anchored world** frame, and its half-extent.
    centre: Vec2,
    half_m: f32,
    /// Metres between texels — what `--dump-state` reports, so a capture can state
    /// that the ground is shading at half a metre rather than leaving it to the eye
    /// on a software rasteriser, where a lighting term is not evidence.
    pub sample_m: f32,
    /// Bumped whenever the box moves, which is what [`refresh_ground_detail`] watches.
    revision: u32,
}

impl GroundDetail {
    /// What a new ground material should be built with.
    #[must_use]
    pub fn extension(&self, origin: &GridCellOrigin) -> GroundDetailExtension {
        GroundDetailExtension {
            params: self.params(origin),
            slope: self.slope.clone(),
            // The clipmap's own; a caller that is a tile fills these in after
            // (`map_stream::apply_stream_textures`), and everything else leaves
            // them off.
            ..default()
        }
    }

    fn params(&self, origin: &GridCellOrigin) -> Vec4 {
        if self.slope.is_none() || self.half_m <= 0.0 {
            return Vec4::ZERO;
        }
        Vec4::new(
            self.centre.x - origin.0.x,
            self.centre.y - origin.0.y,
            1.0 / self.half_m,
            DETAIL_STRENGTH,
        )
    }
}

/// The `params` a surface that must never take the detail is created with.
///
/// The volumetric groups — canopy, trunk, street furniture — share this material type
/// so there are not two, but a tree's normals describe the tree, not the hill.
/// [`refresh_ground_detail`] leaves any material with a negative strength alone.
pub const DETAIL_OPTED_OUT: Vec4 = Vec4::new(0.0, 0.0, 0.0, -1.0);

/// How much of the measured residual slope reaches the normal.
///
/// One, and it is written down rather than left implicit: the texture is a
/// measurement, not a look, and anything but 1.0 here would be a quiet decision that
/// AHN is wrong about the ground. It exists as a constant so a future *quality* rung
/// has something to turn down, not so the picture can be tuned.
const DETAIL_STRENGTH: f32 = 1.0;

/// Where render space currently sits in the anchored world frame.
///
/// A resource rather than a query at each site, because two systems need the same
/// answer on the same frame and getting it from different places is how the uniform
/// and the geometry come to disagree by a whole grid cell.
#[derive(Resource, Default)]
pub struct GridCellOrigin(pub Vec2);

/// Track the floating origin, so the render-space uniform can be written.
pub fn track_grid_origin(
    mut origin: ResMut<GridCellOrigin>,
    camera: Query<&big_space::prelude::GridCell, With<FreeCamera>>,
) {
    let Ok(cell) = camera.single() else {
        return;
    };
    let want = Vec2::new(cell.x as f32, cell.z as f32) * WORLD_CELL_EDGE;
    if origin.0 != want {
        origin.0 = want;
    }
}

/// Whether the layer is on at all.
///
/// `CARTO_NO_GROUND_DETAIL=1` strips it, on the same rule as `CARTO_NO_ATMOSPHERE`
/// and the rest: a shading term is the hardest kind of change to attribute, and the
/// only honest A/B is the same scene with the term removed.
#[must_use]
pub fn enabled() -> bool {
    !matches!(
        std::env::var("CARTO_NO_GROUND_DETAIL").as_deref(),
        Ok("1") | Ok("true")
    )
}

/// Fade the slope out where the ground is filled back over a lid or beside a deep
/// cutting (`Tunnels::fill_weight`).
///
/// The half-metre tier is the same survey the fill corrects — flown when the ring
/// was an open trench — so its slopes are the trench's sides, and shaded onto the
/// flat ground over a lid they drew a crumpled ditch that was not there: gone with
/// `CARTO_NO_GROUND_DETAIL=1` in the 2026-09-23 captures. The vector ground's own
/// normals are already flattened by the same weight (`map_geometry`).
///
/// Only the texels inside a filled tunnel's box are visited, so a box with no
/// tunnel in it — nearly all of them — costs one query.
fn flatten_over_fills(
    bytes: &mut [u8],
    grid: usize,
    centre: Vec2,
    half_m: f32,
    tunnels: &crate::systems::tunnels::Tunnels,
) {
    if grid < 2 {
        return;
    }
    let Some(b) = tunnels.filled_box_near(centre, half_m, cartopolis_geo::tunnels::DATUM_M) else {
        return;
    };
    let sample = half_m * 2.0 / (grid - 1) as f32;
    let origin = centre - Vec2::splat(half_m);
    let index = |v: f32, o: f32| (((v - o) / sample).floor().max(0.0) as usize).min(grid - 1);
    let (x0, x1) = (index(b[0], origin.x), index(b[2], origin.x));
    let (z0, z1) = (index(b[1], origin.y), index(b[3], origin.y));
    for z in z0..=z1 {
        for x in x0..=x1 {
            let p = origin + Vec2::new(x as f32 * sample, z as f32 * sample);
            let w = tunnels.fill_weight(p, cartopolis_geo::tunnels::DATUM_M);
            if w <= 0.0 {
                continue;
            }
            let i = (z * grid + x) * 2;
            for b in &mut bytes[i..i + 2] {
                let v = f32::from(*b as i8) * (1.0 - w);
                *b = (v.round() as i8) as u8;
            }
        }
    }
}

/// Take a finished tier bake and put its slope into the shared image.
pub fn receive_ground_detail(
    mut detail: ResMut<GroundDetail>,
    mut field: ResMut<TerrainField>,
    mut images: ResMut<Assets<Image>>,
    tunnels: Res<crate::systems::tunnels::Tunnels>,
) {
    let Some((mut bytes, grid, centre, half_m)) = field.take_fine_slope() else {
        return;
    };
    if !enabled() {
        return;
    }
    flatten_over_fills(&mut bytes, grid, centre, half_m, &tunnels);
    detail.centre = centre;
    detail.half_m = half_m;
    detail.sample_m = if grid > 1 {
        half_m * 2.0 / (grid - 1) as f32
    } else {
        0.0
    };
    detail.revision = detail.revision.wrapping_add(1);

    if let Some(handle) = detail.slope.clone() {
        // Rewritten in place: every ground material already holds this handle, and
        // handing out a new one would mean rebuilding hundreds of bind groups for a
        // buffer whose size never changes.
        if let Some(mut image) = images.get_mut(&handle) {
            image.data = Some(bytes);
            return;
        }
    }
    let mut image = Image::new(
        Extent3d {
            width: grid as u32,
            height: grid as u32,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        bytes,
        TextureFormat::Rg8Snorm,
        // `RENDER_WORLD` only: nothing reads it back, and the CPU copy of a megabyte
        // per re-centre is the kind of quiet resident cost `upload_meter` exists to
        // catch.
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        // Linear, because the point of the texture is a *slope* and a nearest fetch
        // would draw the tier's own half-metre lattice on the ground.
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        // The shader fades the box out before its own rim, so the edge is never
        // sampled at strength — but clamping rather than repeating is what stops a
        // lookup that lands a texel outside from wrapping in a kerb from the far side.
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        ..default()
    });
    detail.slope = Some(images.add(image));
}

/// Rewrite the render-space uniform on every ground material, when it has moved.
///
/// Two triggers and no third: the box re-centres, or the floating origin does. Both
/// are rare — a few times a minute on foot at worst — which is what makes touching
/// every ground material affordable at all. Doing it per frame, as `building_windows`
/// can afford to with its single material, would re-prepare hundreds of bind groups
/// forever.
pub fn refresh_ground_detail(
    detail: Res<GroundDetail>,
    origin: Res<GridCellOrigin>,
    mut materials: ResMut<Assets<GroundMaterial>>,
    mut roads: ResMut<Assets<crate::systems::road_depth::RoadMaterial>>,
    mut applied: Local<Option<(u32, Vec2, usize)>>,
) {
    // The count is the third of the three, and it is not padding: a ground material
    // created *after* the last write — a cell that streamed in, a tile that landed —
    // would otherwise keep the zero strength it was built with until the box happened
    // to move again. `wet_surfaces::DryLook` guards itself the same way and for the
    // same reason.
    let want = (detail.revision, origin.0, materials.len() + roads.len());
    if *applied == Some(want) {
        return;
    }
    if materials.is_empty() && roads.is_empty() {
        return;
    }
    *applied = Some(want);
    let params = detail.params(&origin);
    let slope = detail.slope.clone();
    let ids: Vec<AssetId<GroundMaterial>> = materials.ids().collect();
    for id in ids {
        let Some(mut material) = materials.get_mut(id) else {
            continue;
        };
        // Opted out at creation and never overwritten — see [`DETAIL_OPTED_OUT`].
        if material.extension.params.w < 0.0 {
            continue;
        }
        material.extension.params = params;
        if material.extension.slope != slope {
            material.extension.slope = slope.clone();
        }
    }
    // The road groups carry the same three bindings on their own extension, because
    // they need a vertex stage of their own and cannot share the material type.
    let road_ids: Vec<AssetId<crate::systems::road_depth::RoadMaterial>> = roads.ids().collect();
    for id in road_ids {
        let Some(mut material) = roads.get_mut(id) else {
            continue;
        };
        material.extension.detail = params;
        if material.extension.slope != slope {
            material.extension.slope = slope.clone();
        }
    }
}
