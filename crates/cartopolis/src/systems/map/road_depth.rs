//! The road stack's depth ladder: the per-vertex rung, and the material that
//! applies it.
//!
//! Road ribbons overlap by construction — a junction stub crosses whatever it
//! tees into, Shortbread's merged ways cross each other freely, a flyover shares
//! the ground plane with the road under it — and every overlap is two opaque
//! *coplanar* triangles. Which one wins is then decided by rasteriser rounding,
//! and it re-rolls every frame the view matrix moves: the shimmer down a street
//! and the bleeding at its junctions.
//!
//! [`vector_tiles::road_rung`](cartopolis_geo::vector_tiles) decides the order —
//! class, then casing, then width band, then a tie colouring. This module is what
//! makes the decision stick.
//!
//! ## Why this is a material and not a `depth_bias`
//!
//! `StandardMaterial::depth_bias` reaches the hardware depth bias and is exactly
//! the right *denomination* — depth-buffer ULPs, whose apparent world shift grows
//! with range in step with the buffer's own quantum. It is the wrong *granularity*:
//! it is one constant for every ribbon sharing the material, and all roads of a
//! class share one. So it can rank `Minor` against `Major` and can say nothing at
//! all about two residential streets crossing.
//!
//! The rung used to make up the difference geometrically, lifting each ribbon
//! 24 µm. That is the half that failed. A geometric gap's significance decays as
//! 1/z — 24 µm is 2.0 ULP at 100 m, 1.0 at 200 m, nothing beyond — so past a
//! couple of hundred metres the within-class ladder was below the depth buffer's
//! resolution and every same-class overlap went back to a coin flip. Worse, the
//! two ladders were denominated differently and had to be kept agreeing by hand;
//! where they disagreed the winner *flipped* with camera distance, which reads as
//! a street's tarmac vanishing rather than as flicker.
//!
//! Carrying the rung per vertex and applying it in the vertex stage
//! (`assets/shaders/road_depth.wgsl`) collapses both ladders into one, in the
//! units the depth buffer actually arbitrates in. Nothing decays, nothing can
//! contradict, and — since the rung costs no height — the ceiling that forced the
//! width bands down to two disappears with it.
//!
//! ## What this costs
//!
//! Four bytes per road vertex, and one extra pipeline. It costs no draw calls:
//! there is still exactly one material per [`SurfaceGroup`], which is the whole
//! reason the ordering had to be per *vertex* rather than per material in the
//! first place.

use bevy::mesh::{MeshVertexAttribute, MeshVertexBufferLayoutRef};
use bevy::pbr::{
    ExtendedMaterial, MaterialExtension, MaterialExtensionKey, MaterialExtensionPipeline,
};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, RenderPipelineDescriptor, SpecializedMeshPipelineError, VertexFormat,
};
use bevy::shader::ShaderRef;

/// The road ladder rung this vertex draws on — `SurfaceMesh::rungs`.
///
/// Constant across a ribbon, so it interpolates to itself and every fragment of
/// one road gets the same offset. A `f32` rather than a `u32` because it is
/// multiplied straight into clip-space `z`, and the attribute id is arbitrary but
/// must not collide with bevy's own (which live below 10).
pub const ATTRIBUTE_ROAD_RUNG: MeshVertexAttribute =
    MeshVertexAttribute::new("RoadRung", 970_412_233, VertexFormat::Float32);

/// The shader location `road_depth.wgsl` reads the rung from. Bevy's own mesh
/// attributes occupy 0–7, so this is the first free one.
const RUNG_SHADER_LOCATION: u32 = 8;

/// Roads and their casings, ordered by [`ATTRIBUTE_ROAD_RUNG`].
pub type RoadMaterial = ExtendedMaterial<StandardMaterial, RoadDepthExtension>;

/// Adds nothing but the vertex stage — the fragment is `StandardMaterial`'s,
/// unchanged, which is what keeps a road lit by the same sun, shadow cascades,
/// fog and environment map as the ground it lies on.
#[derive(Asset, AsBindGroup, Reflect, Debug, Clone, Default)]
pub struct RoadDepthExtension {}

impl MaterialExtension for RoadDepthExtension {
    fn vertex_shader() -> ShaderRef {
        "shaders/road_depth.wgsl".into()
    }

    // `prepass_vertex_shader` is deliberately left at bevy's default. The rung is
    // a depth-buffer trick for the *camera's* view of a coplanar sheet; applying
    // it to the shadow map would bias a road against the sun instead, for no
    // reason. The geometry is flat and honest, so the default is correct there.

    fn specialize(
        _pipeline: &MaterialExtensionPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        layout: &MeshVertexBufferLayoutRef,
        _key: MaterialExtensionKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // The vertex shader declares the rung at `RUNG_SHADER_LOCATION`, so the
        // buffer layout has to supply it or the pipeline fails validation.
        let vertex_layout = layout.0.get_layout(&[
            Mesh::ATTRIBUTE_POSITION.at_shader_location(0),
            Mesh::ATTRIBUTE_NORMAL.at_shader_location(1),
            Mesh::ATTRIBUTE_UV_0.at_shader_location(2),
            ATTRIBUTE_ROAD_RUNG.at_shader_location(RUNG_SHADER_LOCATION),
        ])?;
        descriptor.vertex.buffers = vec![vertex_layout];
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::systems::road_texture::RoadClass;
    use crate::systems::vector_tiles::{road_rung_ceiling, SurfaceGroup, ROAD_RUNG_ULP};

    /// The shader hard-codes the ULPs per rung, because a uniform for one
    /// compile-time constant is a bind group and a plumbing seam for nothing. The
    /// cost of hard-coding it is that two files can drift, so this is the thing
    /// that notices.
    #[test]
    fn the_shader_and_the_ladder_agree_on_the_step() {
        let src = include_str!("../../../../../assets/shaders/road_depth.wgsl");
        let line = src
            .lines()
            .find(|l| l.trim_start().starts_with("const RUNG_ULP"))
            .expect("road_depth.wgsl declares RUNG_ULP");
        let value: f32 = line
            .split('=')
            .nth(1)
            .and_then(|v| v.trim().trim_end_matches(';').parse().ok())
            .expect("RUNG_ULP parses as a float");
        assert!(
            (value - ROAD_RUNG_ULP).abs() < f32::EPSILON,
            "road_depth.wgsl says {value} ULPs per rung, the ladder says {ROAD_RUNG_ULP}",
        );
    }

    /// The road ladder lives *inside* the band `SurfaceGroup::depth_rung` leaves
    /// for it. If the ladder outgrew that band, the tallest road rungs would climb
    /// past the bridge decks and the street furniture and start drawing over them
    /// — which is the same class of bug (two ladders disagreeing) that the
    /// per-vertex rung exists to end, so it is worth an assertion rather than a
    /// comment.
    #[test]
    fn the_road_ladder_stays_under_everything_drawn_over_a_road() {
        let ceiling = road_rung_ceiling();
        for above in [
            SurfaceGroup::BridgeDeck,
            SurfaceGroup::Canopy,
            SurfaceGroup::Trunk,
            SurfaceGroup::Furniture,
        ] {
            assert!(
                above.depth_bias() > ceiling,
                "{above:?} biases {} but the road ladder reaches {ceiling}",
                above.depth_bias(),
            );
        }
        // …and above the ground sheets a road is drawn over.
        for below in [
            SurfaceGroup::Water,
            SurfaceGroup::Grass,
            SurfaceGroup::Forest,
            SurfaceGroup::Built,
        ] {
            assert!(
                below.depth_bias() < SurfaceGroup::Road(RoadClass::Path).depth_bias(),
                "{below:?} is not under the road band",
            );
        }
    }
}
