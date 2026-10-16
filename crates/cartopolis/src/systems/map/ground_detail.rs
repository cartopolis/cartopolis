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
//! So this is the other half of the same answer `water.wesl` gives: a 12 cm kerb, the
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
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use crate::components::FreeCamera;
use crate::grid::WORLD_CELL_EDGE;
use crate::systems::terrain::TerrainField;

// The ground material lives in `cartopolis_materials`, so that Bevy's
// per-material plugin code is generated there rather than in this crate.
pub use cartopolis_materials::ground_detail::{GroundDetailExtension, GroundMaterial};

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
    // Every write is guarded by a read. `get_mut` alone marks a material changed,
    // a changed material is re-prepared, and the count above moves with every
    // tile that lands — so writing unconditionally rebuilt *every* ground bind
    // group once per landing, all through a load — with the same write in
    // `map_stream::refresh_tile_lod_blend`, 6.6–8.5 s of a 75 s load's main-thread
    // time on the web build, and 25 ms once both read first
    // (`docs/notes/wasm-performance.md`).
    let ids: Vec<AssetId<GroundMaterial>> = materials.ids().collect();
    for id in ids {
        let Some(material) = materials.get(id) else {
            continue;
        };
        // Opted out at creation and never overwritten — see [`DETAIL_OPTED_OUT`].
        if material.extension.params.w < 0.0
            || (material.extension.params == params && material.extension.slope == slope)
        {
            continue;
        }
        if let Some(mut material) = materials.get_mut(id) {
            material.extension.params = params;
            material.extension.slope = slope.clone();
        }
    }
    // The road groups carry the same three bindings on their own extension, because
    // they need a vertex stage of their own and cannot share the material type.
    let road_ids: Vec<AssetId<crate::systems::road_depth::RoadMaterial>> = roads.ids().collect();
    for id in road_ids {
        if roads
            .get(id)
            .is_none_or(|m| m.extension.detail == params && m.extension.slope == slope)
        {
            continue;
        }
        if let Some(mut material) = roads.get_mut(id) {
            material.extension.detail = params;
            material.extension.slope = slope.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::systems::road_depth::RoadMaterial;

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Image>()
            .init_asset::<GroundMaterial>()
            .init_asset::<RoadMaterial>()
            .init_resource::<GridCellOrigin>()
            .insert_resource(GroundDetail {
                slope: Some(Handle::default()),
                centre: Vec2::new(100.0, -50.0),
                half_m: 48.0,
                ..default()
            })
            .add_systems(Update, refresh_ground_detail);
        app
    }

    fn modified(app: &mut App) -> Vec<AssetId<GroundMaterial>> {
        app.world_mut()
            .resource_mut::<Messages<AssetEvent<GroundMaterial>>>()
            .drain()
            .filter_map(|e| match e {
                AssetEvent::Modified { id } => Some(id),
                _ => None,
            })
            .collect()
    }

    /// A material landing must not re-prepare the ones already right. The trigger
    /// counts materials, so it fires once per landing — and on the web build an
    /// unconditional write there rebuilt every ground bind group each time, the
    /// largest single cost of a load.
    #[test]
    fn a_new_material_rewrites_only_itself() {
        let mut app = app();
        // Held, as a mesh's `MeshMaterial3d` holds them: a dropped handle is
        // removed before the system ever sees the material.
        let first = app
            .world_mut()
            .resource_mut::<Assets<GroundMaterial>>()
            .add(GroundMaterial::default());
        app.update();
        assert_eq!(
            modified(&mut app),
            vec![first.id()],
            "a new material takes the uniform"
        );

        app.update();
        assert!(
            modified(&mut app).is_empty(),
            "nothing moved, nothing written"
        );

        let second = app
            .world_mut()
            .resource_mut::<Assets<GroundMaterial>>()
            .add(GroundMaterial::default());
        app.update();
        assert_eq!(
            modified(&mut app),
            vec![second.id()],
            "only the newcomer is written"
        );
    }
}
