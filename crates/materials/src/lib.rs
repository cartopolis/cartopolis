//! The client's custom render materials, and [`MaterialsPlugin`], which registers
//! all of them.
//!
//! One module per client module the types came from; the client re-exports each
//! type from its old path (`cartopolis::systems::water::WaterMaterial`, …). Why
//! they live in a crate of their own is this crate's `Cargo.toml`.

use bevy::pbr::MaterialPlugin;
use bevy::prelude::*;

pub mod building_fade;
pub mod building_windows;
pub mod data_layers;
pub mod globe_ocean;
pub mod grass;
pub mod ground_detail;
pub mod particles;
pub mod road_depth;
pub mod sky;
pub mod sky_dome;
pub mod sun_rays;
pub mod water;

/// Every custom material's plugin, in the order `cartopolis::run` added them
/// before they moved here.
pub struct MaterialsPlugin;

impl Plugin for MaterialsPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<data_layers::PinHeadMaterial>::default())
            .add_plugins(MaterialPlugin::<sky::StarFieldMaterial>::default())
            .add_plugins(MaterialPlugin::<sky::MoonMaterial>::default())
            .add_plugins(MaterialPlugin::<sky::HorizonHazeMaterial>::default())
            // The sky for adapters that cannot run the atmosphere above (its module
            // header has the whole story). The plugin is added unconditionally — the
            // shell is only spawned on the runs that need it.
            .add_plugins(MaterialPlugin::<sky_dome::SkyDomeMaterial>::default())
            .add_plugins(MaterialPlugin::<sky::CloudMaterial>::default())
            // Sunbeams: a full-screen pass on the camera, switched by `render_features`.
            .add_plugins(
                bevy::core_pipeline::fullscreen_material::FullscreenMaterialPlugin::<
                    sun_rays::SunRays,
                >::default(),
            )
            .add_plugins(MaterialPlugin::<sky::GlobeCloudMaterial>::default())
            .add_plugins(MaterialPlugin::<sky::RainMaterial>::default())
            .add_plugins(MaterialPlugin::<particles::ParticleMaterial>::default())
            .add_plugins(MaterialPlugin::<water::WaterMaterial>::default())
            // The basemap and the ground surfaces, carrying the half-metre residual slope
            // the geometry is too coarse to hold (`systems::ground_detail`).
            .add_plugins(MaterialPlugin::<ground_detail::GroundMaterial>::default())
            // The LoD2.2 buildings' walls, with the window pattern on them
            // (`systems::building_windows`).
            .add_plugins(MaterialPlugin::<building_windows::BuildingMaterial>::default())
            // The far building tier's own material: the same `StandardMaterial`, plus a
            // vertex stage that takes the outermost ring down into the map rather than
            // ending the city at a cell edge (`systems::building_fade`).
            .add_plugins(MaterialPlugin::<building_fade::FarBuildingMaterial>::default())
            .add_plugins(MaterialPlugin::<grass::GrassMaterial>::default())
            // Roads and their casings. The ladder that decides which of two
            // overlapping ribbons is on top rides in a vertex attribute, so the road
            // groups need a pipeline that declares it (`systems::road_depth`).
            .add_plugins(MaterialPlugin::<road_depth::RoadMaterial>::default())
            .add_plugins(MaterialPlugin::<globe_ocean::GlobeSurfaceMaterial>::default());
    }
}
