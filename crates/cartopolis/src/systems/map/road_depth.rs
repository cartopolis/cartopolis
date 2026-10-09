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
//! decays nor runs away with range. `assets/shaders/road_depth.wesl` does it in
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

// The road material and the rung attribute live in `cartopolis_materials`, so
// that Bevy's per-material plugin code is generated there rather than in this
// crate.
pub use cartopolis_materials::road_depth::{RoadDepthExtension, RoadMaterial, ATTRIBUTE_ROAD_RUNG};

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
        let src = include_str!("../../../../../assets/shaders/road_depth.wesl");
        let line = src
            .lines()
            .find(|l| l.trim_start().starts_with("const RUNG_ULP"))
            .expect("road_depth.wesl declares RUNG_ULP");
        let value: f32 = line
            .split('=')
            .nth(1)
            .and_then(|v| v.trim().trim_end_matches(';').parse().ok())
            .expect("RUNG_ULP parses as a float");
        assert!(
            (value - ROAD_RUNG_ULP).abs() < f32::EPSILON,
            "road_depth.wesl says {value} ULPs per rung, the ladder says {ROAD_RUNG_ULP}",
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
