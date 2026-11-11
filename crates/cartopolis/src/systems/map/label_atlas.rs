//! The street names' atlas: one two-channel texture with a slot per sign, written
//! slot by slot from the render world as signs take new names.
//!
//! Why an atlas, and why written this way: each of the pool's 160 signs used to
//! carry its own material and texture, and a sign taking a new name was a
//! material written through `get_mut` — re-prepared on the render thread, a
//! uniform buffer and a bind group each, ~65 of them every time a street-level
//! turn re-placed the names (`docs/notes/render-performance-survey.md`,
//! 2026-11-10). One material cannot be modified per sign, and a bevy `Image`
//! modified in the main world is re-uploaded whole, so a new name is written
//! into its slot with `Queue::write_texture` from the render world instead: the
//! sign's `MeshTag` says which slot and how much of it the name fills
//! (`cartopolis_materials::label_sign`), and nothing about a material or a mesh
//! changes.
//!
//! The slots are a fixed grid. A cell is [`CELL_W`] × [`CELL_H`] texels and a
//! name is drawn in its top-left [`NAME_W`] × [`NAME_H`]; the rest is a gutter
//! that keeps neighbouring names out of each other's filtering down to the
//! coarsest level written, [`MIPS`] − 1, where it is one texel wide. The cell
//! sides are multiples of 2^([`MIPS`] − 1) so every level's origin is whole.

use std::sync::{Arc, Mutex};

use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::{
    Extent3d, Origin3d, TexelCopyBufferLayout, TexelCopyTextureInfo, TextureAspect,
    TextureDimension, TextureFormat,
};
use bevy::render::renderer::RenderQueue;
use bevy::render::texture::GpuImage;
use bevy::render::{Render, RenderApp, RenderSystems};

/// Slots across and down. Their product is the sign pool's size.
pub const SLOT_COLS: u32 = 5;
pub const SLOT_ROWS: u32 = 32;
/// One slot's cell, texels. Multiples of 32 (`2^(MIPS - 1)`).
pub const CELL_W: u32 = 768;
pub const CELL_H: u32 = 96;
/// The gutter between a name and the next cell, texels, at level 0.
pub const GUTTER: u32 = 32;
/// The name's canvas within a cell.
pub const NAME_W: u32 = CELL_W - GUTTER;
pub const NAME_H: u32 = CELL_H - GUTTER;
/// Mip levels written, level 0 included. Six take a 32-texel gutter to one.
pub const MIPS: u32 = 6;
/// The whole atlas.
pub const ATLAS_W: u32 = SLOT_COLS * CELL_W;
pub const ATLAS_H: u32 = SLOT_ROWS * CELL_H;

/// A name rasterised for a slot: the full [`NAME_W`] × [`NAME_H`] canvas and
/// its mip chain, two bytes a texel (ink coverage, halo coverage). The whole
/// canvas, so a short name overwrites the tail of the long one before it.
pub struct SlotRaster {
    /// `(width, height, texels)` per level, level 0 first.
    pub levels: Vec<(u32, u32, Vec<u8>)>,
    /// How much of the **cell** the name fills, for the sign's tag: the shader
    /// scales the quad's 0..1 by these and then by one cell's share of the atlas.
    pub u_frac: f32,
    pub v_frac: f32,
}

impl SlotRaster {
    /// From the name's two coverage planes, `w × h` each, clipped to the canvas.
    #[must_use]
    pub fn new(ink: &[u8], halo: &[u8], w: u32, h: u32) -> Self {
        let (cw, ch) = (w.min(NAME_W), h.min(NAME_H));
        let mut level0 = vec![0u8; (NAME_W * NAME_H * 2) as usize];
        for y in 0..ch {
            for x in 0..cw {
                let src = (y * w + x) as usize;
                let dst = ((y * NAME_W + x) * 2) as usize;
                level0[dst] = ink[src];
                level0[dst + 1] = halo[src];
            }
        }
        let mut levels = vec![(NAME_W, NAME_H, level0)];
        for _ in 1..MIPS {
            let (pw, ph, prev) = levels.last().expect("level 0 exists");
            let (nw, nh) = ((pw / 2).max(1), (ph / 2).max(1));
            let mut next = vec![0u8; (nw * nh * 2) as usize];
            for y in 0..nh {
                for x in 0..nw {
                    for c in 0..2 {
                        let mut acc = 0u32;
                        for dy in 0..2 {
                            for dx in 0..2 {
                                let sx = (x * 2 + dx).min(pw - 1);
                                let sy = (y * 2 + dy).min(ph - 1);
                                acc += u32::from(prev[((sy * pw + sx) * 2 + c) as usize]);
                            }
                        }
                        next[((y * nw + x) * 2 + c) as usize] = (acc / 4) as u8;
                    }
                }
            }
            levels.push((nw, nh, next));
        }
        Self {
            levels,
            u_frac: cw as f32 / CELL_W as f32,
            v_frac: ch as f32 / CELL_H as f32,
        }
    }
}

/// The empty atlas image: every level transparent, uploaded once and never
/// modified from the main world again.
#[must_use]
pub fn blank_atlas() -> Image {
    let mut bytes = 0usize;
    let (mut w, mut h) = (ATLAS_W, ATLAS_H);
    for _ in 0..MIPS {
        bytes += (w * h * 2) as usize;
        w = (w / 2).max(1);
        h = (h / 2).max(1);
    }
    // `new_uninit` and not `new`: the latter asserts the data against level 0
    // alone, and this carries every level.
    let mut image = Image::new_uninit(
        Extent3d {
            width: ATLAS_W,
            height: ATLAS_H,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        TextureFormat::Rg8Unorm,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.texture_descriptor.mip_level_count = MIPS;
    image.data = Some(vec![0u8; bytes]);
    image.sampler = crate::systems::tile_loader::ground_sampler();
    image
}

/// Slot writes waiting for the render world, and the atlas they go into.
/// Shared with the render world's copy through the `Arc`, so the main world
/// pushes and the render world drains the same queue.
#[derive(Resource, Clone, Default)]
pub struct LabelAtlasWrites {
    pub atlas: Option<Handle<Image>>,
    pub pending: Arc<Mutex<Vec<(u32, Arc<SlotRaster>)>>>,
}

impl LabelAtlasWrites {
    /// Queue `raster` for `slot`.
    pub fn push(&self, slot: u32, raster: Arc<SlotRaster>) {
        self.pending
            .lock()
            .expect("label atlas queue")
            .push((slot, raster));
    }
}

impl ExtractResource<RenderApp> for LabelAtlasWrites {
    type Source = Self;
    fn extract_resource(source: &Self) -> Self {
        source.clone()
    }
}

/// Write the queued slots into the atlas texture. Waits while the atlas has no
/// GPU image yet (its first frame), and leaves what it could not write queued.
fn write_label_atlas(
    writes: Res<LabelAtlasWrites>,
    images: Res<RenderAssets<GpuImage>>,
    queue: Res<RenderQueue>,
) {
    let Some(atlas) = writes.atlas.as_ref() else {
        return;
    };
    let Some(gpu) = images.get(atlas) else {
        return;
    };
    let mut pending = writes.pending.lock().expect("label atlas queue");
    for (slot, raster) in pending.drain(..) {
        let (ox, oy) = ((slot % SLOT_COLS) * CELL_W, (slot / SLOT_COLS) * CELL_H);
        for (level, (w, h, texels)) in raster.levels.iter().enumerate() {
            let level = level as u32;
            queue.write_texture(
                TexelCopyTextureInfo {
                    texture: &gpu.texture,
                    mip_level: level,
                    origin: Origin3d {
                        x: ox >> level,
                        y: oy >> level,
                        z: 0,
                    },
                    aspect: TextureAspect::All,
                },
                texels,
                TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(w * 2),
                    rows_per_image: Some(*h),
                },
                Extent3d {
                    width: *w,
                    height: *h,
                    depth_or_array_layers: 1,
                },
            );
        }
    }
}

pub struct LabelAtlasPlugin;

impl Plugin for LabelAtlasPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LabelAtlasWrites>()
            .add_plugins(ExtractResourcePlugin::<LabelAtlasWrites>::default());
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app.add_systems(
                Render,
                write_label_atlas.in_set(RenderSystems::PrepareResources),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cells_keep_every_level_origin_whole() {
        let unit = 1u32 << (MIPS - 1);
        assert_eq!(CELL_W % unit, 0);
        assert_eq!(CELL_H % unit, 0);
        assert!(GUTTER >= unit, "the gutter must survive to the last level");
    }

    #[test]
    fn a_raster_fills_the_canvas_and_halves_per_level() {
        let (w, h) = (300u32, 50u32);
        let ink = vec![255u8; (w * h) as usize];
        let halo = vec![128u8; (w * h) as usize];
        let r = SlotRaster::new(&ink, &halo, w, h);
        assert_eq!(r.levels.len(), MIPS as usize);
        assert_eq!(r.levels[0].0, NAME_W);
        assert_eq!(r.levels[1].0, NAME_W / 2);
        assert!((r.u_frac - w as f32 / CELL_W as f32).abs() < 1e-6);
        assert!((r.v_frac - h as f32 / CELL_H as f32).abs() < 1e-6);
        // Inside the name both channels are what was given; the canvas past it
        // stays clear.
        let l0 = &r.levels[0].2;
        assert_eq!(l0[0], 255);
        assert_eq!(l0[1], 128);
        assert_eq!(l0[((w + 5) * 2) as usize], 0);
    }
}
