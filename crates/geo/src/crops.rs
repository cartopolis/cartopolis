//! The crop grown on each agricultural field, as the farmer registered it: the
//! RVO's Basisregistratie Gewaspercelen (BRP).
//!
//! No Bevy: parcels in, [`SurfaceMesh`]es out. The streaming half is
//! `systems::crops`, the extractor is atlas's `pipeline/brp.rs`, and
//! `docs/notes/crop-parcels.md` has the endpoint, the licence and the
//! measurements.
//!
//! # What this is for
//!
//! [`land_group`](crate::vector_tiles::land_group) collapses `grass`, `meadow`,
//! `park`, `garden`, `farmland`, `orchard`, `scrub`, `heath`, `allotments`,
//! `village_green` and `recreation_ground` into one [`SurfaceGroup::Grass`], so
//! everything outside a Dutch city is drawn in one flat green — a potato field,
//! an orchard and a municipal lawn are the same colour. The register names the
//! actual crop on every parcel in the country, and this draws it.
//!
//! # Added over the green, never instead of it
//!
//! A parcel is drawn as a fill on its own rung, one step above `Grass` and below
//! `Forest` — so where the register covers the ground the crop shows, and where
//! it does not (everywhere outside the Netherlands, and every host that has not
//! run the extractor) the map is byte-identical to the one this client always
//! drew. Value 4 in `docs/direction.md`.
//!
//! # What is deliberately not here
//!
//! * **`Landschapselement`.** Sixty per cent of the features are landscape
//!   elements and most of those are ditches — measured 316 of 340 sampled over
//!   the wider Groningen box on 2026-08-30. Those are *water*, drawn already from
//!   Shortbread's `water_polygons` / `water_lines`, and filling one with land is
//!   putting ground where the register says there is none. The category has no
//!   variant here and the parser drops it.
//! * **Season.** A tulip field is red for a fortnight in April and bare earth for
//!   the rest of the year; the register records the crop, not the flower. Every
//!   tone here is a year-round one — the crop's leaf, straw or soil — distinct
//!   enough between families to read which crop a field grows ([`rgb`]).
//! * **Geometry.** No furrows, no crop height, no rows. Flat fills.

use crate::flat_fill::{build_flat_fills, FlatFill};
use crate::vector_tiles::{palette, SurfaceGroup, SurfaceMesh};

/// One registered agricultural parcel, in cell-local metres.
#[derive(Debug, Clone)]
pub struct CropParcel {
    pub category: CropCategory,
    /// The register's `gewascode` — the national crop number, carried verbatim.
    /// A code this build has no tone for falls back to the category's, which is
    /// the whole reason it is a number and not an enum: the list runs to hundreds
    /// of crops and grows every year.
    pub code: u32,
    /// Which way this field's tone leans ([`field_rgb`]), from [`field_seed`].
    /// Taken from the parcel's lat/lng rather than from `rings`, because the
    /// basemap paints the same field from lat/lng and the two must agree.
    pub seed: u32,
    /// Outer ring first, holes after. Open rings — no repeated closing vertex.
    pub rings: Vec<Vec<[f32; 2]>>,
}

/// What the register calls the *use* of a parcel. The BRP extractor
/// (`CATEGORIES` in atlas's `pipeline/brp.rs`) mirrors this order — **append,
/// never renumber**.
///
/// There is no `Landschapselement`: see the module note. `Overige` is the
/// register's own residual category (permanent woodland with a replanting
/// obligation, field margins beside a wood), not a catch-all for a category this
/// build does not know — an unknown position on the wire is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CropCategory {
    /// `Grasland`: pasture and ley. About half the country's agricultural area,
    /// and the honest limit of this layer — green repainted a more accurate
    /// green.
    Grassland,
    /// `Bouwland`: worked ground. The category the crop codes below matter for.
    Arable,
    /// `Natuurterrein`: heath, rough grazing and other nature under an
    /// agricultural registration.
    Nature,
    /// `Braakland`: land lying fallow.
    Fallow,
    /// `Overige`: the register's residual — woodland, margins.
    Other,
}

impl CropCategory {
    /// The tone a parcel of this category draws in when its crop code is not one
    /// of the curated ones below.
    ///
    /// Grassland stays near the map's own green, because it is half the country
    /// and repainting it would recolour every province; the others are the
    /// ground's own tones — soil, heath — rather than a crop's.
    #[must_use]
    pub fn rgb(self) -> u32 {
        match self {
            Self::Grassland => 0x6E_8C_48,
            // Worked ground with nothing more said about it: soil showing
            // through, warmer and paler than any growing crop.
            Self::Arable => 0x9C_84_62,
            Self::Nature => 0x7A_7E_52,
            Self::Fallow => 0xA0_94_70,
            Self::Other => 0x8A_8A_74,
        }
    }

    /// Whether a blade of near-field grass may stand on this parcel
    /// ([`crate::turf`]).
    ///
    /// False for exactly one category, and it fixes something that already ships:
    /// `farmland` is one of the OSM kinds `land_group` calls `Grass`, so a Dutch
    /// potato field grows generated lawn today.
    #[must_use]
    pub fn grows_grass(self) -> bool {
        !matches!(self, Self::Arable)
    }
}

/// The tone one parcel is drawn in: its crop's where this build has one, its
/// category's otherwise.
///
/// **An unlisted code never takes a neighbouring crop's colour.** The fallback is
/// the category, which is the same discipline `land_group` follows for an
/// unlisted OSM kind: an unstyled polygon beats a wrongly-styled one. The register
/// carries several hundred codes and adds more each year, so most of them will
/// always land here.
///
/// The curated set is the crops that cover real area, chosen from a national
/// sample taken on 2026-08-30 (`docs/notes/crop-parcels.md` has the method). Every
/// code below was read off the live service; a code that looks like it belongs to
/// a family here but was not observed is deliberately absent rather than guessed.
///
/// **The families are meant to be told apart at a glance**, which the first
/// palette was not: it kept every crop in the map's pale cartographic range, and
/// potatoes, maize and pasture came out within a few steps of one green
/// (reported 2026-09-27, the first time the layer was seen on a real map). Each
/// arable family now sits at least 30 apart in RGB from every other and from
/// pasture — `the_crop_families_can_be_told_apart` holds that — while staying a
/// field's own colour: straw, soil, leaf, blossom.
#[must_use]
pub fn rgb(category: CropCategory, code: u32) -> u32 {
    match code {
        // ---- Grassland -------------------------------------------------
        // Permanent pasture: the default green of the countryside.
        265 => 0x6E_8C_48,
        // A ley is reseeded and reads fresher than old pasture.
        266 => 0x7A_9A_50,
        // Natural and semi-natural grassland under an agricultural claim: rough,
        // yellower, unmown.
        331 | 332 | 1926 => 0x8A_8C_5A,
        // ---- Cereals ---------------------------------------------------
        // Wheat, barley, oats, rye. Straw gold — the one crop whose colour
        // everybody already knows.
        233 | 234 | 235 | 236 | 238 | 670 | 7129 | 7130 => 0xC8_AE_6C,
        // ---- Maize -----------------------------------------------------
        // Two and a half metres of dense, yellowish green.
        259 | 316 | 317 | 814 | 1935 => 0x4A_76_30,
        // ---- Potatoes --------------------------------------------------
        // Ridged earth between the haulm rows: the brown field.
        2014 | 2015 | 2016 | 2017 | 6632 => 0x7E_62_48,
        // ---- Root crops ------------------------------------------------
        // Sugar beet, red beet, chicory root, carrots: a low, dense, blue-green
        // leaf canopy.
        256 | 511 | 2741 | 2783 | 2785 | 2787 => 0x3C_7C_50,
        // ---- Onions ----------------------------------------------------
        // Grey-blue over open ground between the rows.
        1932 | 1933 | 6660 | 6664 => 0x8C_94_80,
        // ---- Oilseed and fibre -----------------------------------------
        // Rape and flax. Rape is famously yellow for two weeks in May and green
        // either side of it; a lime yellow, greener than the cereals.
        666 | 1922 | 1923 | 3736 => 0xB8_BC_50,
        // ---- Legumes and fodder ----------------------------------------
        244 | 258 | 311 | 426 => 0x86_A8_3E,
        // ---- Cover crops -----------------------------------------------
        // `groenbemesting, vanggewas`: sown to be ploughed in. Uniform young
        // green, and the second most numerous thing on arable land after the
        // crop itself.
        6751 | 6755 | 6762 | 6783 | 6785 | 7126 => 0x86_A5_5A,
        // ---- Flower bulbs ----------------------------------------------
        // Tulip, hyacinth, narcissus, lily, dahlia, gladiolus and the rest. The
        // light sandy soil the bulbs are grown in, which is what the field is for
        // all but the fortnight it flowers — a bloom's colour would be right in
        // April and wrong the rest of the year.
        997..=1006 | 1012 | 1015 | 6803 => 0xB8_9C_84,
        // ---- Nursery stock ---------------------------------------------
        // Avenue trees, fruit-tree rootstocks, hedging: rows of young trees on
        // open ground.
        1067 | 1071 | 1072 | 1077 | 1078 | 1079 | 1093 => 0x5C_64_50,
        // ---- Orchard ---------------------------------------------------
        // Apples and pears: standing trees, the deepest green here.
        1096 | 1097 | 1098 => 0x3A_5A_2E,
        // ---- Green fallow ----------------------------------------------
        // Registered as arable and lying fallow, so it takes the fallow tone
        // rather than the worked-ground one.
        6794 => CropCategory::Fallow.rgb(),
        // ---- Registered woodland ---------------------------------------
        // The map already has a colour for a wood, and a wood the register knows
        // about should not be a different one.
        1936 => palette::FOREST,
        _ => category.rgb(),
    }
}

/// A field's seed for [`field_rgb`]: a hash of its first outer vertex, in
/// micro-degrees so the mesh and the basemap — which both start from the same
/// lat/lng — cannot round it differently.
#[must_use]
pub fn field_seed(lat: f64, lng: f64) -> u32 {
    let q = |v: f64| (v * 1e6).round() as i64 as u64;
    let mut h = q(lat).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ q(lng);
    h ^= h >> 31;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 29;
    h as u32
}

/// The tone one field is actually drawn in: its crop's ([`rgb`]), leaned a
/// little drier or lusher per field.
///
/// **Without it the countryside is one colour whatever the palette says.** Over
/// the Groningen coverage 2,524 of 3,015 pieces are one crop — permanent pasture,
/// code 265 (counted 2026-09-27) — so a crop palette, however well separated,
/// paints the whole box one green. What makes grassland read as a patchwork from
/// the air is that neighbouring fields were mown, grazed and fertilised on
/// different days: some straw-yellow, some deep green. Grassland takes the full
/// swing; other categories a third of it, since their crop already carries the
/// variety.
///
/// A parcel that crosses a cell edge is two pieces with two seeds, so it can
/// change tone along that edge. Keying on a parcel id instead needs one on the
/// wire, which the extractor does not write yet.
#[must_use]
pub fn field_rgb(category: CropCategory, code: u32, seed: u32) -> u32 {
    let base = rgb(category, code);
    let swing = match category {
        CropCategory::Grassland => 1.0,
        _ => 0.35,
    };
    // Uniform in [-1, 1]: negative leans lush, positive leans dry.
    let t = ((seed >> 8) as f32 / (1u32 << 24) as f32) * 2.0 - 1.0;
    // The two ends as per-channel multipliers, mirror images about 1.0 so a
    // run of fields averages back to the crop's own tone. Dry lifts red and
    // drops blue (hay); lush is the reverse, a cooler fresh sward.
    const DRY: [f32; 3] = [1.10, 1.05, 0.90];
    const LUSH: [f32; 3] = [0.90, 0.95, 1.10];
    let end = if t >= 0.0 { DRY } else { LUSH };
    let k = t.abs() * swing;
    let mut out = 0u32;
    for (i, shift) in [16, 8, 0].into_iter().enumerate() {
        let c = ((base >> shift) & 0xFF) as f32;
        let m = 1.0 + (end[i] - 1.0) * k;
        out |= ((c * m).round().clamp(0.0, 255.0) as u32) << shift;
    }
    out
}

/// Triangulate a cell's parcels into merged meshes, each no larger than
/// `max_bytes`.
///
/// One group, one material, one tone per parcel in the vertex colours — the
/// contract `Canopy`, `Furniture` and `Paving` already follow.
#[must_use]
pub fn build_crop_meshes(parcels: &[CropParcel], max_bytes: Option<usize>) -> Vec<SurfaceMesh> {
    let fills: Vec<FlatFill<'_>> = parcels
        .iter()
        .map(|p| FlatFill {
            rings: &p.rings,
            rgb: field_rgb(p.category, p.code, p.seed),
            pattern: 0,
            shade: None,
        })
        .collect();
    build_flat_fills(&fills, SurfaceGroup::Crop, max_bytes)
}

/// Make the crop parcels and the rest of a cell's ground a partition: every
/// sheet *above* `Crop` on the ground stack (woods, sand, water, paving, the
/// road sheet) is cut out of the parcels, and the parcels are then cut out of
/// every sheet *below* (`Built`, `Grass`).
///
/// **The ladder alone does not hold this pair.** A parcel is almost always laid
/// over OSM's `farmland`/`meadow`, which is `Grass`, so every field in the
/// country was two opaque sheets over one piece of ground. Flat, the half-step
/// rung between them was enough; with relief on, each sheet is bent onto the
/// measured ground against its own triangles and the two disagree by up to
/// `CREASE_SAG_M` — measured on the Deck on 2026-09-26 as every field drawn in
/// blotches of its crop's tone and the grass under it. The rule
/// `the-ground-stack.md` has for this is the one applied here: remove the
/// overlap rather than order it.
///
/// `meshes` holds the cell's other flat ground; anything in it that is not a
/// sheet (trees, furniture, tunnels) neither cuts nor is cut.
pub fn cut_into_ground(meshes: &mut Vec<SurfaceMesh>, crops: &mut Vec<SurfaceMesh>) {
    if crops.is_empty() {
        return;
    }
    let crop = SurfaceGroup::Crop.layer();
    let (mut below, rest): (Vec<_>, Vec<_>) = std::mem::take(meshes).into_iter().partition(|m| {
        let l = m.group.layer();
        l.is_sheet() && l < crop
    });
    let (above, rest): (Vec<_>, Vec<_>) = rest.into_iter().partition(|m| {
        let l = m.group.layer();
        l.is_sheet() && l > crop
    });
    crate::road_stitch::cut_under_roads(crops, &above);
    crate::road_stitch::cut_under_roads(&mut below, crops);
    meshes.extend(below);
    meshes.extend(above);
    meshes.extend(rest);
}

/// The arable parcels' triangles, for the near-field grass keep-out
/// ([`crate::turf`]).
///
/// Triangulated here rather than in `turf` so there is one earcut of a parcel per
/// cell and one place that knows what a parcel's rings mean.
#[must_use]
pub fn arable_triangles(parcels: &[CropParcel]) -> Vec<[(f32, f32); 3]> {
    let mut out = Vec::new();
    for parcel in parcels.iter().filter(|p| !p.category.grows_grass()) {
        let Some(outer) = parcel.rings.first() else {
            continue;
        };
        if outer.len() < 3 {
            continue;
        }
        let mut flat: Vec<f64> = Vec::with_capacity(outer.len() * 2);
        for p in outer {
            flat.push(f64::from(p[0]));
            flat.push(f64::from(p[1]));
        }
        let mut holes = Vec::new();
        for ring in &parcel.rings[1..] {
            if ring.len() < 3 {
                continue;
            }
            holes.push(flat.len() / 2);
            for p in ring {
                flat.push(f64::from(p[0]));
                flat.push(f64::from(p[1]));
            }
        }
        let Ok(tris) = earcutr::earcut(&flat, &holes, 2) else {
            continue;
        };
        for t in tris.chunks_exact(3) {
            let p = |i: usize| (flat[i * 2] as f32, flat[i * 2 + 1] as f32);
            out.push([p(t[0]), p(t[1]), p(t[2])]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(category: CropCategory, code: u32, size: f32) -> CropParcel {
        CropParcel {
            category,
            code,
            seed: 0,
            rings: vec![vec![[0.0, 0.0], [size, 0.0], [size, size], [0.0, size]]],
        }
    }

    fn area(meshes: &[SurfaceMesh], group: SurfaceGroup) -> f32 {
        meshes
            .iter()
            .filter(|m| m.group == group)
            .flat_map(|m| {
                m.indices.chunks_exact(3).map(move |t| {
                    let p = |i: u32| m.positions[i as usize];
                    let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
                    ((b[0] - a[0]) * (c[2] - a[2]) - (b[2] - a[2]) * (c[0] - a[0])).abs() / 2.0
                })
            })
            .sum()
    }

    fn sheet(group: SurfaceGroup, x0: f32, x1: f32) -> Vec<SurfaceMesh> {
        let rings = vec![vec![[x0, x0], [x1, x0], [x1, x1], [x0, x1]]];
        build_flat_fills(
            &[FlatFill {
                rings: &rings,
                rgb: 0,
                pattern: 0,
                shade: None,
            }],
            group,
            None,
        )
    }

    /// The grass under a field goes, the field under a canal goes, and nothing
    /// else changes size: after the cut no two sheets share ground.
    #[test]
    fn crops_and_the_ground_around_them_become_a_partition() {
        let mut meshes = sheet(SurfaceGroup::Grass, 0.0, 100.0);
        meshes.extend(sheet(SurfaceGroup::Water, 50.0, 70.0));
        let mut crops = build_crop_meshes(&[square(CropCategory::Arable, 2014, 60.0)], None);
        cut_into_ground(&mut meshes, &mut crops);
        let (grass, water, crop) = (
            area(&meshes, SurfaceGroup::Grass),
            area(&meshes, SurfaceGroup::Water),
            area(&crops, SurfaceGroup::Crop),
        );
        // Field 60×60 less the 10×10 corner of canal lying over it.
        assert!((crop - 3500.0).abs() < 1.0, "crop {crop}");
        assert!((water - 400.0).abs() < 1.0, "water {water}");
        // The grass keeps only what neither the field nor the canal covers…
        // except that the canal is *above* grass and was never cut out of it:
        // that pair is the ground fills' own partition (`tessellate_surfaces`),
        // not this one. So: 100² − the field's 3500.
        assert!((grass - 6500.0).abs() < 1.0, "grass {grass}");
    }

    #[test]
    fn a_cell_with_no_parcels_is_left_alone() {
        let mut meshes = sheet(SurfaceGroup::Grass, 0.0, 100.0);
        let before = area(&meshes, SurfaceGroup::Grass);
        cut_into_ground(&mut meshes, &mut Vec::new());
        assert_eq!(area(&meshes, SurfaceGroup::Grass), before);
    }

    #[test]
    fn a_parcel_becomes_triangles_facing_up() {
        let meshes = build_crop_meshes(&[square(CropCategory::Arable, 2014, 40.0)], None);
        assert_eq!(meshes.len(), 1);
        assert_eq!(meshes[0].group, SurfaceGroup::Crop);
        assert_eq!(meshes[0].indices.len(), 6);
        assert!(meshes[0].normals.iter().all(|n| n[1] > 0.9));
    }

    /// The register does not promise a winding, and a field wound the other way
    /// round must still face the sky — the same rule, and the same failure, as
    /// the paving.
    #[test]
    fn a_reversed_ring_still_faces_up() {
        let mut parcel = square(CropCategory::Grassland, 265, 40.0);
        parcel.rings[0].reverse();
        let meshes = build_crop_meshes(&[parcel], None);
        for t in meshes[0].indices.chunks_exact(3) {
            let p = |i: u32| meshes[0].positions[i as usize];
            let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
            let cross = (b[0] - a[0]) * (c[2] - a[2]) - (b[2] - a[2]) * (c[0] - a[0]);
            assert!(cross < 0.0, "triangle wound face-down: {a:?} {b:?} {c:?}");
        }
    }

    /// The whole safety argument for a register that adds crop codes every year:
    /// a code this build has never heard of draws as its own category and never
    /// as some other crop that happens to sit nearby in the match.
    #[test]
    fn an_unknown_code_falls_back_to_its_category() {
        // 8888 is not a gewascode; it stands in for next year's.
        assert_eq!(rgb(CropCategory::Arable, 8888), CropCategory::Arable.rgb());
        assert_eq!(
            rgb(CropCategory::Grassland, 8888),
            CropCategory::Grassland.rgb()
        );
        // …and it is not any curated crop's tone.
        for code in [233, 259, 2014, 256, 6660, 1004, 1096, 6751] {
            assert_ne!(
                rgb(CropCategory::Arable, 8888),
                rgb(CropCategory::Arable, code),
                "an unknown code drew as crop {code}"
            );
        }
    }

    /// A curated code has to actually change a pixel, or the table is a wire
    /// field nobody sees.
    #[test]
    fn a_curated_code_overrides_the_category_tone() {
        for code in [233, 259, 2014, 1004, 1096] {
            assert_ne!(
                rgb(CropCategory::Arable, code),
                CropCategory::Arable.rgb(),
                "crop {code} draws as generic arable"
            );
        }
        assert_ne!(rgb(CropCategory::Arable, 6794), CropCategory::Arable.rgb());
        assert_eq!(rgb(CropCategory::Arable, 1936), palette::FOREST);
    }

    /// Two categories that draw the same are a distinction the wire is carrying
    /// for nothing.
    #[test]
    fn every_category_has_its_own_tone() {
        use CropCategory::*;
        let cats = [Grassland, Arable, Nature, Fallow, Other];
        for (i, a) in cats.iter().enumerate() {
            for b in &cats[i + 1..] {
                assert_ne!(a.rgb(), b.rgb(), "{a:?} and {b:?} share a tone");
            }
        }
    }

    /// The same one crop-family over: a curated tone that collides with another
    /// is a family that cannot be told from its neighbour in the field.
    #[test]
    fn every_curated_family_has_its_own_tone() {
        let families = [
            265, 266, 331, 233, 259, 2014, 256, 6660, 666, 244, 6751, 1004, 1071, 1096,
        ];
        for (i, a) in families.iter().enumerate() {
            for b in &families[i + 1..] {
                assert_ne!(
                    rgb(CropCategory::Arable, *a),
                    rgb(CropCategory::Arable, *b),
                    "crops {a} and {b} share a tone"
                );
            }
        }
    }

    /// Different is not enough: the first palette passed the test above with
    /// potatoes, maize and pasture a few steps apart in one pale green, and
    /// nobody could tell them apart on the map. Every arable family, and
    /// pasture, is at least `MIN` apart in RGB from every other.
    #[test]
    fn the_crop_families_can_be_told_apart() {
        const MIN: f32 = 30.0;
        let families = [265, 233, 259, 2014, 256, 6660, 666, 244, 1004, 1071, 1096];
        let dist = |a: u32, b: u32| {
            let ch = |v: u32, s: u32| ((v >> s) & 0xFF) as f32;
            [16, 8, 0]
                .iter()
                .map(|&s| (ch(a, s) - ch(b, s)).powi(2))
                .sum::<f32>()
                .sqrt()
        };
        let mut all: Vec<(u32, u32)> = families
            .iter()
            .map(|&c| (c, rgb(CropCategory::Arable, c)))
            .collect();
        all.push((0, CropCategory::Arable.rgb()));
        for (i, (a, ca)) in all.iter().enumerate() {
            for (b, cb) in &all[i + 1..] {
                let d = dist(*ca, *cb);
                assert!(d >= MIN, "crops {a} and {b} are only {d:.0} apart");
            }
        }
    }

    /// A run of pasture fields must not come out one colour — that is the whole
    /// reason `field_rgb` exists — and must still average to pasture, or the
    /// countryside drifts off the tone the palette chose for it.
    #[test]
    fn neighbouring_pastures_differ_and_average_to_pasture() {
        let base = rgb(CropCategory::Grassland, 265);
        let tones: Vec<u32> = (0..200)
            .map(|i| {
                let seed = field_seed(53.25 + f64::from(i) * 1e-3, 6.6);
                field_rgb(CropCategory::Grassland, 265, seed)
            })
            .collect();
        let distinct: std::collections::HashSet<_> = tones.iter().collect();
        assert!(distinct.len() > 40, "only {} tones", distinct.len());
        for shift in [16, 8, 0] {
            let ch = |v: u32| f64::from((v >> shift) & 0xFF);
            let mean = tones.iter().map(|&t| ch(t)).sum::<f64>() / tones.len() as f64;
            assert!(
                (mean - ch(base)).abs() < 8.0,
                "channel {shift} averages {mean:.0} against {}",
                ch(base)
            );
        }
    }

    /// Grass grows on a meadow and not on a ploughed field. This is the value-1
    /// defect the layer fixes, so it is asserted at the source of the answer as
    /// well as in `turf`.
    #[test]
    fn only_arable_keeps_the_grass_out() {
        assert!(!CropCategory::Arable.grows_grass());
        for c in [
            CropCategory::Grassland,
            CropCategory::Nature,
            CropCategory::Fallow,
            CropCategory::Other,
        ] {
            assert!(c.grows_grass(), "{c:?} should still grow blades");
        }
    }

    #[test]
    fn a_degenerate_ring_is_skipped_rather_than_drawn() {
        let parcel = CropParcel {
            category: CropCategory::Arable,
            code: 2014,
            seed: 0,
            rings: vec![vec![[0.0, 0.0], [1.0, 0.0]]],
        };
        assert!(build_crop_meshes(&[parcel], None).is_empty());
        assert!(build_crop_meshes(&[], None).is_empty());
    }

    /// The byte cap is what keeps a province of fields off the single-asset
    /// upload cliff (value 5), so it has to actually cut.
    #[test]
    fn the_byte_cap_cuts_a_cell_into_pieces() {
        let parcels: Vec<CropParcel> = (0..64)
            .map(|_| square(CropCategory::Arable, 2014, 40.0))
            .collect();
        let whole = build_crop_meshes(&parcels, None);
        let split = build_crop_meshes(&parcels, Some(crate::flat_fill::TRI_BYTES * 8));
        assert_eq!(whole.len(), 1);
        assert!(split.len() > 1, "the cap should have cut it up");
        let tris = |m: &[SurfaceMesh]| m.iter().map(|x| x.indices.len()).sum::<usize>();
        assert_eq!(
            tris(&whole),
            tris(&split),
            "cutting must not lose triangles"
        );
    }

    #[test]
    fn only_arable_parcels_reach_the_keep_out() {
        let tris = arable_triangles(&[
            square(CropCategory::Arable, 2014, 40.0),
            square(CropCategory::Grassland, 265, 40.0),
        ]);
        // One square, two triangles — the meadow contributed nothing.
        assert_eq!(tris.len(), 2);
    }
}
