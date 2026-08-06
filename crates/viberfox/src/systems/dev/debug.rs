use bevy::prelude::*;
use crate::components::Region;
use crate::systems::rendering::RegionMesh;

pub fn debug_region_entities(
    all_regions: Query<(Entity, &Region)>,
    region_with_mesh: Query<Entity, (With<Region>, With<RegionMesh>)>,
    region_without_mesh: Query<Entity, (With<Region>, Without<RegionMesh>)>,
) {
    tracing::debug!(
        total = all_regions.iter().count(),
        with_mesh = region_with_mesh.iter().count(),
        without_mesh = region_without_mesh.iter().count(),
        "region mesh status"
    );
    for (entity, region) in all_regions.iter() {
        tracing::trace!("region entity {:?} id={} name={}", entity, region.id, region.name);
    }
}
