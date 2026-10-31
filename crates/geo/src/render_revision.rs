//! Which renderer drew a stored basemap tile, named by what it draws.
//!
//! The client keeps every basemap tile it has drawn on disk
//! (`tile_loader::rendered_cache_key`), because drawing is the whole cost of a
//! warm start. A stored tile is only right for the renderer that drew it, so the
//! key carries a revision, and a new revision empties the store once.
//!
//! **The revision used to be a hash of this crate's source and `Cargo.lock`.**
//! That is never wrong, and it is nearly always a false alarm: 109 of the 318
//! commits to `main` between 2026-09-20 and 2026-10-04 touched one of them, on
//! 12 of those 14 days — routing, terrain grading, tunnels — so practically every
//! build a phone installed threw away every tile it had drawn and redrew the map
//! from nothing on its first launch.
//!
//! So the revision is now a hash of the **output**: the recorded tiles drawn
//! through every path the client stores ([`measure`]). A change that moves a
//! pixel fails `the_revision_names_what_the_renderer_draws`, which prints the
//! new value to paste in; a change that moves none leaves every device's store
//! alone. Nobody has to remember to bump it — the test is what remembers.
//!
//! What it cannot see is a colour for a kind of feature none of the recorded
//! tiles contains. Those are four real tiles (Groningen's centre and ring at
//! z14, two low-zoom tiles in the far style), plus painted-in fills of every
//! kind the client adds, so the gap is a rare land use restyled on its own;
//! such a change should bump [`RENDER_REVISION`] by hand.

/// The basemap renderer's revision. Part of every stored tile's key.
///
/// Update it to what `the_revision_names_what_the_renderer_draws` prints.
pub const RENDER_REVISION: u64 = 0xb467_52a1_1b47_2768;

/// FNV-1a, 64 bit: written out rather than `DefaultHasher`, whose output Rust
/// does not promise to keep between releases — a toolchain update would
/// otherwise move the revision without a pixel changing.
#[cfg(test)]
struct Fnv(u64);

#[cfg(test)]
impl Fnv {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
        // The length too, so two buffers cannot trade bytes across a seam.
        for b in (bytes.len() as u64).to_le_bytes() {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }
}

/// Draw the recorded tiles every way the client stores a drawn tile, and hash
/// the pixels.
#[cfg(test)]
fn measure() -> u64 {
    use crate::road_texture::SurveyedCover;
    use crate::testdata;
    use crate::vector_tiles::{
        ocean_mask_sized, render_mvt_layered, render_mvt_surveyed, CoveredCell, OverlayFill,
        SubTile, Survey, TileData, NOMINAL_Z14_TILE_M,
    };

    // A z10 tile's ground edge at Dutch latitudes, which puts the low-zoom
    // tiles in the far style as the client draws them.
    const Z10_TILE_M: f32 = NOMINAL_Z14_TILE_M * 16.0;

    let square = |x0: f32, y0: f32, x1: f32, y1: f32, rgb: u32| OverlayFill {
        rings: vec![vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1], [x0, y0]]],
        rgb,
    };
    // The client's painted-in layers: land cover under everything, crop parcels
    // over the land, surveyed paving under the streets, and a quarter of the
    // tile at each survey cover so both ribbon hand-overs are drawn.
    let underlay = vec![square(0.0, 0.0, 0.6, 0.6, 0x55_6B_3A)];
    let crops = vec![square(0.1, 0.5, 0.4, 0.9, 0xB8_A2_5C)];
    let survey = Survey {
        paving: vec![square(0.4, 0.3, 0.7, 0.6, 0x9C_5A_4A)],
        cells: vec![
            CoveredCell {
                bounds: [0.0, 0.0, 0.5, 0.5],
                cover: SurveyedCover::Paths,
            },
            CoveredCell {
                bounds: [0.5, 0.5, 1.0, 1.0],
                cover: SurveyedCover::Streets,
            },
        ],
    };

    let groningen = testdata::groningen();
    let ring = testdata::groningen_ring();
    let lowzoom: Vec<TileData> = testdata::LOWZOOM_TILES
        .iter()
        .map(|(name, bytes)| {
            TileData::parse(bytes).unwrap_or_else(|e| panic!("fixture {name} parses: {e}"))
        })
        .collect();
    let magnified = SubTile {
        factor: 8,
        x: 3,
        y: 5,
    };

    let mut h = Fnv::new();
    let mut flat = |tile: &TileData, sub: SubTile, size: u32, source_m: f32, painted: bool| {
        let (under, over, surveyed) = if painted {
            (&underlay[..], &crops[..], survey.clone())
        } else {
            (&[][..], &[][..], Survey::default())
        };
        let pixels = render_mvt_surveyed(tile, sub, size, source_m, under, over, &surveyed)
            .expect("a recorded tile draws");
        h.write(&pixels);
        h.write(&ocean_mask_sized(tile, sub, size).expect("a recorded tile has a mask"));
    };
    // The flat basemap, at both tile sizes a device uses, whole and magnified.
    for size in [512, 256] {
        flat(&groningen, SubTile::WHOLE, size, NOMINAL_Z14_TILE_M, true);
    }
    flat(&groningen, magnified, 512, NOMINAL_Z14_TILE_M, true);
    flat(&ring, SubTile::WHOLE, 512, NOMINAL_Z14_TILE_M, false);
    for tile in &lowzoom {
        flat(tile, SubTile::WHOLE, 256, Z10_TILE_M, true);
    }
    // The globe's tiles: land cover under, nothing over, no survey.
    for tile in &lowzoom {
        let pixels = render_mvt_layered(tile, SubTile::WHOLE, 256, Z10_TILE_M, &underlay, &[])
            .expect("a recorded tile draws");
        h.write(&pixels);
    }
    h.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **If this fails, the basemap looks different now** — set
    /// [`RENDER_REVISION`] to the value it prints, and every device redraws its
    /// stored tiles once. If you did not mean to change the picture, this is the
    /// test telling you that you did.
    #[test]
    fn the_revision_names_what_the_renderer_draws() {
        let measured = measure();
        assert_eq!(
            RENDER_REVISION, measured,
            "the basemap renderer draws different pixels now: set \
             `render_revision::RENDER_REVISION` to 0x{measured:016x}"
        );
    }
}
