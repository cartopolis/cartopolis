//! Real tree models up close, the merged crowns further out.
//!
//! A tree is drawn three ways, by distance:
//!
//! | distance | drawn as | triangles a tree |
//! |---|---|---|
//! | under [`LOD0_M`] | its form's full model (`assets/models/trees/<form>_lod0.glb`) | 4,000–20,000 |
//! | to [`NEAR_M`] | the same tree, light (`<form>_lod1.glb`) | 1,000–5,000 |
//! | beyond | the merged crown of [`scatter::build_vegetation_meshes`] | 18–54 |
//!
//! **The switch to the crowns is made per chunk, not per tree.** The crowns are
//! merged into one mesh per [`CHUNK_M`] square of a cell, which is what keeps
//! 6,000 trees a cell affordable; a mesh cannot hide one tree out of three
//! hundred. So the map worker builds the crowns chunk by chunk
//! ([`chunked_meshes`]), and [`update_tree_models`] decides per chunk from the
//! camera's distance to its centre: a near chunk hides its crowns and stands its
//! trees up as models, a far one shows its crowns and has no models. Within a
//! near chunk, the full and the light model crossfade per tree on Bevy's
//! [`VisibilityRange`], which is evaluated on the GPU's side of the frame and
//! costs nothing to keep current.
//!
//! **The cost is triangles, not draw calls.** Every model of one form shares one
//! mesh and one material, so Bevy batches them; what instancing cannot change is
//! that the GPU still shades every triangle of every model, once more for each
//! shadow cascade. A Groningen quay in this rig measured 21.7 M triangles against
//! 52 k for the crowns (`docs/notes/tree-silhouettes.md`) before the light model
//! and the chunk distance existed. The light model casts no shadow.
//!
//! Off where [`quality::Budget::tree_models`] says so — the phone and the browser —
//! and then the worker builds the crowns as it always did.
//!
//! [`scatter::build_vegetation_meshes`]: crate::systems::scatter::build_vegetation_meshes
//! [`quality::Budget::tree_models`]: crate::quality::Budget::tree_models

use std::collections::HashMap;

use bevy::camera::visibility::VisibilityRange;
use bevy::prelude::*;
use big_space::prelude::Grid;

use crate::grid::{AnchoredCamera, WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use crate::systems::scatter::{TreeForm, TreeInstance};
use crate::systems::vector_tiles::SurfaceMesh;
use crate::systems::vegetation::VegetationLod;
use cartopolis_geo::tree_form::TREE_FORMS;

/// The side of a crown chunk, metres.
pub const CHUNK_M: f32 = 100.0;

/// A chunk whose centre is nearer the camera than this is drawn as models.
pub const NEAR_M: f32 = 140.0;

/// How much further a near chunk may drift before it turns back into crowns, so
/// a camera standing on the line does not flip it every frame.
const HYSTERESIS_M: f32 = 20.0;

/// Where a tree's full model hands over to its light one, and the band they
/// crossfade across.
pub const LOD0_M: f32 = 50.0;
const LOD_FADE_M: f32 = 10.0;

/// How many trees may be stood up in one frame: four entities each, batched by
/// Bevy, so the cost is spawn work, not upload.
const MAX_TREES_PER_FRAME: usize = 160;

/// Whether this tree's form has a model. The weeping willow has none: EZ-Tree
/// cannot hang a curtain, and its own crown says "willow" better than any
/// tree the generator made.
#[must_use]
pub fn has_model(form: TreeForm) -> bool {
    form != TreeForm::Weeping
}

/// One tree a near chunk stands up, cell-local.
#[derive(Clone, Copy, Debug)]
pub struct ModelTree {
    pub x: f32,
    pub z: f32,
    /// The ground under it, anchored world metres; filled in by
    /// [`TreeModels::insert_cell`], which has the terrain.
    pub y: f32,
    pub height: f32,
    pub form: TreeForm,
    pub yaw: f32,
}

/// The worker's half: the crowns of the trees that have a model, built chunk by
/// chunk, and those trees for [`update_tree_models`]. Trees without a model —
/// the willows — are left in `trees` for the caller's ordinary build.
#[must_use]
pub fn chunked_meshes(
    trees: &mut Vec<TreeInstance>,
    max_bytes: Option<usize>,
) -> (Vec<SurfaceMesh>, Vec<ModelTree>) {
    let mut chunks: HashMap<(i32, i32), Vec<TreeInstance>> = HashMap::new();
    let mut models = Vec::new();
    trees.retain(|t| {
        if !has_model(t.form) {
            return true;
        }
        chunks.entry(chunk_of(t.x, t.z)).or_default().push(*t);
        models.push(ModelTree {
            x: t.x,
            z: t.z,
            y: 0.0,
            height: t.height,
            form: t.form,
            // Its own turn, from its position, so a row is not one tree repeated.
            yaw: (t.x * 12.9898 + t.z * 78.233).sin() * 43_758.547 % std::f32::consts::TAU,
        });
        false
    });
    let mut keys: Vec<_> = chunks.keys().copied().collect();
    keys.sort_unstable();
    let meshes = keys
        .into_iter()
        .flat_map(|k| crate::systems::scatter::build_vegetation_meshes(&chunks[&k], max_bytes))
        .collect();
    (meshes, models)
}

fn chunk_of(x: f32, z: f32) -> (i32, i32) {
    ((x / CHUNK_M).floor() as i32, (z / CHUNK_M).floor() as i32)
}

/// The shared meshes and materials of every model, loaded once.
struct ModelAssets {
    /// Per form (by `TreeForm as usize`), per level: (bark, leaves) meshes and
    /// materials. `None` for a form with no model.
    parts: Vec<Option<[[(Handle<Mesh>, Handle<StandardMaterial>); 2]; 2]>>,
}

#[derive(Default)]
struct Chunk {
    /// Anchored world XZ of the chunk's centre, and its ground height.
    centre: Vec3,
    trees: Vec<ModelTree>,
    /// The crown pieces this chunk was merged into.
    crowns: Vec<Entity>,
    /// The models standing, while near.
    models: Vec<Entity>,
    /// How many of `trees` have been stood up.
    spawned: usize,
    near: bool,
}

struct CellTrees {
    /// Anchored world XZ of the cell's origin.
    origin: Vec2,
    chunks: HashMap<(i32, i32), Chunk>,
}

/// Every live cell's chunks, and the models standing in the near ones.
#[derive(Resource, Default)]
pub struct TreeModels {
    assets: Option<ModelAssets>,
    cells: HashMap<(i64, i64), CellTrees>,
}

impl TreeModels {
    /// Record a cell's model trees and the crown pieces they were merged into
    /// (`(piece centre, entity)`, cell-local). A piece belongs to the chunk its
    /// centre is in: the worker built each piece from one chunk's trees, and a
    /// crown is centred on its trunk.
    pub fn insert_cell(
        &mut self,
        cell: (i64, i64),
        origin: Vec2,
        trees: Vec<ModelTree>,
        crowns: Vec<(Vec2, Entity)>,
        ground: impl Fn(f32, f32) -> f32,
    ) {
        if trees.is_empty() {
            self.cells.remove(&cell);
            return;
        }
        let mut chunks: HashMap<(i32, i32), Chunk> = HashMap::new();
        for mut t in trees {
            t.y = ground(origin.x + t.x, origin.y + t.z);
            chunks.entry(chunk_of(t.x, t.z)).or_default().trees.push(t);
        }
        for (key, chunk) in &mut chunks {
            let local = Vec2::new(
                (key.0 as f32 + 0.5) * CHUNK_M,
                (key.1 as f32 + 0.5) * CHUNK_M,
            );
            let world = origin + local;
            chunk.centre = Vec3::new(world.x, ground(world.x, world.y), world.y);
        }
        for (centre, entity) in crowns {
            if let Some(chunk) = chunks.get_mut(&chunk_of(centre.x, centre.y)) {
                chunk.crowns.push(entity);
            }
        }
        self.cells.insert(cell, CellTrees { origin, chunks });
    }
}

/// Load the models, once, where the rung draws them.
pub fn load_tree_models(mut models: ResMut<TreeModels>, asset_server: Res<AssetServer>) {
    if models.assets.is_some() || !crate::quality::budget().tree_models {
        return;
    }
    let parts = TREE_FORMS
        .iter()
        .map(|&form| {
            has_model(form).then(|| {
                [0, 1].map(|lod| {
                    let path = format!("models/trees/{}_lod{lod}.glb", form.slug());
                    [0, 1].map(|primitive| {
                        (
                            asset_server.load(
                                bevy::gltf::GltfAssetLabel::Primitive { mesh: 0, primitive }
                                    .from_asset(path.clone()),
                            ),
                            // `MaterialN` is the glTF's own description
                            // (`GltfMaterial`); bevy_pbr files the
                            // `StandardMaterial` it makes of it under `/std`.
                            asset_server.load(format!("{path}#Material{primitive}/std")),
                        )
                    })
                })
            })
        })
        .collect();
    models.assets = Some(ModelAssets { parts });
}

/// Decide per chunk between models and crowns, and stand the models up.
#[allow(clippy::too_many_arguments)]
pub fn update_tree_models(
    mut commands: Commands,
    mut models: ResMut<TreeModels>,
    mut surfaces: ResMut<crate::systems::map_geometry::MapSurfaces>,
    veg: Res<VegetationLod>,
    camera: AnchoredCamera,
    bigspace: Query<Entity, With<big_space::prelude::BigSpace>>,
    mut visibility: Query<&mut Visibility>,
) {
    if models.assets.is_none() {
        return;
    }
    let (Ok(root), Some(eye)) = (bigspace.single(), camera.world_position_f64()) else {
        return;
    };
    let eye = eye.as_vec3();
    // Above the vegetation fade, or with the layer off, every chunk is far: the
    // crowns are what fades into the flat fill.
    let allowed = veg.enabled && veg.factor <= 0.0;
    let TreeModels { assets, cells } = &mut *models;
    let assets = assets.as_ref().expect("checked above");
    cells.retain(|cell, _| surfaces.holds(*cell));
    let mut budget = MAX_TREES_PER_FRAME;
    let mut to_attach = Vec::new();
    for (cell, trees) in cells.iter_mut() {
        for chunk in trees.chunks.values_mut() {
            let d = chunk.centre.distance(eye);
            let near = allowed && d < NEAR_M + if chunk.near { HYSTERESIS_M } else { 0.0 };
            if near != chunk.near {
                chunk.near = near;
                if !near {
                    for e in chunk.models.drain(..) {
                        commands.entity(e).try_despawn();
                    }
                    chunk.spawned = 0;
                    for &e in &chunk.crowns {
                        if let Ok(mut v) = visibility.get_mut(e) {
                            v.set_if_neq(veg.visibility());
                        }
                    }
                }
            }
            if !chunk.near {
                continue;
            }
            // Stand up what the frame allows; the crowns stay until all of the
            // chunk stands, so a tree is never missing on the way.
            while chunk.spawned < chunk.trees.len() && budget > 0 {
                let t = chunk.trees[chunk.spawned];
                chunk.spawned += 1;
                budget -= 1;
                let Some(Some(parts)) = assets.parts.get(t.form as usize) else {
                    continue;
                };
                let world = Vec3::new(trees.origin.x + t.x, t.y, trees.origin.y + t.z);
                let (grid_cell, local) = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD)
                    .translation_to_grid(world.as_dvec3());
                let transform = Transform {
                    translation: local,
                    rotation: Quat::from_rotation_y(t.yaw),
                    scale: Vec3::splat(t.height),
                };
                for (lod, level) in parts.iter().enumerate() {
                    let range = if lod == 0 {
                        VisibilityRange {
                            start_margin: 0.0..0.0,
                            end_margin: LOD0_M - LOD_FADE_M * 0.5..LOD0_M + LOD_FADE_M * 0.5,
                            use_aabb: false,
                        }
                    } else {
                        VisibilityRange {
                            start_margin: LOD0_M - LOD_FADE_M * 0.5..LOD0_M + LOD_FADE_M * 0.5,
                            end_margin: f32::MAX..f32::MAX,
                            use_aabb: false,
                        }
                    };
                    for (mesh, material) in level {
                        let mut e = commands.spawn((
                            Mesh3d(mesh.clone()),
                            MeshMaterial3d(material.clone()),
                            transform,
                            grid_cell,
                            ChildOf(root),
                            range.clone(),
                            crate::components::VegetationSurface,
                            veg.visibility(),
                            crate::components::HideInSpace,
                        ));
                        if lod == 1 {
                            e.insert(bevy::light::NotShadowCaster);
                        }
                        let id = e.id();
                        chunk.models.push(id);
                        to_attach.push((*cell, id));
                    }
                }
            }
            if chunk.spawned == chunk.trees.len() {
                for &e in &chunk.crowns {
                    if let Ok(mut v) = visibility.get_mut(e) {
                        v.set_if_neq(Visibility::Hidden);
                    }
                }
            }
        }
    }
    // Hung off the cell like everything else it holds, so retiring the cell
    // retires them.
    for (cell, id) in to_attach {
        surfaces.attach(cell, id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(x: f32, z: f32, form: TreeForm) -> TreeInstance {
        TreeInstance {
            x,
            z,
            radius: 3.0,
            height: 12.0,
            tone: 0.5,
            luma: 1.0,
            form,
        }
    }

    /// Every modelled tree is in exactly one chunk's crowns and in the model
    /// list; a willow is in neither and stays for the ordinary build.
    #[test]
    fn chunks_take_the_modelled_trees_and_leave_the_willows() {
        let mut trees = vec![
            tree(10.0, 10.0, TreeForm::Broad),
            tree(150.0, 20.0, TreeForm::Round),
            tree(160.0, 30.0, TreeForm::Weeping),
            tree(410.0, 390.0, TreeForm::Pine),
        ];
        let (meshes, models) = chunked_meshes(&mut trees, None);
        assert_eq!(models.len(), 3);
        assert_eq!(trees.len(), 1);
        assert_eq!(trees[0].form, TreeForm::Weeping);
        // Three chunks, a crown and a trunk piece each.
        assert_eq!(meshes.len(), 6);
        for m in &meshes {
            let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
            for p in &m.positions {
                lo = [lo[0].min(p[0]), lo[1].min(p[2])];
                hi = [hi[0].max(p[0]), hi[1].max(p[2])];
            }
            let centre = ((lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5);
            assert!(
                models
                    .iter()
                    .any(|t| chunk_of(t.x, t.z) == chunk_of(centre.0, centre.1)),
                "a piece centred outside every chunk of its trees: {centre:?}"
            );
        }
    }

    #[test]
    fn every_form_but_the_willow_has_a_model() {
        let modelled: Vec<_> = TREE_FORMS.into_iter().filter(|f| has_model(*f)).collect();
        assert_eq!(modelled.len(), TREE_FORMS.len() - 1);
        assert!(!has_model(TreeForm::Weeping));
    }
}
