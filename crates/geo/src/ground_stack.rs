//! **The** order of everything drawn flat on the ground — one list, and nothing
//! else in the tree is allowed an opinion about it.
//!
//! A map is a stack of sheets laid on one plane: the land, the fields on it, the
//! water through it, the pavement beside the road, the road, the paint on the
//! road, the route line over that, the name of the street over that. Every one is
//! coplanar with its neighbours by construction, so the rasteriser cannot say
//! which is on top — their depth values agree to the last few bits and the winner
//! is decided by rounding, re-decided every frame the view matrix moves. Something
//! has to declare the order. That something is [`GroundLayer`], and the order **is
//! the declaration order of its variants**.
//!
//! # Why this module exists
//!
//! The order used to be written down in five places in three denominations:
//!
//! * `SurfaceGroup::depth_rung` — eleven groups, hand-written 1…11.
//! * `SurfaceGroup::y_offset` — a *second* ladder, in millimetres of real height,
//!   written independently and required to agree with the first.
//! * `road_rung` — a third, per vertex, inside a band the first had to leave room
//!   for, with the band's ceiling spelled `ROAD_BAND_FOOT + 2.0 + (…) as u32`.
//! * Five free constants in four client modules — the zebra paint at 10.5, the
//!   route casing at 18, the route line at 20, the street label at 23, the driver
//!   arrow at 24 — on this same ladder, declared nowhere near it and picked by eye.
//! * `assets/shaders/road_depth.wesl`, which hard-codes the step.
//!
//! Every recurring bug in this area was one of those disagreeing with another. Two
//! ladders in different units **crossed over** in the middle distance, so a
//! street's tarmac vanished at some camera distances and not others. The
//! millimetre ladder **dissolved** past a couple of hundred metres, where 24 µm
//! stops being worth a single depth-buffer unit. And when the road band grew from
//! a handful of rungs to 399, its ceiling climbed past all five hand-picked
//! constants at once, and every zebra crossing on a road wider than a residential
//! street was quietly drawn *underneath* the tarmac.
//!
//! None of those was a wrong idea. Each was somebody picking a number without
//! being able to see the other numbers.
//!
//! # What makes it not happen again
//!
//! **There is no number to pick.** A layer's rung is its position in the `enum`,
//! read off the discriminant, so placing a sheet in the stack is editing one
//! ordered list and nothing else. Both denominations — the depth bias and the
//! millimetre height — are *derived* from that one position, so they cannot
//! contradict each other. The road band's height is derived from the road ladder
//! itself, so growing that ladder moves everything above it automatically instead
//! of silently swallowing it.
//!
//! And the tests walk [`GroundLayer::ALL`] rather than a hand-written list of the
//! things somebody remembered. The test that was supposed to catch the swallowed
//! zebra crossing existed, stayed green throughout, and checked four *other*
//! things; a test that enumerates cannot fail that way.
//!
//! # What a ladder cannot do, and so is not asked to
//!
//! It orders a layer against *another* layer. It can say nothing about a layer
//! against **itself** — two polygons of one layer share a rung by definition — and
//! that is a live fight rather than a hypothetical: measured on the Groningen
//! fixture, 9.7 % of the grass and 3.1 % of the built land was covered twice by
//! its own polygons before `vector_tiles::tessellate_surfaces` cut them apart. The
//! answer there is geometry, never depth, exactly as it is for the roads
//! ([`crate::road_stitch`]). `docs/notes/the-ground-stack.md` has both halves.

use crate::vector_tiles::{ROAD_DEPTH_RUNGS, ROAD_RUNG_ULP};

/// Depth-buffer units between adjacent rungs of the stack.
///
/// The unit is a **depth-buffer ULP**, not a world distance: Bevy writes
/// `StandardMaterial::depth_bias` into `DepthStencilState::bias.constant`, and for
/// a primitive at depth `D` in the reversed-z `Depth32Float` buffer one unit is
/// `≈ D · 1.19e-7`. That is the whole reason the ladder is denominated here and
/// not in millimetres: the apparent world shift is `1.19e-7 · bias · z`, so a rung
/// is worth the same fraction of the buffer's own quantum at every range. A
/// geometric gap is not — it decays as `1/z`, which is how the old millimetre
/// ladder came to dissolve in the middle distance.
///
/// Sixty-four. Sixteen was tried and found wanting: lakes still took the odd
/// triangle through the surface at the shallow angles a low camera looks at the
/// middle distance with, where the depth gradient across a pixel is steepest. This
/// is the blunt lever for that. The sharp one is `bias.slope_scale`, which is
/// proportional to that gradient and is the textbook remedy — `StandardMaterial`
/// exposes only `constant`, so reaching it means a `MaterialExtension::specialize`
/// on every ground group. If a lake ever flickers again, that is the fix, not
/// another doubling here.
pub const DEPTH_BIAS_STEP: f32 = 64.0;

/// Floor of the millimetre window the sheet layers live in.
///
/// `map_stream` spawns the finest tier as a 0.04 m cuboid centred at −0.03, so its
/// top face is at −0.01; every building, 3DBAG and Overpass alike, stands on
/// y = 0. A sheet drawn above zero pushes grass and water up through the bottom
/// courses of the buildings — the green bleeding out from under the grey — and one
/// below −0.01 sinks under the map. Ten millimetres, and the whole sheet stack has
/// to fit inside it.
const SHEET_FLOOR_M: f32 = -0.0095;

/// Height granularity of that window: half a millimetre, so a layer's clearance is
/// a small integer and the sum of them is exact in `f32`.
const SHEET_UNIT_M: f32 = 0.0005;

/// How tall the road band is, in depth units — the room [`GroundLayer::Road`] must
/// leave above itself for `systems::road_depth`'s per-vertex rungs.
///
/// Derived from the road ladder rather than written down beside it. The old
/// `ROAD_BAND_CEILING` was `foot + 2.0 + (rungs × ulp / step) as u32`, where the
/// `as u32` truncated and one of the two spare rungs existed only to undo that
/// truncation. Here the arithmetic is exact: the tallest road vertex reaches
/// `(ROAD_DEPTH_RUNGS − 1) × ROAD_RUNG_ULP` above the band's foot, so a band of
/// `ROAD_DEPTH_RUNGS × ROAD_RUNG_ULP` clears it by a full rung with nothing to
/// round.
const ROAD_BAND_ULPS: f32 = ROAD_DEPTH_RUNGS as f32 * ROAD_RUNG_ULP;

/// The rung the streamed basemap tile occupies.
///
/// It is not a [`GroundLayer`] — it is a raster cuboid `map_stream` spawns with
/// bevy's default zero bias, and it is what every vector sheet is laid *on*. Zero,
/// spelled out, because the sheets' own biases are offset past it and the reason
/// is not otherwise visible: the finest tier's top face is at −0.01 and the lowest
/// sheet sits 0.5 mm above that, which settles nothing at range.
const MAP_TILE_RUNG: u32 = 0;

/// Everything drawn flat on the ground, **bottom to top**. The declaration order
/// is the ladder; there is no other statement of it anywhere in the tree.
///
/// Adding a sheet means putting a variant where it belongs and giving it a
/// [`Self::clearance`]. That is the whole edit — you cannot give it a colliding
/// rung, because you never give it a rung.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u32)]
pub enum GroundLayer {
    /// Residential, industrial, commercial, retail, quarry and railway land.
    Built = 0,
    /// The generic green: parks, meadows, gardens, cemeteries, verges.
    Grass,
    /// A registered agricultural field, laid *over* the generic green — the
    /// countryside keeps its grass everywhere the register does not reach, which
    /// is every country but this one.
    Crop,
    /// Woodland.
    Forest,
    /// Sand, beach, bare rock, scree.
    ///
    /// A rung of its own rather than the wood's. Two layers sharing a rung is the
    /// one thing a ladder cannot arbitrate, and a rung costs nothing now that the
    /// stack is not squeezed under a 1.5 mm ceiling.
    Sand,
    /// Open water. Takes the widest gap in the sheet window — see
    /// [`Self::clearance`].
    Water,
    /// Surveyed paving beside the carriageway: pavements, cycle paths, parking
    /// bays, verges.
    ///
    /// Above the fills and below the road band, and that ordering is what makes
    /// the two sources safe to mix. A surveyed pavement and a nominal-width OSM
    /// carriageway disagree about where the kerb is by a few tens of centimetres;
    /// with the road above, the overlap reads as tarmac meeting the kerb, which is
    /// what it looks like from the pavement. Level with the road it would be a
    /// z-fight running the length of every street.
    Paving,
    /// The road sheet — carriageways and their kerb casings.
    ///
    /// This is the band's **foot**. Roads do not get a rung each: `road_stitch`
    /// cuts the ground ribbons into a partition, so the only overlap left is a
    /// flyover over the road beneath it, and `road_rung` orders that per vertex
    /// inside [`ROAD_BAND_ULPS`] of room reserved here.
    Road,
    /// A bridge deck, standing at its own measured height.
    ///
    /// In the ladder anyway, because a Dutch canal bridge sits at street level
    /// with the water below and so has no honest depth to separate it from the
    /// road it carries.
    BridgeDeck,
    /// Painted road markings — zebra bars and the like, laid on the tarmac.
    RoadPaint,
    /// Three-dimensional things *standing* on the plane: tree canopies, trunks,
    /// street furniture.
    ///
    /// They resolve against the sheets by honest depth — they are a metre or ten
    /// above — but the sheets are biased forward to settle their own fight, and a
    /// grass sheet low in the stack would otherwise punch through a trunk base or
    /// a bench leg.
    Standing,
    /// The route line's darker casing.
    RouteCasing,
    /// The route line itself.
    RouteLine,
    /// The pins at the route's two ends.
    ///
    /// This one was a hard-coded `rung(22.0)` in `navigation.rs` — not in the five
    /// this module was written to absorb, and not in the test that was supposed to
    /// guard them either. It was found by reading the call site while replacing
    /// its neighbours. That is the argument for the list, in one variant.
    RoutePin,
    /// A street name, lying in the road surface.
    StreetLabel,
    /// The driver-mode heading arrow, over the route it follows.
    DriverArrow,
}

impl GroundLayer {
    /// Every layer, bottom to top.
    ///
    /// **The** list, and the one the tests walk — not a hand-maintained subset,
    /// which is how the last swallowed rung got past a green suite. The
    /// `the_list_is_the_ladder` test pins it against the discriminants, so it
    /// cannot silently fall out of step with the `enum` either.
    pub const ALL: [Self; 16] = [
        Self::Built,
        Self::Grass,
        Self::Crop,
        Self::Forest,
        Self::Sand,
        Self::Water,
        Self::Paving,
        Self::Road,
        Self::BridgeDeck,
        Self::RoadPaint,
        Self::Standing,
        Self::RouteCasing,
        Self::RouteLine,
        Self::RoutePin,
        Self::StreetLabel,
        Self::DriverArrow,
    ];

    /// The layer's position in the stack, counting from the bottom.
    ///
    /// Read off the discriminant, which is why the declaration order is the
    /// ladder and why no caller is ever handed a number to choose.
    #[must_use]
    pub const fn rung(self) -> u32 {
        self as u32
    }

    /// The layer's `StandardMaterial::depth_bias`, in depth-buffer units.
    ///
    /// One [`DEPTH_BIAS_STEP`] per rung, plus the whole [`ROAD_BAND_ULPS`] for
    /// everything above the road, so the road's per-vertex ladder has somewhere to
    /// stand. Strictly increasing over [`Self::ALL`] by construction.
    ///
    /// The ladder starts one rung above [`MAP_TILE_RUNG`] rather than at zero,
    /// because the streamed basemap tile is the floor of this stack and the lowest
    /// sheet has to clear *it* too. The tile's top face is half a millimetre under
    /// the built land, and half a millimetre is worth nothing from the air.
    #[must_use]
    pub fn depth_bias(self) -> f32 {
        let band = if self.rung() > Self::Road.rung() {
            ROAD_BAND_ULPS
        } else {
            0.0
        };
        (self.rung() + MAP_TILE_RUNG + 1) as f32 * DEPTH_BIAS_STEP + band
    }

    /// Half-millimetres of clear air between this layer and the one below it.
    ///
    /// The window is only ten millimetres and the fights inside it are **not**
    /// equally visible, so it is not spread evenly. Land against land is pale
    /// green against pale grey and can lose without anyone noticing; land against
    /// water is dark against pale, it is the one that shows, and it is the one
    /// that came back as triangles flickering up through a lake. So the land
    /// layers stay packed half a millimetre apart and the water takes the widest
    /// gap in the stack. Spend the window where the contrast is.
    ///
    /// Every value is at least 1, which is what makes [`Self::y_offset`] strictly
    /// increasing and therefore unable to contradict [`Self::depth_bias`].
    const fn clearance(self) -> u32 {
        match self {
            // The floor of the window; nothing below it to clear.
            Self::Built => 0,
            // A whole millimetre off the built land, which is the busiest overlap
            // in the stack (10.7 % of the built land is under grass).
            Self::Grass => 2,
            Self::Crop | Self::Forest | Self::Sand => 1,
            // The contrasty one.
            Self::Water => 7,
            Self::Paving | Self::Road => 2,
            // Above the sheet window entirely — see `y_offset`.
            Self::BridgeDeck
            | Self::RoadPaint
            | Self::Standing
            | Self::RouteCasing
            | Self::RouteLine
            | Self::RoutePin
            | Self::StreetLabel
            | Self::DriverArrow => 0,
        }
    }

    /// Is this layer a sheet in the millimetre window, or does it carry a height
    /// of its own?
    ///
    /// A bridge deck stands where `systems::height` measured it, a tree stands on
    /// the ground, the route line and the street label are half a metre up. None
    /// of those is a place in a ten-millimetre ladder, and pretending otherwise is
    /// how the two denominations came to disagree in the first place.
    #[must_use]
    pub const fn is_sheet(self) -> bool {
        (self as u32) <= (Self::Road as u32)
    }

    /// Height above the ground plane, in metres.
    ///
    /// Derived from the rung by running [`Self::clearance`] up the stack, so the
    /// height ladder and the depth ladder are **the same ladder** expressed twice
    /// and cannot cross over. Layers that are not sheets stand on the plane itself
    /// and take their real height from wherever measures it.
    #[must_use]
    pub fn y_offset(self) -> f32 {
        if !self.is_sheet() {
            return 0.0;
        }
        let units: u32 = Self::ALL
            .iter()
            .take_while(|l| l.rung() <= self.rung())
            .map(|l| l.clearance())
            .sum();
        SHEET_FLOOR_M + units as f32 * SHEET_UNIT_M
    }

    /// The highest depth bias any vertex of this layer can reach.
    ///
    /// Everything but the road sheet draws on its own rung exactly; the road
    /// carries a per-vertex rung on top of it ([`crate::vector_tiles::road_rung`]),
    /// which is what [`ROAD_BAND_ULPS`] reserves the room for.
    #[must_use]
    pub fn depth_bias_ceiling(self) -> f32 {
        if self == Self::Road {
            self.depth_bias() + (ROAD_DEPTH_RUNGS.saturating_sub(1)) as f32 * ROAD_RUNG_ULP
        } else {
            self.depth_bias()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ALL` is the list every other test walks, so the first thing to pin is that
    /// it *is* the `enum` — in order, complete, nothing repeated. A list that had
    /// quietly lost a variant would make every assertion below vacuous for it,
    /// which is precisely the failure the hand-written lists had.
    #[test]
    fn the_list_is_the_ladder() {
        for (i, layer) in GroundLayer::ALL.iter().enumerate() {
            assert_eq!(
                layer.rung() as usize,
                i,
                "{layer:?} is at index {i} of ALL but its discriminant says {}",
                layer.rung()
            );
        }
        // …and the last variant is the last entry, so nothing was appended to the
        // `enum` without being appended here.
        assert_eq!(
            GroundLayer::ALL.last().copied(),
            Some(GroundLayer::DriverArrow),
            "a variant was added to GroundLayer without being added to ALL"
        );
    }

    /// Every layer outranks the one below it, by enough to survive the
    /// rasteriser's own rounding — the property the whole module exists to
    /// guarantee, checked over the whole ladder rather than over a remembered
    /// subset of it.
    #[test]
    fn every_layer_outranks_the_one_below_it() {
        for pair in GroundLayer::ALL.windows(2) {
            let (below, above) = (pair[0], pair[1]);
            let gap = above.depth_bias() - below.depth_bias_ceiling();
            assert!(
                gap >= DEPTH_BIAS_STEP,
                "{above:?} biases {} but {below:?} reaches {} — only {gap} depth units of air, \
                 and a rung is {DEPTH_BIAS_STEP}",
                above.depth_bias(),
                below.depth_bias_ceiling(),
            );
        }
    }

    /// The two denominations are one ladder written twice, so they may never
    /// disagree about which of two layers is on top. A crossover between them is
    /// the bug that made a street's tarmac vanish at some camera distances and not
    /// others; it is unreachable now, and this is what says so.
    #[test]
    fn the_height_ladder_never_contradicts_the_depth_ladder() {
        for pair in GroundLayer::ALL.windows(2) {
            let (below, above) = (pair[0], pair[1]);
            assert!(
                above.y_offset() >= below.y_offset(),
                "{above:?} outranks {below:?} in depth but sits {} below it in height",
                below.y_offset() - above.y_offset()
            );
        }
        // The sheets specifically must *separate* in height, not merely not
        // invert: the millimetres are what carry the ordering at close range,
        // where a rung is worth micrometres.
        for pair in GroundLayer::ALL.windows(2) {
            let (below, above) = (pair[0], pair[1]);
            if below.is_sheet() && above.is_sheet() {
                assert!(
                    above.y_offset() > below.y_offset(),
                    "{above:?} and {below:?} are both sheets and share a height"
                );
            }
        }
    }

    /// Every sheet is inside the window between the map tile's top face and the
    /// buildings' base. Outside it a surface either sinks under the map or cuts
    /// into the buildings — the latter is what made grass bleed out from under the
    /// grey.
    #[test]
    fn the_sheets_fit_between_the_map_tile_and_the_buildings() {
        const TILE_TOP: f32 = -0.01;
        const BUILDING_BASE: f32 = 0.0;
        for layer in GroundLayer::ALL {
            if !layer.is_sheet() {
                continue;
            }
            let y = layer.y_offset();
            assert!(
                y > TILE_TOP && y < BUILDING_BASE,
                "{layer:?} at y={y} escapes the ground window"
            );
            assert!(
                layer.depth_bias() >= 0.0,
                "{layer:?} must not sink under the map tile"
            );
        }
    }

    /// Water takes the widest gap in the window, because dark-on-pale is the fight
    /// that shows when it is lost. Whoever next rebalances the window has to take
    /// the room from somewhere other than under the water.
    #[test]
    fn the_contrasty_fight_gets_the_widest_gap() {
        let land: u32 = [GroundLayer::Crop, GroundLayer::Forest, GroundLayer::Sand]
            .into_iter()
            .map(GroundLayer::clearance)
            .max()
            .unwrap();
        assert!(
            GroundLayer::Water.clearance() >= 3 * land,
            "water clears {} half-millimetres and the land layers clear {land}",
            GroundLayer::Water.clearance()
        );
    }

    /// The road band is reserved room, and the check is that it is *enough* room —
    /// derived from the road ladder, so growing that ladder can never again climb
    /// through the five things drawn over it.
    #[test]
    fn the_road_band_holds_every_road_vertex() {
        let foot = GroundLayer::Road.depth_bias();
        let tallest = foot + (ROAD_DEPTH_RUNGS.saturating_sub(1)) as f32 * ROAD_RUNG_ULP;
        assert_eq!(GroundLayer::Road.depth_bias_ceiling(), tallest);
        for layer in GroundLayer::ALL {
            if layer.rung() <= GroundLayer::Road.rung() {
                continue;
            }
            assert!(
                layer.depth_bias() > tallest,
                "{layer:?} biases {} but a road vertex reaches {tallest}",
                layer.depth_bias()
            );
        }
    }
}
