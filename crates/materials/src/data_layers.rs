//! The data layers' pin-head material. The layers themselves are
//! `cartopolis::systems::data_layers`.

use bevy::pbr::{
    ExtendedMaterial, MaterialExtension, MaterialExtensionKey, MaterialExtensionPipeline,
};
use bevy::prelude::*;
use bevy::render::mesh::MeshVertexBufferLayoutRef;
use bevy::render::render_resource::{
    AsBindGroup, CompareFunction, RenderPipelineDescriptor, SpecializedMeshPipelineError,
};

/// Material for the floating shop-pin heads: a plain `StandardMaterial` (unlit,
/// per-vertex hue) whose depth test is forced to always pass, so the gem heads
/// draw over the buildings like real map POI markers. The stems and ground
/// discs stay on the ordinary depth-tested marker material and still occlude
/// behind walls, so an occluded pin reads as a gem hovering above its roof
/// rather than a needle poking through it. The base uses `AlphaMode::Blend` so
/// heads render in the transparent phase, after all opaque geometry — nothing
/// painted earlier can then hide them.
pub type PinHeadMaterial = ExtendedMaterial<StandardMaterial, DepthAlways>;

/// Pipeline tweak for [`PinHeadMaterial`]: pass the depth test unconditionally.
#[derive(Asset, AsBindGroup, Reflect, Clone, Default)]
pub struct DepthAlways {}

impl MaterialExtension for DepthAlways {
    fn specialize(
        _pipeline: &MaterialExtensionPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialExtensionKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        if let Some(depth) = descriptor.depth_stencil.as_mut() {
            depth.depth_compare = Some(CompareFunction::Always);
        }
        Ok(())
    }
}
