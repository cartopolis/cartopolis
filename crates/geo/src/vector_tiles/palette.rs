
/// Land nothing more is known about — and, at globe zooms, all land. A dry
/// khaki rather than paper white, so the planet reads as ground.
pub const PAPER: u32 = 0xA0_9A_80;
/// Water as the raster basemap paints it — and, more to the point, what the
/// *shaded* water of `systems::water` looks like from far enough that its
/// waves have merged, the same contract [`FOREST_CANOPIED`] has with the
/// trees.
///
/// The vector surfaces stream in a 3×3 cell ring around the camera
/// (`map_geometry::CELL_RADIUS`), so there is always a hard edge where the
/// geometry stops and this paint takes over. That edge is only invisible if
/// the two agree. They used to disagree by almost the whole value range —
/// pale `#A8C4DA` paint meeting water shaded from a near-black albedo — and
/// it read as a bright cut straight across every lake.
///
/// **Both sides of that comparison have to be *rendered* ones, and until
/// 2026-08-20 only one of them was.** The value was picked to land on the
/// water surface's shaded tone (`#36495C`–`#415462`, sampled from a render) —
/// but this is an albedo as well, painted into a tile texture that is then lit
/// by the same sun on a `perceptual_roughness: 1.0`, `reflectance: 0.0`
/// ground quad. So the paint got the sunlight *too*, and arrived on screen
/// well above the tone it had been matched to. The step came back, quietly,
/// as a brighter sea outside the vector ring than inside it — which is what a
/// square of darker water in the middle of the screen is, and it is visible on
/// a phone at any oblique angle because the ring's far edge crosses the view.
///
/// Measured at 53.07,5.34 from 12 km (nadir, 900×1900, `--shot-mode earth`,
/// same frame so the lighting is common to both): the ring's water rendered
/// `#425F7F` and the paint beside it `#5A7690` — a 24-level luminance step,
/// not the "residual blue cast" this comment used to promise. This value is
/// that measurement carried back through the ground quad's own lighting, so
/// the two now agree *on screen* rather than one of them agreeing with the
/// other's albedo.
///
/// So this is not a free colour choice: move `water::WATER_ALBEDO` and this
/// has to move with it — and re-sample **both** tones out of one frame, never
/// this constant against a render of that one.
///
/// The container's software rasteriser is trustworthy for the *step* (one
/// frame, one light, two materials) and not for the absolute tone, so the
/// direction and rough size of this move are measured and the last few levels
/// want an eye on real hardware.
///
/// It is pulled the other way too, and that one is silent. On a *raster*
/// basemap the globe has no geometry to ask, so it decides what is ocean by
/// how much bluer than red a texel is (`tile_loader::WATER_BLUE_BIAS`): this
/// has to stay at least 40 bluer than it is red or the planet quietly loses
/// its oceans. Matching the shaded tone wants it greyer, that wants it blue,
/// and this value is where the two settle — the residual blue cast at the ring
/// edge is the price. `tile_loader`'s
/// `the_basemaps_water_paint_reads_as_fully_water` holds the line.
pub const WATER: u32 = 0x2C_48_62;
pub const FOREST: u32 = 0x4A_5A_36;
/// Meadow olive: a muted green at the value of mown grass from the air.
///
/// Measured against PDOK's aerial photograph on 2026-10-01: Groningen's
/// lawns and parks sample at about (60–95, 87–113, 71–96), and the old
/// `0x6E8A4C` rendered sunlit at about (110, 125, 82) — a third too bright
/// and leaning yellow where the photo leans blue-green. A lawn is mostly
/// blades shading each other, which is why it is darker than the paint chip
/// says. This one tone feeds the flat fill, the image and the blades
/// (`systems::grass` derives from it), so they move together.
pub const GRASS: u32 = 0x52_6E_44;
/// Canopy tone: one step down the [`FOREST`] hue ramp, at the same muted
/// saturation — a game green here reads as confetti (see the module note).
pub const CANOPY: u32 = 0x4A_6A_36;
/// Second canopy tone, alternated per instance. Two tones buy depth without
/// costing a light.
pub const CANOPY_DEEP: u32 = 0x3C_5A_2C;
/// Trunks are a warm grey-brown kept *below* the canopy's value: a trunk
/// brighter than the leaves over it is the inversion dc01bc5 describes, and
/// a saturated brown at walking distance is the loudest colour on the map.
pub const TRUNK: u32 = 0x5A_4E_40;
/// What a forest polygon looks like from far enough that the individual trees
/// have merged: [`FOREST`] ground seen through ~65% [`CANOPY`] cover —
/// `mix(FOREST, CANOPY, 0.65)`, computed, not matched by eye. The fill ramps
/// to this as the trees fade out, so the aggregate brightness does not step
/// when the geometry goes away.
pub const FOREST_CANOPIED: u32 = 0x4A_64_36;
pub const SAND: u32 = 0xD6_C3_9A;
/// Glaciers and ice caps, from the land cover — a cool white, the one
/// near-white left on the land, because from orbit it is the one that is.
pub const ICE: u32 = 0xEE_F1_F4;
/// A tunnel's walls and roof. Darker than any surface in this palette on
/// purpose: it is the one thing here that is never lit by the sun, and a
/// concrete the tone of a pavement reads as a white box with a hole at each
/// end. The sky's ambient still reaches the open approach, so the opening
/// stays lighter than the tunnel without anything modelling it.
pub const TUNNEL: u32 = 0x6E_6B_66;
/// A quay wall: weathered brick and stone above a canal, darker than the
/// street on top of it so the edge reads from across the water.
pub const QUAY_WALL: u32 = 0x7A_6A_5C;
pub const RESIDENTIAL: u32 = 0xAB_A6_9C;
pub const INDUSTRIAL: u32 = 0xA6_A4_A0;
/// Built-up land as read from altitude. Up close, [`RESIDENTIAL`] and
/// [`INDUSTRIAL`] sit within a few counts of [`GRASS`] on purpose — streets
/// and buildings carry the figure there — but from ~z12 out those are
/// sub-pixel, the land layer is all that is left, and a whole province
/// rendered as one near-uniform green sheet with no visible cities. These are
/// the same land uses a hue step warmer and darker, used by the far style
/// band (`FAR_STYLE_TILE_M`) so a town reads as a town from the air.
pub const RESIDENTIAL_FAR: u32 = 0x92_8B_80;
pub const INDUSTRIAL_FAR: u32 = 0x8E_8C_88;
pub const SITE: u32 = 0xAD_A9_9E;
pub const BUILDING: u32 = 0x8F_85_7C;
pub const BUILDING_EDGE: u32 = 0x7A_70_68;
pub const ROAD_FILL: u32 = 0xFC_FB_F8;
pub const ROAD_CASING: u32 = 0xCB_C4_B6;
pub const MOTORWAY_FILL: u32 = 0xF4_DF_B4;
pub const MOTORWAY_CASING: u32 = 0xD8_BC_86;
pub const RAIL: u32 = 0xB4_AE_A4;
/// A bridge deck seen as a structure rather than as a stretch of road: the
/// concrete-and-asphalt grey of the whole thing, parapets included.
pub const BRIDGE_DECK: u32 = 0x57_56_59;
pub const PATH: u32 = 0xC5_C0_B0;
pub const BOUNDARY: u32 = 0xB0_A6_B4;
/// Base tone of the street furniture group (`systems::street_furniture`). The
/// per-part tones below are carried as vertex-colour *multipliers* against
/// this, exactly as the canopy's second tone is, so one material paints a whole
/// street's benches, bins and shelters.
pub const FURNITURE: u32 = 0xB8_B0_A2;
/// Timber: bench slats, table tops, swing seats. In the [`TRUNK`] family and
/// muted for the same reason — a saturated brown at walking distance is the
/// loudest colour on the map.
pub const FURNITURE_WOOD: u32 = 0xB4_A9_98;
/// Painted steel: posts, frames, bins.
pub const FURNITURE_METAL: u32 = 0x9A_98_93;
/// Roof and sign panels — a step *lighter* than the posts under them, so a
/// shelter still reads as a shelter from directly overhead, which is where the
/// orbit camera spends most of its time.
pub const FURNITURE_ROOF: u32 = 0xC6_C1_B6;
/// Masonry: a fountain rim, an artwork plinth.
pub const FURNITURE_STONE: u32 = 0xC0_BA_AE;
/// Glazing: a car's windows. Dark enough to read as a cabin against any body
/// colour, and grey rather than blue — a tinted windscreen on a pale map is
/// the same mistake a saturated bench is.
pub const VEHICLE_GLASS: u32 = 0x6E_74_79;
/// Rubber: tyres. The darkest tone in the group, and the one thing that makes
/// a box on the ground read as a vehicle rather than as a crate.
pub const VEHICLE_TYRE: u32 = 0x4A_47_45;
