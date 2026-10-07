//! The local water surface's material. The sea state it is built from, and the
//! systems that drive it, are `cartopolis::systems::water`.

use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::AsBindGroup;
use bevy::render::storage::ShaderBuffer;
use bevy::shader::ShaderRef;

/// The material `map_geometry` uses for `SurfaceGroup::Water`
/// (`cartopolis::systems::vector_tiles::SurfaceGroup::Water`).
pub type WaterMaterial = ExtendedMaterial<StandardMaterial, WaterExtension>;

/// Wave/surface uniforms for `shaders/water.wgsl`.
#[derive(Asset, AsBindGroup, Reflect, Clone, Debug)]
pub struct WaterExtension {
    /// x: RMS slope of the wave field (see `cartopolis::systems::water::rms_slope`).
    /// y: base wavelength (m).
    /// z: wave speed scalar (m/s). w: detail cut-off (per metre of view range).
    #[uniform(100)]
    pub wave: Vec4,
    /// x: base (micro) roughness, perceptual. y: crest sub-surface scattering.
    /// z: top-down depth darkening. w: crest foam amount.
    #[uniform(101)]
    pub surface: Vec4,
    /// xy: unit wind direction. z: chop 0–1 (swell → wind chop). w: how clear the
    /// sky is, 1 − `overcast` (`cartopolis::systems::sky::overcast`) — read only by the
    /// shader's stand-in sky reflection on a view with no environment map.
    #[uniform(102)]
    pub wind: Vec4,
    /// Where this cell's shore field
    /// (`cartopolis::systems::vector_tiles::water_shore_field`) sits: xy = the
    /// cell's north-west corner in the same anchor-metre frame the mesh's UV0 uses,
    /// z = the cell's ground size in metres, w = the metre value a fully-saturated
    /// texel stands for.
    #[uniform(103)]
    pub field: Vec4,
    /// The shape of the octave sum, all of it from `octave_norms`
    /// (`cartopolis::systems::water::octave_norms`): x = how many
    /// octaves to run, y = the gradient normaliser that makes `wave.x` an RMS
    /// slope, z = 1/Σ amplitude, w = 1/Σ amplitude over the short octaves.
    ///
    /// It is a uniform rather than four shader constants because the octave count
    /// is a quality rung (`cartopolis::quality::Budget::water_octaves`) and because
    /// the series arithmetic behind y and z is worth having on the CPU, where a
    /// test can read it.
    #[uniform(106)]
    pub octaves: Vec4,
    /// Distance from the nearest bank, in this cell's tile-local frame.
    #[texture(104)]
    #[sampler(105)]
    pub shore: Handle<Image>,
    /// Distance from the nearest **building**, in the same frame and at the same
    /// metre scale (`field.w`), a quarter of the resolution.
    ///
    /// Only the bank-in-the-mirror term reads it, and only to answer the one
    /// question the shore field cannot: not "where does the water end" but "is
    /// there anything standing there". See
    /// `ShoreField::bank` (`cartopolis::systems::vector_tiles::ShoreField::bank`).
    #[texture(107)]
    #[sampler(108)]
    pub bank: Handle<Image>,
    /// The ground cloud deck this frame
    /// ([`CLOUD_FIELD_BUFFER`](crate::sky::CLOUD_FIELD_BUFFER)), so the
    /// surface mirrors the clouds that are overhead. One buffer shared by every
    /// cell: the deck moves every frame, and rewriting each per-cell material
    /// would re-upload all of them every frame.
    #[storage(109, read_only, visibility(fragment))]
    pub clouds: Handle<ShaderBuffer>,
}

impl MaterialExtension for WaterExtension {
    fn fragment_shader() -> ShaderRef {
        "shaders/water.wgsl".into()
    }
}
