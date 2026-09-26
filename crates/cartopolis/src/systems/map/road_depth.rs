//! The road stack's depth ladder: the per-vertex rung, and the material that
//! applies it.
//!
//! **The ladder has two rungs — ground, and bridge — and that is the whole of it.**
//! It used to have 399, ordering every road ribbon against every other by class,
//! then casing, then width band, then a tie colouring, because road ribbons
//! overlapped by construction and something had to decide which one the
//! rasteriser drew. `cartopolis_geo::road_stitch` cuts the overlap away now: the
//! ground roads are a partition, no two of them cover the same patch, and there is
//! nothing left for a tie-break to break.
//!
//! What remains is the one overlap that is real. A flyover and the road beneath it
//! are two surfaces in one place — both drawn on the ground plane, since the deck
//! carries its own height only where `tessellate_bridges` gives it one — and the
//! rung is what holds them apart.
//!
//! ## Why this is a material and not a `depth_bias`
//!
//! `StandardMaterial::depth_bias` reaches the hardware depth bias and is the right
//! *denomination* — depth-buffer units, whose apparent world shift grows with
//! range in step with the buffer's own quantum. It is the wrong *granularity*: it
//! is one constant for every ribbon sharing the material, and a bridge's tarmac
//! shares a material with the ordinary road it flies over. Hence a vertex
//! attribute.
//!
//! ## Why the rung is a depth offset and not a height
//!
//! It was a height once, lifting each ribbon 24 µm, and that decays: a geometric
//! gap is worth 2.0 depth units at 100 m, 1.0 at 200 m and nothing beyond, so past
//! a couple of hundred metres the ordering simply dissolved. Scaling clip-space
//! `z` instead offsets a surface by a fixed number of units of its *own* depth,
//! which is exactly what a hardware depth-bias unit is, so the separation neither
//! decays nor runs away with range. `assets/shaders/road_depth.wgsl` does it in
//! the vertex stage.
//!
//! One rung is [`ROAD_RUNG_ULP`] = 64 depth units, matching
//! `vector_tiles::DEPTH_BIAS_STEP`. It was 4, on the assertion that four "clears
//! the rasteriser's interpolation noise" — which was never measured, and was
//! wrong: the sibling ladder needs 64, having found 16 too little at the shallow
//! angles a low camera looks at the middle distance with. That is the whole
//! failure this module used to have, and the reason the fix moved into geometry.
//!
//! ## What this costs
//!
//! Four bytes per road vertex, and one extra pipeline. It costs no draw calls:
//! there is still exactly one material per [`SurfaceGroup`].

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

/// The vertex stage that applies the depth rung, plus the sub-metre ground slope the
/// flat surfaces carry (`systems::ground_detail`) — a carriageway's camber is exactly
/// the kind of half-metre relief no mesh here can hold.
///
/// The bindings and their meaning are `GroundDetailExtension`'s, deliberately at the
/// same numbers, so the *fragment* shader is literally the same file. Two material
/// types, two vertex stages, one description of what the ground looks like up close.
///
/// That sameness is a standing obligation, not a coincidence: a binding added to one
/// must be added to the other, or the shared shader declares a binding this material's
/// layout does not supply and the pipeline fails validation — which
/// `lib.rs::handle_render_error` answers by halting the renderer for the rest of the
/// process. The clipmap cross-fade's three (103–105) are here for that reason alone; a
/// road is never a clipmap tile, so they are never written and `lod.w` stays 0.
#[derive(Asset, AsBindGroup, Reflect, Debug, Clone, Default)]
pub struct RoadDepthExtension {
    /// xy: the detail box's centre in render-space XZ. z: 1 / its half-extent.
    /// w: strength, `0.0` when there is no tier to read.
    #[uniform(100)]
    pub detail: Vec4,
    #[texture(101)]
    #[sampler(102)]
    pub slope: Option<Handle<Image>>,
    /// Always zero — see the type's own docs.
    #[uniform(103)]
    pub lod: Vec4,
    /// Always `None` — see the type's own docs.
    #[texture(104)]
    #[sampler(105)]
    pub basemap: Option<Handle<Image>>,
    /// Always zero — see the type's own docs. The clipmap's water (106–107) is
    /// here for the same reason the cross-fade is: a road carries no water mask,
    /// and `water_sea.z == 0` keeps the shader off it.
    #[uniform(106)]
    pub water_body: Vec4,
    #[uniform(107)]
    pub water_sea: Vec4,
}

impl MaterialExtension for RoadDepthExtension {
    fn vertex_shader() -> ShaderRef {
        "shaders/road_depth.wgsl".into()
    }

    /// `ground_detail`'s, not one of our own: the ladder is a *vertex* trick and the
    /// fragment has nothing to add to it beyond the slope every other ground surface
    /// already gets.
    fn fragment_shader() -> ShaderRef {
        "shaders/ground_detail.wgsl".into()
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
        // **This hook runs for the prepass too, and must do nothing there.**
        //
        // Bevy calls the material's `specialize` from `PrepassPipelineSpecializer`
        // as well as from the main material pipeline — its own source carries the
        // comment "it's possible to change something that would break the prepass
        // but be fine in the main pass", and this is that case. The prepass
        // compiles **bevy's** vertex shader (`prepass_vertex_shader` is left at
        // the default, for the reason above), which declares a different set of
        // vertex inputs; replacing the buffer layout with the one our shader wants
        // leaves that shader with a location nothing supplies.
        //
        // The failure is not subtle and is not survivable: `RenderPipeline with
        // 'pbr_prepass_pipeline' label is invalid` at `Queue::submit`, which
        // `lib.rs::handle_render_error` halts the renderer on — i.e. the app draws
        // nothing for the rest of the process. It stayed latent for as long as
        // this client had no prepass at all, and appeared the moment ambient
        // occlusion required one.
        //
        // **There is no prepass again**: ambient occlusion was removed on
        // 2026-09-21 (`systems::render_features`) and took `DepthPrepass` and
        // `NormalPrepass` with it, so this branch is currently unreachable. It
        // stays because the last time it was unreachable somebody added a feature
        // that needed a prepass, and the cost of keeping it is one string compare
        // per pipeline specialisation.
        //
        // Matched on the label because that is the only thing here that
        // distinguishes the two calls: the mesh key carries the prepass *bits* in
        // both, since the main pass is told which prepasses exist.
        if descriptor
            .label
            .as_ref()
            .is_some_and(|label| label.contains("prepass"))
        {
            return Ok(());
        }
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
    use crate::systems::ground_stack::GroundLayer;
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

    /// **Everything drawn over a road has to clear the road ladder**, and until
    /// 2026-08-26 five things did not.
    ///
    /// The ladder grew from a handful of rungs to 399 when the road ordering moved
    /// into the depth buffer, and its ceiling went with it — from a couple of
    /// hundred depth units to 1,916. Four hard-coded rungs on the same ladder were
    /// left where they were and were swallowed: the zebra paint, the route line's
    /// casing and fill, the street labels and the driver arrow. A zebra crossing
    /// on anything wider than a residential street was being drawn underneath the
    /// tarmac.
    ///
    /// The test that existed then checked four *other* things — the bridge decks,
    /// the tree canopy, the trunks and the street furniture — and stayed green
    /// throughout. **That is the failure mode this version does not have**: it
    /// walks [`GroundLayer::ALL`], so a flat thing added tomorrow is checked
    /// tomorrow without anybody remembering to list it here.
    ///
    /// `ground_stack::the_road_band_holds_every_road_vertex` makes the same
    /// assertion from the other side, over the derivation rather than over the
    /// road ladder's arithmetic. Both are cheap and they fail for different
    /// reasons: this one if `road_rung` outgrows its band, that one if the band is
    /// mis-derived.
    #[test]
    fn everything_drawn_over_a_road_clears_the_road_ladder() {
        let ceiling = road_rung_ceiling();
        for layer in GroundLayer::ALL {
            if layer.rung() <= GroundLayer::Road.rung() {
                continue;
            }
            assert!(
                layer.depth_bias() > ceiling,
                "{layer:?} biases {} but a road vertex reaches {ceiling}",
                layer.depth_bias(),
            );
        }
    }

    /// …and the ground sheets a road is drawn *over* stay under it, likewise over
    /// the whole list rather than over a remembered four.
    #[test]
    fn the_road_ladder_stays_over_everything_a_road_is_drawn_on() {
        let foot = SurfaceGroup::Road(RoadClass::Path).depth_bias();
        for layer in GroundLayer::ALL {
            if layer.rung() >= GroundLayer::Road.rung() {
                continue;
            }
            assert!(
                layer.depth_bias() < foot,
                "{layer:?} is not under the road band"
            );
        }
    }
}
