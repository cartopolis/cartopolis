//! The crop grown on each agricultural field, as the farmer registered it: the
//! RVO's Basisregistratie Gewaspercelen (BRP).
//!
//! No Bevy: parcels in, [`SurfaceMesh`]es out. The streaming half is
//! `systems::crops`, the extraction is not written yet (it belongs in atlas's
//! `pipeline/`), and
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
//!   tone here is a muted, year-round one in the same pale cartographic family as
//!   the rest of the map (`scatter.rs` says why nothing here is saturated).
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
    /// Outer ring first, holes after. Open rings — no repeated closing vertex.
    pub rings: Vec<Vec<[f32; 2]>>,
}

/// What the register calls the *use* of a parcel. The BRP extractor, when it is
/// written in atlas's `pipeline/`, must mirror this order — **append, never
/// renumber**.
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
    /// Pale, like the rest of the map. These are wide fills over whole provinces:
    /// a saturated field would be the loudest thing on screen from any altitude.
    #[must_use]
    pub fn rgb(self) -> u32 {
        match self {
            Self::Grassland => 0xC9_DB_BB,
            // Worked ground with nothing more said about it: soil showing
            // through, warmer and paler than any growing crop.
            Self::Arable => 0xD9_CE_B5,
            Self::Nature => 0xC9_C6_A4,
            Self::Fallow => 0xD4_CC_BC,
            Self::Other => 0xD1_CE_C6,
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
#[must_use]
pub fn rgb(category: CropCategory, code: u32) -> u32 {
    match code {
        // ---- Grassland -------------------------------------------------
        // Permanent pasture: the default green of the countryside.
        265 => 0xC9_DB_BB,
        // A ley is reseeded and reads fresher than old pasture.
        266 => 0xD6_E3_C4,
        // Natural and semi-natural grassland under an agricultural claim: rough,
        // yellower, unmown.
        331 | 332 | 1926 => 0xC6_D1_A9,
        // ---- Cereals ---------------------------------------------------
        // Wheat, barley, oats, rye. Straw gold — the one crop whose colour
        // everybody already knows.
        233 | 234 | 235 | 236 | 238 | 670 | 7129 | 7130 => 0xE1_D3_A4,
        // ---- Maize -----------------------------------------------------
        // Two and a half metres of dense green; the darkest field in the country.
        259 | 316 | 317 | 814 | 1935 => 0xB4_C5_95,
        // ---- Potatoes --------------------------------------------------
        2014 | 2015 | 2016 | 2017 | 6632 => 0xC1_D1_A2,
        // ---- Root crops ------------------------------------------------
        // Sugar beet, red beet, chicory root, carrots: low dense canopy over
        // visible soil.
        256 | 511 | 2741 | 2783 | 2785 | 2787 => 0xBC_CB_B0,
        // ---- Onions ----------------------------------------------------
        // Blue-green over open ground between the rows.
        1932 | 1933 | 6660 | 6664 => 0xCE_D6_BE,
        // ---- Oilseed and fibre -----------------------------------------
        // Rape and flax. Rape is famously yellow for two weeks in May and green
        // either side of it; this is the year-round tone, not the fortnight.
        666 | 1922 | 1923 | 3736 => 0xD9_DB_98,
        // ---- Legumes and fodder ----------------------------------------
        244 | 258 | 311 | 426 => 0xC7_D8_AE,
        // ---- Cover crops -----------------------------------------------
        // `groenbemesting, vanggewas`: sown to be ploughed in. Uniform young
        // green, and the second most numerous thing on arable land after the
        // crop itself.
        6751 | 6755 | 6762 | 6783 | 6785 | 7126 => 0xCF_DE_B8,
        // ---- Flower bulbs ----------------------------------------------
        // Tulip, hyacinth, narcissus, lily, dahlia, gladiolus and the rest. A
        // faint lilac-grey: enough to mark the bulb country apart from the arable
        // around it, without painting a bloom that is only there in April.
        997..=1006 | 1012 | 1015 | 6803 => 0xD6_C7_C9,
        // ---- Nursery stock ---------------------------------------------
        // Avenue trees, fruit-tree rootstocks, hedging: rows of young trees on
        // open ground.
        1067 | 1071 | 1072 | 1077 | 1078 | 1079 | 1093 => 0xBE_C7_9F,
        // ---- Orchard ---------------------------------------------------
        // Apples and pears: standing trees, the deepest green here after maize.
        1096 | 1097 | 1098 => 0xB0_C3_94,
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
            rgb: rgb(p.category, p.code),
        })
        .collect();
    build_flat_fills(&fills, SurfaceGroup::Crop, max_bytes)
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
            rings: vec![vec![[0.0, 0.0], [size, 0.0], [size, size], [0.0, size]]],
        }
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
