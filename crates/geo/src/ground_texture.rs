//! The finish of the ground between the roads.
//!
//! `docs/notes/surface-finish-and-occlusion.md` measured the state this answers:
//! every ground surface in the app was one base colour at
//! `perceptual_roughness: 0.95`, and it named paving and the built-land fill as
//! "the largest remaining flat area in a street scene". Roads had already been
//! given a generated colour-plus-roughness map ([`crate::road_texture`]); the
//! pavement they run beside had nothing, so a street was a textured ribbon lying
//! on a perfectly uniform sheet.
//!
//! **Roughness only, deliberately.** The colour of the ground is the map's
//! palette — chosen, argued over, and reverted once already for being too
//! saturated — and this is not the place to start varying it. Roughness varies
//! how a surface *catches the sun* without touching a single tone, which is the
//! same reason the note gives for doing roughness before normal maps: it needs no
//! tangents and it cannot change the cartography.
//!
//! **One texture for every group that takes it**, rather than one per group. The
//! groups differ in colour and this says nothing about colour, so a second image
//! would be a second upload of the same information.
//!
//! It tiles, and it must: it is sampled from UV0, which on an area surface is
//! **metres from the map anchor** (`map_geometry::build_mesh`), so the sampler
//! wraps it across a whole city. Both octaves therefore wrap their lattice
//! indices — an octave that does not is a seam every repeat, i.e. a grid over the
//! ground at exactly the spacing the texture is scaled to.

/// Edge of the generated map, in texels. Small on purpose: it carries no shape,
/// only a level, and the mip chain is what is actually sampled at any distance
/// worth calling a street scene.
pub const GROUND_TEX_PX: u32 = 128;

/// How much roughness the map takes off at its darkest, against the group's own
/// scalar.
///
/// The map is a **multiplier** — bevy multiplies the green channel into
/// `perceptual_roughness` — so it can only go down from the material's level, and
/// 1.0 means "leave this texel alone". That is the same contract
/// [`crate::road_texture::RoadTexture::surface`] has and it is what lets
/// `systems::wet_surfaces` keep working unchanged: rain lowers the scalar, this
/// varies it, and the two multiply without either knowing about the other.
///
/// 0.25 against a 0.95 base puts the range at 0.71–0.95. That is a change in
/// sheen across a square, not a change from stone to polish — the top of the
/// range is still matte.
const ROUGHNESS_RANGE: f32 = 0.25;

/// The coarse octave's block size in texels. Divides [`GROUND_TEX_PX`], so it
/// wraps.
const COARSE_PX: u32 = 16;

/// A glTF-style metallic-roughness map for the flat ground groups: **G is
/// roughness, B is metallic** (always zero here — no ground fill is metal), R and
/// A unused. Linear coefficients, never sRGB.
///
/// Returns mip 0 as RGBA8; the caller builds the chain and uploads it.
#[must_use]
pub fn ground_finish_texture() -> Vec<u8> {
    let mut rgba = Vec::with_capacity((GROUND_TEX_PX * GROUND_TEX_PX * 4) as usize);
    for y in 0..GROUND_TEX_PX {
        for x in 0..GROUND_TEX_PX {
            // Two octaves, and they do different jobs. The fine one is the grain
            // you see underfoot; the coarse one is what survives three or four
            // mip levels and stops a whole square reading as one value from across
            // it — the same split, for the same reason, as `road_texture::shade`.
            let fine = hash2(x, y) - 0.5;
            let coarse = smooth_block(x, y) - 0.5;
            // Weighted toward the coarse octave: at any distance where the ground
            // is a large part of the frame, the fine octave is already averaged
            // away by the mips and only the blotches are left doing work.
            let n = (fine * 0.35 + coarse * 0.65).clamp(-0.5, 0.5) + 0.5;
            let rough = 1.0 - ROUGHNESS_RANGE * (1.0 - n);
            rgba.extend_from_slice(&[0, to_u8(rough), 0, 255]);
        }
    }
    rgba
}

/// The coarse octave: a value per [`COARSE_PX`] block, interpolated smoothly
/// between block centres so it has no hard edges of its own.
///
/// Interpolated rather than blocky because a hard-edged block *is* a visible
/// feature — at this scale it would read as paving slabs the size of a bus, which
/// is a thing the map would then appear to be claiming.
fn smooth_block(x: u32, y: u32) -> f32 {
    let blocks = GROUND_TEX_PX / COARSE_PX;
    let fx = x as f32 / COARSE_PX as f32;
    let fy = y as f32 / COARSE_PX as f32;
    let (bx, by) = (fx.floor(), fy.floor());
    let (tx, ty) = (smoothstep(fx - bx), smoothstep(fy - by));
    // Wrapped on the block lattice, which is what makes the whole map tile.
    let at = |ix: f32, iy: f32| {
        let wx = (ix as u32) % blocks;
        let wy = (iy as u32) % blocks;
        hash2(wx, wy + 512)
    };
    let a = at(bx, by);
    let b = at(bx + 1.0, by);
    let c = at(bx, by + 1.0);
    let d = at(bx + 1.0, by + 1.0);
    let top = a + (b - a) * tx;
    let bottom = c + (d - c) * tx;
    top + (bottom - top) * ty
}

fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

fn to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// The same cheap integer hash [`crate::road_texture`] uses, repeated rather than
/// shared: that one is private to a module about roads, and a ground finish
/// borrowing it would make the road texture's internals part of this module's
/// contract.
fn hash2(x: u32, y: u32) -> f32 {
    let mut h = x.wrapping_mul(0x9E37_79B9) ^ y.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2545_F491);
    h ^= h >> 13;
    (h & 0x00FF_FFFF) as f32 / 16_777_215.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_is_the_size_it_says_it_is() {
        assert_eq!(
            ground_finish_texture().len(),
            (GROUND_TEX_PX * GROUND_TEX_PX * 4) as usize
        );
    }

    /// The contract with bevy: the green channel multiplies `perceptual_roughness`,
    /// so a texel above 1.0 would make the ground *glossier* than its group asked
    /// for and a texel at 0 would make it a mirror. Neither is a thing this map is
    /// allowed to express — it varies a finish, it does not set one.
    #[test]
    fn roughness_only_ever_comes_down_and_never_to_zero() {
        let t = ground_finish_texture();
        let floor = to_u8(1.0 - ROUGHNESS_RANGE);
        for px in t.chunks_exact(4) {
            assert!(px[1] >= floor, "roughness {} below {floor}", px[1]);
            assert_eq!(px[2], 0, "nothing on the ground is metal");
        }
    }

    /// It is sampled with `Repeat` addressing over a whole city, so a value at one
    /// edge has to be the neighbour of the value at the other. A non-wrapping
    /// octave is the classic way to get this wrong, and the failure — one seam
    /// every repeat — looks like a grid somebody drew on the ground rather than
    /// like a bug.
    #[test]
    fn the_coarse_octave_wraps() {
        for i in 0..GROUND_TEX_PX {
            assert_eq!(
                smooth_block(0, i),
                smooth_block(GROUND_TEX_PX, i),
                "column seam at row {i}"
            );
            assert_eq!(
                smooth_block(i, 0),
                smooth_block(i, GROUND_TEX_PX),
                "row seam at column {i}"
            );
        }
    }

    /// If every texel came out the same the map would be an upload that does
    /// nothing, which is the failure a wrongly-scaled noise actually produces.
    #[test]
    fn it_is_not_flat() {
        let t = ground_finish_texture();
        let g: Vec<u8> = t.chunks_exact(4).map(|p| p[1]).collect();
        let lo = *g.iter().min().expect("texels");
        let hi = *g.iter().max().expect("texels");
        assert!(hi - lo > 20, "range {lo}..{hi} is not a variation");
    }
}
