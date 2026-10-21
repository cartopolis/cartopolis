//! The surveyed inputs a ground cell is built from — the BGT's paving, the crop
//! register's parcels, atlas's measured bridge decks — parsed and put into a
//! cell's local metre frame.
//!
//! Moved here from the client (`systems::surveyed`, `systems::crops`,
//! `systems::terrain_tiles`, which re-export it) so that atlas can build a cell's
//! ground sheets with exactly the code the client builds them with
//! (`crate::ground_sheets`). Nothing here knows about Bevy.

use crate::crops::{CropCategory, CropParcel};
use crate::formats::bgt::Surface as WireSurface;
use crate::paving::{PavedArea, PavingKind, PavingMaterial};
use cartopolis_core::world::tile_to_lat_lng;

/// The zoom the surveyed layers are binned at.
const BGT_ZOOM: u32 = crate::formats::CACHE_ZOOM;

/// The zoom the crop parcels are binned at — must match the ground cells'.
pub const BRP_ZOOM: u32 = 14;

/// The paved forms, in wire order. Mirrors `SURFACE_KINDS` in
/// atlas's `pipeline/bgt.rs` — **append, never renumber**.
const SURFACE_KINDS: [PavingKind; 13] = [
    PavingKind::Footway,
    PavingKind::Cycleway,
    PavingKind::Parking,
    PavingKind::Drive,
    PavingKind::Verge,
    PavingKind::Island,
    PavingKind::Paved,
    PavingKind::Steps,
    PavingKind::Street,
    PavingKind::LivingStreet,
    PavingKind::Road,
    PavingKind::Yard,
    PavingKind::Garden,
];

/// The surveyed materials, in wire order. Mirrors `SURFACE_MATERIALS` in
/// atlas's `pipeline/bgt.rs` — **append, never renumber**.
const SURFACE_MATERIALS: [PavingMaterial; 12] = [
    PavingMaterial::Asphalt,
    PavingMaterial::Concrete,
    PavingMaterial::Brick,
    PavingMaterial::ConcretePaver,
    PavingMaterial::Tiles,
    PavingMaterial::Stone,
    PavingMaterial::GrassPaver,
    PavingMaterial::Grass,
    PavingMaterial::Planting,
    PavingMaterial::Gravel,
    PavingMaterial::Sand,
    PavingMaterial::Unpaved,
];

// One surface on the wire: its form, its rings, and — only where the register
// names one — what it is made of.
//
// The material is a **trailing** element rather than a field, and the
// `#[serde(default)]` is what makes the two-element form still read. Two things
// need that. The register says nothing about the substance of roughly one drawn
// surface in six, so "unknown" has to cost no bytes; and a paving cell cached
// before this existed must keep parsing, because the alternative is that every
// stored cell in the country fails at once and silently re-downloads the second
// heaviest thing this client streams.
//
// The type is `crate::formats::bgt::Surface`, the one atlas writes.

/// One cell's surveyed paving, as rings of lat/lng: outer ring first, holes after.
#[derive(Clone, Debug, Default)]
pub struct SurveyedPaving {
    pub areas: Vec<(PavingKind, Option<PavingMaterial>, Vec<Vec<(f64, f64)>>)>,
}

/// Parse a cell's paving file: `{"surfaces":[[kind,[ring,…],material?],…]}`.
pub fn parse_surveyed_paving(text: &str) -> Result<SurveyedPaving, String> {
    let wire = crate::formats::bgt::parse_paving(text)?;
    // Every surface in the layer shares one rung of the depth ladder, so two
    // copies of one polygon are a per-pixel z-fight that re-rolls with the view
    // matrix — the pavement shimmers wherever the camera moves. The extractor
    // has shipped exactly that: the BGT API answers with every *version* of a
    // surface, superseded ones marked only by `eind_registratie`, and a filter
    // that tested `termination_date` alone kept them all (measured at 19 % exact
    // duplicates over a Groningen cell, one pavement seven times). The data is
    // fixed upstream; this is the floor under it, because a cell cached before
    // the fix keeps its ghosts for as long as the cache lives.
    let mut seen = std::collections::HashSet::new();
    Ok(SurveyedPaving {
        areas: wire
            .surfaces
            .into_iter()
            // An outer ring under three points is not a surface. The extractor
            // drops those; a truncated cache entry can produce one.
            .filter(|s| s.1.first().is_some_and(|r| r.len() >= 3))
            .filter(|WireSurface(k, rings, m)| {
                seen.insert((
                    *k,
                    *m,
                    rings
                        .iter()
                        .map(|r| {
                            r.iter()
                                .map(|&(lat, lng)| (lat.to_bits(), lng.to_bits()))
                                .collect::<Vec<_>>()
                        })
                        .collect::<Vec<_>>(),
                ))
            })
            .filter_map(|WireSurface(k, rings, m)| {
                // An unknown *form* drops the surface — there is nothing to draw
                // it as. An unknown *material* only drops the material: a
                // vocabulary this reader is too old for still leaves a pavement,
                // drawn the way it was drawn before materials existed.
                let kind = *SURFACE_KINDS.get(k)?;
                let material = m.and_then(|i| SURFACE_MATERIALS.get(i).copied());
                Some((kind, material, rings))
            })
            .collect(),
    })
}

/// This cell's paving in the local metre frame, ready to triangulate.
///
/// A ring is kept whole or not at all, and a surface whose outer ring leaves the
/// cell is dropped: the cell that owns the surface's first vertex draws it whole,
/// and clipping it here would leave a straight edge across the middle of a car
/// park. Holes that fall outside are dropped individually — a hole that is not
/// cut is a filled patch, which is wrong but local, where a dropped outer ring
/// loses the whole surface.
#[must_use]
pub fn surveyed_paving(
    paving: &SurveyedPaving,
    measured: &crate::formats::roofs::Cell,
    cx: i64,
    cy: i64,
    tile_m: f32,
) -> Vec<PavedArea> {
    let to_local = |pts: &Vec<(f64, f64)>| -> Option<Vec<[f32; 2]>> {
        let out: Vec<[f32; 2]> = pts
            .iter()
            .filter_map(|&(lat, lng)| to_cell_local(lat, lng, cx, cy, tile_m).map(|(x, z)| [x, z]))
            .collect();
        (out.len() == pts.len() && out.len() >= 3).then_some(out)
    };
    let polygon = |rings: &[Vec<(f64, f64)>]| -> Option<Vec<Vec<[f32; 2]>>> {
        let mut local = vec![to_local(rings.first()?)?];
        local.extend(rings[1..].iter().filter_map(to_local));
        Some(local)
    };
    let mut out = Vec::with_capacity(paving.areas.len());
    for (kind, material, rings) in &paving.areas {
        let Some(&first) = rings.first().and_then(|r| r.first()) else {
            continue;
        };
        let key = crate::formats::roofs::paving_key(first.0, first.1);
        // A garden atlas has split along the infrared photograph is drawn as
        // its two halves: the planted part as grass, the rest as hard ground.
        // They were cut by one overlay, so together they are the plot.
        if *kind == PavingKind::Garden {
            if let Some(split) = measured.gardens.get(&key) {
                for (polys, material) in [
                    (&split.planted, Some(PavingMaterial::Grass)),
                    (&split.paved, None),
                ] {
                    out.extend(
                        polys
                            .iter()
                            .filter_map(|p| polygon(p))
                            .map(|rings| PavedArea {
                                kind: PavingKind::Garden,
                                material,
                                rings,
                                measured: None,
                            }),
                    );
                }
                continue;
            }
        }
        let Some(local) = polygon(rings) else {
            continue;
        };
        // The photo's measurement, where atlas took one (`roofs` step).
        let measured = measured
            .paving
            .get(&key)
            .map(|m| u32::from(m.c[0]) << 16 | u32::from(m.c[1]) << 8 | u32::from(m.c[2]));
        out.push(PavedArea {
            kind: *kind,
            material: *material,
            rings: local,
            measured,
        });
    }
    out
}

/// Convert a lat/lng to the tile-local metre frame a cell is meshed in: metres
/// from the cell's **north-west corner**, +x east, +z south.
///
/// That is the frame `furniture.rs` and `scatter.rs` work in — they get it as
/// `tile_coordinate * (tile_m / extent)` off the MVT — and matching it exactly is
/// what lets surveyed objects be handed to the same builders as generated ones.
///
/// Points outside `[0, 1)` of the cell are the caller's to drop. They happen: the
/// extractor bins on the same rule, but a cell edge in floating point is not
/// bit-identical between Python and Rust, and a lamppost on the boundary would
/// otherwise land a whole cell away.
fn to_cell_local(lat: f64, lng: f64, cx: i64, cy: i64, tile_m: f32) -> Option<(f32, f32)> {
    cell_local(lat, lng, cx, cy, BGT_ZOOM, tile_m)
}

/// [`to_cell_local`] at an explicit zoom, for the other producers of surveyed
/// points that bin on the same rule — `systems::municipal`'s city registers.
///
/// Shared rather than copied for the reason [`cell_local_unclamped`] gives: the
/// containment test and the projection have to agree, and two callers who each
/// wrote their own is how they stop agreeing.
#[must_use]
pub fn cell_local(
    lat: f64,
    lng: f64,
    cx: i64,
    cy: i64,
    zoom: u32,
    tile_m: f32,
) -> Option<(f32, f32)> {
    let (north, west) = tile_to_lat_lng(cx, cy, zoom);
    let (south, east) = tile_to_lat_lng(cx + 1, cy + 1, zoom);
    if !(west..east).contains(&lng) || !(south..north).contains(&lat) {
        return None;
    }
    Some(cell_local_unclamped(lat, lng, cx, cy, zoom, tile_m))
}

/// The projection above without the containment test: the same frame, and an
/// answer outside `[0, tile_m)` for a point outside the cell.
///
/// Shared rather than copied because there is only one right way to do it and two
/// callers who must agree — the surveyed objects here, which are points and are
/// binned by containment, and the crop parcels (`systems::crops`), which are wide
/// polygons binned by centroid. **A parcel that leaves its cell is drawn whole**:
/// 22 % of the parcels a Groningen farmland cell owns leave it, and 24 % over
/// Flevoland (measured 2026-08-30, `docs/notes/crop-parcels.md`), so dropping them
/// the way a stray pavement is dropped would delete a quarter of the country's
/// fields.
pub fn cell_local_unclamped(
    lat: f64,
    lng: f64,
    cx: i64,
    cy: i64,
    zoom: u32,
    tile_m: f32,
) -> (f32, f32) {
    let (north, west) = tile_to_lat_lng(cx, cy, zoom);
    let (south, east) = tile_to_lat_lng(cx + 1, cy + 1, zoom);
    // Longitude is linear in the projection; latitude is not, so the north-south
    // fraction is taken in Mercator y rather than in degrees. Over one z14 cell
    // the difference is centimetres — but it is free to be right, and this is the
    // function every surveyed position passes through.
    let merc = |deg: f64| {
        let r = deg.to_radians();
        (r.tan() + 1.0 / r.cos()).ln()
    };
    let (my_n, my_s, my) = (merc(north), merc(south), merc(lat));
    let fx = (lng - west) / (east - west);
    let fy = (my_n - my) / (my_n - my_s);
    (fx as f32 * tile_m, fy as f32 * tile_m)
}

/// The categories the extractor can name, in wire order. The BRP extractor
/// (`CATEGORIES` in atlas's `pipeline/brp.rs`) mirrors this list — **append,
/// never renumber**.
///
/// Position 5 is the register's `Landschapselement` and is deliberately **not
/// drawable**. Sixty per cent of the features in the register are landscape
/// elements and most of those are ditches, which the Shortbread water layers
/// already draw — filling one with land puts ground where the register says
/// water. The extractor drops them; the position is claimed here so nothing can
/// ever quietly re-use it for a form that *is* drawn, and a record that arrives
/// in it is dropped rather than drawn as the category next to it.
const CATEGORIES: [Option<CropCategory>; 6] = [
    Some(CropCategory::Grassland),
    Some(CropCategory::Arable),
    Some(CropCategory::Nature),
    Some(CropCategory::Fallow),
    Some(CropCategory::Other),
    None,
];

/// One cell's crop parcels, still in lat/lng as the host sent them.
#[derive(Clone, Debug, Default)]
pub struct CropCell {
    pub parcels: Vec<(CropCategory, u32, Vec<Vec<(f64, f64)>>)>,
}

/// Parse a cell's crop file: `{"parcels":[[category,code,[ring,…]],…]}` — one
/// parcel on the wire is its category, its `gewascode`, and its rings as
/// lat/lng, outer first, holes after (`crate::formats::brp`).
pub fn parse_crop_cell(text: &str) -> Result<CropCell, String> {
    let wire = crate::formats::brp::parse(text)?;
    // Every parcel shares one rung of the depth ladder, so two copies of one
    // polygon are a per-pixel z-fight that re-rolls with the view matrix. The
    // register serves one year with no superseded versions in it — unlike the
    // BGT, which is where this fold was first needed (`parse_surveyed_paving`) —
    // but a cached cell outlives the extractor that wrote it, and the floor costs
    // one hash set.
    let mut seen = std::collections::HashSet::new();
    Ok(CropCell {
        parcels: wire
            .parcels
            .into_iter()
            // An outer ring under three points is not a parcel. The extractor
            // drops those; a truncated cache entry can produce one.
            .filter(|p| p.2.first().is_some_and(|r| r.len() >= 3))
            .filter(|(c, code, rings)| {
                seen.insert((
                    *c,
                    *code,
                    rings
                        .iter()
                        .map(|r| {
                            r.iter()
                                .map(|&(lat, lng)| (lat.to_bits(), lng.to_bits()))
                                .collect::<Vec<_>>()
                        })
                        .collect::<Vec<_>>(),
                ))
            })
            .filter_map(|(c, code, rings)| {
                // A category this build has no variant for — the reserved
                // landscape-element position, or a position from an extractor
                // newer than this client — drops the parcel. Clamping to the last
                // known category would paint a ditch as farmland.
                let category = (*CATEGORIES.get(c)?)?;
                Some((category, code, rings))
            })
            .collect(),
    })
}

/// This cell's parcels in the local metre frame, ready to triangulate.
///
/// **Projected unclamped.** Where the surveyed paving drops any surface whose
/// outer ring leaves its cell, the extractor cuts a field crossing a cell edge
/// along it (atlas's `brp::pieces`) — 22 % of a Groningen farmland cell's
/// parcels cross one, 24 % over Flevoland, measured 2026-08-30 — so each cell
/// holds exactly the ground it covers and cuts its own grass against all of it
/// (`crops::cut_into_ground`). A cell written by an older extractor, which binned
/// whole parcels on their centroid, still draws: its overhang lands over the
/// neighbour, as it always did.
#[must_use]
pub fn crop_parcels(cell: &CropCell, cx: i64, cy: i64, tile_m: f32) -> Vec<CropParcel> {
    let to_local = |pts: &Vec<(f64, f64)>| -> Option<Vec<[f32; 2]>> {
        (pts.len() >= 3).then(|| {
            pts.iter()
                .map(|&(lat, lng)| {
                    let (x, z) = cell_local_unclamped(lat, lng, cx, cy, BRP_ZOOM, tile_m);
                    [x, z]
                })
                .collect()
        })
    };
    cell.parcels
        .iter()
        .filter_map(|(category, code, rings)| {
            let outer = to_local(rings.first()?)?;
            let mut local = vec![outer];
            local.extend(rings[1..].iter().filter_map(to_local));
            Some(CropParcel {
                category: *category,
                code: *code,
                seed: seed_of(rings),
                rings: local,
            })
        })
        .collect()
}

/// A piece's [`crate::crops::field_seed`], off its first outer vertex
/// as the host sent it — the one input the mesh and the basemap share.
pub fn seed_of(rings: &[Vec<(f64, f64)>]) -> u32 {
    rings
        .first()
        .and_then(|r| r.first())
        .map_or(0, |&(lat, lng)| crate::crops::field_seed(lat, lng))
}

/// One deck as atlas published it (`crates/atlas/src/pipeline/terrain_grade.rs::
/// write_decks`): `[x, y, height, top?, profile?]` — its bounding-box centre in
/// fractions of the tile, its clearance in metres (0 for a deck laid flat), and,
/// trailing so a file from before them still reads, its median top in NAP and its
/// profile (`DeckProfile::to_wire`).
#[derive(Clone, Debug, PartialEq)]
pub struct AtlasDeck {
    pub at: [f32; 2],
    pub height: f32,
    pub top: Option<f32>,
    pub profile: Option<Vec<f64>>,
}

/// Every deck of a cell's heights out of atlas's list, by the same index as
/// `decks`, or `None` if any deck is not in it — the map changed since atlas
/// measured it, and the cell measures its own as before. Matched on the
/// bounding-box centre: both sides tessellate the same tile with the same code.
#[must_use]
pub fn match_atlas_decks(
    decks: &[crate::vector_tiles::BridgeDeck],
    tile_m: f32,
    atlas: &[AtlasDeck],
) -> Option<Vec<crate::deck_profile::DeckMeasure>> {
    // A metre, in tile fractions.
    let tol = 1.0 / tile_m;
    decks
        .iter()
        .map(|d| {
            let c = [
                (d.min[0] + d.max[0]) * 0.5 / tile_m,
                (d.min[1] + d.max[1]) * 0.5 / tile_m,
            ];
            atlas
                .iter()
                .find(|a| (a.at[0] - c[0]).abs() < tol && (a.at[1] - c[1]).abs() < tol)
                .map(|a| crate::deck_profile::DeckMeasure {
                    height: a.height,
                    top: a.top.filter(|t| t.is_finite() && a.height > 0.0),
                    // In tile fractions on the wire; this cell's metres here.
                    profile: a.profile.as_deref().and_then(|w| {
                        crate::deck_profile::DeckProfile::from_wire(w, f64::from(tile_m))
                    }),
                })
        })
        .collect()
}

/// Atlas's deck rows for a cell, as the matcher reads them.
#[must_use]
pub fn atlas_decks(cell: crate::formats::decks::Cell) -> Vec<AtlasDeck> {
    cell.decks
        .into_iter()
        .map(|d| AtlasDeck {
            at: [d.at[0] as f32, d.at[1] as f32],
            height: d.height as f32,
            top: d.top.map(|v| v as f32),
            profile: d.profile,
        })
        .collect()
}
