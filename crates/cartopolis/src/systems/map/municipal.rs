//! **Municipal registers**: the street objects a *city* surveys, where the
//! national ones are silent — and the public art Wikidata registers.
//!
//! `systems::surveyed` reads the BGT, which is national and is the right default
//! precisely because it is. What it is not is complete: its point collections are
//! *optional* content, filled per municipality, and the hole they leave is not at
//! the edges but in the middle. Over the cell that covers the study quay in
//! Groningen the BGT registers **no trees at all**; the city's own register has
//! 3,018. Street lighting is the same hole again, and the parking bays (35,000 of
//! them, one polygon each) are a register the BGT has no equivalent for at all.
//!
//! # Atlas reads the city; this reads atlas
//!
//! Until 2026-10-19 this module asked each city's own server, per cell, and held
//! the city's layer names, field names and vocabulary — and a reader fingerprint
//! in every cache key so a change to any of them invalidated what it had stored.
//! All of that is atlas's `pipeline/registers.rs` now. Atlas asks once per layer
//! per run and writes `registers/14/{x}/{y}.json`
//! (`cartopolis_geo::formats::registers`), and this module reads those files from
//! our own host like every other additive layer: behind a coverage manifest,
//! through [`CellSource`], keyed on the run's stamp so a re-run is never answered
//! from an old copy. No city's vocabulary reaches the client any more.
//!
//! # A register produces shapes the client already has
//!
//! A tree with a height, a prop with a [`FurnitureKind`], a parking bay. Trees
//! are merged into the canopy meshes; props go through
//! `furniture::place_surveyed`, which faces each one at the nearest street; bays
//! go to `parking::park_in_bays`.
//!
//! # A city register replaces the national one, per kind
//!
//! Two **registers** describe the same objects: the BGT's 112 trees in a cell are
//! a subset of the city's 5,014, so drawing both plants a second lime a metre from
//! the first. Matching them up by distance would be arbitration — a tolerance to
//! tune, wrong in both directions — so the overlap is removed instead: over a
//! cell a register covers, the kinds it [`supplies`](RegisteredCell::supplies)
//! are not drawn from the BGT or the map tile.
//!
//! It is removed **per kind, not per cell**. A city that registers trees and lamp
//! posts says nothing about benches, so suppressing the whole BGT cell over its
//! ground would delete the benches in order to fix the trees. And which kinds a
//! cell's registers speak for travels **in the cell file**: a covered cell with no
//! lamp posts in it is still the city's word that there are none.

use crate::platform::cell_source::CellSource;
use crate::systems::coverage::CoverageSlot;
use crate::systems::furniture::{FurnitureInstance, FurnitureKind};
use crate::systems::osm_buildings::tdbag_url;
use crate::systems::scatter::TreeInstance;
use cartopolis_geo::formats::registers::{self, SUPPLIES_TREES};

/// The zoom cells are cut at — `map_geometry::GEOMETRY_ZOOM`, because these
/// objects are baked into that cell's merged meshes and have to arrive on the
/// worker that builds them.
const REGISTER_ZOOM: u32 = cartopolis_geo::formats::CACHE_ZOOM;

/// Which cells the host serves registers for.
static COVERAGE: CoverageSlot =
    CoverageSlot::new("registers", "/registers/coverage.json", REGISTER_ZOOM);

/// Keep the coverage manifest fetched. A system of its own for the reason
/// `surveyed::prefetch_bgt_coverage` gives: its consumer, `map_geometry`, sits on
/// Bevy's parameter ceiling and has no clock.
pub fn prefetch_registers_coverage(time: bevy::prelude::Res<bevy::prelude::Time>) {
    COVERAGE.ensure(time.elapsed_secs_f64());
}

/// 0 while the coverage question is open — read by `map_geometry` as a rebuild
/// revision, since registered objects are baked into its merged meshes.
pub(crate) fn coverage_revision() -> u32 {
    COVERAGE.revision()
}

/// The credit line for what the host's registers carry, once coverage has loaded
/// and is non-empty. Not decoration: the city's data is CC BY, unlike the CC0
/// national registers around it, so the credit is a licence condition.
pub fn attribution() -> Option<String> {
    COVERAGE.attribution()
}

/// The bisecting switch, read once. One `std::env::var` per cell would be a
/// syscall on the mesh path for a value that cannot change.
static DISABLED: std::sync::LazyLock<bool> =
    std::sync::LazyLock::new(|| std::env::var("CARTO_NO_MUNICIPAL").is_ok());

/// One tree as its municipality registered it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RegisteredTree {
    pub lat: f64,
    pub lng: f64,
    /// Height of the crown above the ground in metres, when the register says.
    ///
    /// `None` is ordinary — Groningen leaves it blank on about one tree in thirty —
    /// and means "draw it the way an unmeasured street tree has always been drawn".
    pub height_m: Option<f32>,
}

/// One street object as a register placed it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RegisteredProp {
    pub lat: f64,
    pub lng: f64,
    pub kind: FurnitureKind,
    /// sRGB body colour where the register says something the form can show (a
    /// container's waste fraction); `None` is the colour the form always had.
    pub paint: Option<u32>,
}

/// A kind of content a register is the authority on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Supply {
    Trees,
    Prop(FurnitureKind),
}

/// One cell's registered objects, still in lat/lng, one vector per shape.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RegisteredCell {
    pub trees: Vec<RegisteredTree>,
    pub props: Vec<RegisteredProp>,
    /// Parking bays, one outer ring each.
    pub bays: Vec<Vec<(f64, f64)>>,
    /// What this cell's registers stand in for. Empty outside the coverage.
    pub supplies: Vec<Supply>,
}

impl RegisteredCell {
    /// Does a register stand in for this content over this cell? The question
    /// `map_geometry` asks of the BGT and the tile before drawing their copy.
    #[must_use]
    pub fn supplies(&self, supply: Supply) -> bool {
        self.supplies.contains(&supply)
    }

    /// Every prop kind a register stands in for here.
    #[must_use]
    pub fn supplied_props(&self) -> Vec<FurnitureKind> {
        self.supplies
            .iter()
            .filter_map(|s| match s {
                Supply::Prop(kind) => Some(*kind),
                Supply::Trees => None,
            })
            .collect()
    }
}

/// A cell file, read. A prop or a supply this build has no form for drops out.
fn parse_cell(text: &str) -> Option<RegisteredCell> {
    let doc = registers::parse(text).ok()?;
    Some(RegisteredCell {
        trees: doc
            .trees
            .into_iter()
            .map(|registers::Tree(lat, lng, height_m)| RegisteredTree { lat, lng, height_m })
            .collect(),
        props: doc
            .props
            .into_iter()
            .filter_map(|registers::Prop(lat, lng, slug, paint)| {
                Some(RegisteredProp {
                    lat,
                    lng,
                    kind: FurnitureKind::from_slug(&slug)?,
                    paint,
                })
            })
            .collect(),
        bays: doc.bays,
        supplies: doc
            .supplies
            .iter()
            .filter_map(|s| match s.as_str() {
                SUPPLIES_TREES => Some(Supply::Trees),
                slug => FurnitureKind::from_slug(slug).map(Supply::Prop),
            })
            .collect(),
    })
}

/// Keyed on the extraction run (`CoverageSlot::stamp`): nothing revalidates a
/// stored cell, so a re-run — a felled tree, a new statue — must not be answered
/// from the old copy.
fn cache_key(cx: i64, cy: i64) -> String {
    format!(
        "registers/{}/{REGISTER_ZOOM}/{cx}/{cy}.json",
        COVERAGE.stamp()
    )
}

const CELLS: CellSource<RegisteredCell> = CellSource {
    label: "registers",
    gate: Some(|cx, cy| !*DISABLED && COVERAGE.covers(cx, cy)),
    key: cache_key,
    url: Some(|cx, cy| {
        format!(
            "{}/registers/{REGISTER_ZOOM}/{cx}/{cy}.json?v={}",
            tdbag_url(),
            COVERAGE.stamp()
        )
    }),
    // Our own host: the coverage manifest already answers "does it serve this".
    breaker: None,
    timeout: None,
    parse: parse_cell,
};

/// One cell's registered objects. Empty outside the coverage, and when the host
/// cannot be reached — in both cases the caller's fallback is the layer it
/// already had.
pub(crate) async fn load_registered_cell(cx: i64, cy: i64) -> RegisteredCell {
    CELLS.load(cx, cy).await
}

// ─── Into the client's own builders ──────────────────────────────────────────

/// This cell's registered trees as instances, ready to join the scattered ones.
/// Objects outside the cell are dropped here.
#[must_use]
pub fn registered_trees(
    cell: &RegisteredCell,
    cx: i64,
    cy: i64,
    tile_m: f32,
    seed: u64,
) -> Vec<TreeInstance> {
    let points: Vec<(f32, f32, Option<f32>)> = cell
        .trees
        .iter()
        .filter_map(|t| {
            crate::systems::surveyed::cell_local(t.lat, t.lng, cx, cy, REGISTER_ZOOM, tile_m)
                .map(|(x, z)| (x, z, t.height_m))
        })
        .collect();
    crate::systems::scatter::surveyed_trees(&points, seed)
}

/// This cell's registered street objects, oriented against the tile's own
/// streets — the same builder the BGT's props go through.
#[must_use]
pub fn registered_props(
    cell: &RegisteredCell,
    tile: &crate::systems::vector_tiles::TileData,
    cx: i64,
    cy: i64,
    tile_m: f32,
    seed: u64,
) -> Vec<FurnitureInstance> {
    let points: Vec<(f32, f32, FurnitureKind, Option<u32>)> = cell
        .props
        .iter()
        .filter_map(|p| {
            crate::systems::surveyed::cell_local(p.lat, p.lng, cx, cy, REGISTER_ZOOM, tile_m)
                .map(|(x, z)| (x, z, p.kind, p.paint))
        })
        .collect();
    crate::systems::furniture::place_surveyed(tile, tile_m, seed, &points)
}

/// This cell's parking bays as rings in the tile-local metre frame. Kept whole
/// where a ring leaves the cell: atlas files a bay under the cell of its first
/// corner, and a bay is two metres across.
#[must_use]
pub fn registered_bays(cell: &RegisteredCell, cx: i64, cy: i64, tile_m: f32) -> Vec<Vec<[f32; 2]>> {
    cell.bays
        .iter()
        .map(|ring| {
            ring.iter()
                .map(|&(lat, lng)| {
                    let (x, z) = crate::systems::surveyed::cell_local_unclamped(
                        lat,
                        lng,
                        cx,
                        cy,
                        REGISTER_ZOOM,
                        tile_m,
                    );
                    [x, z]
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cell_reads_into_the_client_s_shapes() {
        let cell = parse_cell(concat!(
            r#"{"bays":[[[53.2,6.5],[53.2,6.5001],[53.20005,6.5001],[53.20005,6.5]]],"#,
            r#""props":[[53.2194,6.5665,"lamp"],[53.2195,6.5666,"recycling",4087180],"#,
            r#"[53.2196,6.5667,"hovercraft"]],"#,
            r#""supplies":["trees","lamp","hovercraft"],"trees":[[53.2196,6.5667,16.5],[53.2197,6.5668]]}"#
        ))
        .unwrap();
        assert_eq!(cell.trees.len(), 2);
        assert_eq!(cell.trees[0].height_m, Some(16.5));
        assert_eq!(cell.trees[1].height_m, None);
        // A slug this build does not know drops out, rather than being drawn as
        // something else.
        assert_eq!(cell.props.len(), 2);
        assert_eq!(cell.props[1].kind, FurnitureKind::Recycling);
        assert_eq!(cell.props[1].paint, Some(0x3E_5D_8C));
        assert_eq!(cell.bays.len(), 1);
        assert_eq!(
            cell.supplies,
            vec![Supply::Trees, Supply::Prop(FurnitureKind::Lamp)]
        );
    }

    /// Suppression is per kind: a cell that supplies trees and lamp posts says
    /// nothing about benches, so the BGT's benches must survive over it.
    #[test]
    fn a_register_supersedes_only_what_it_supplies() {
        let cell = parse_cell(r#"{"supplies":["trees","lamp","artwork"]}"#).unwrap();
        assert!(cell.supplies(Supply::Trees));
        assert!(cell.supplies(Supply::Prop(FurnitureKind::Lamp)));
        assert!(!cell.supplies(Supply::Prop(FurnitureKind::Bench)));
        assert_eq!(
            cell.supplied_props(),
            vec![FurnitureKind::Lamp, FurnitureKind::Artwork]
        );
        // An uncovered cell is the default, and stands in for nothing.
        assert!(!RegisteredCell::default().supplies(Supply::Trees));
    }

    #[test]
    fn a_bay_keeps_its_corners_in_the_cell_frame() {
        let (cx, cy) = (8491, 5320);
        let (north, west) = cartopolis_core::world::tile_to_lat_lng(cx, cy, REGISTER_ZOOM);
        let cell = RegisteredCell {
            bays: vec![vec![
                (north - 0.0001, west + 0.0001),
                (north - 0.0001, west + 0.0002),
            ]],
            ..RegisteredCell::default()
        };
        let rings = registered_bays(&cell, cx, cy, 600.0);
        assert_eq!(rings[0].len(), 2);
        assert!(rings[0][1][0] > rings[0][0][0], "east is +x");
        assert!(rings[0][0][1] > 0.0, "south of the north edge is +z");
    }
}
