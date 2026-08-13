//! The budgets themselves: what one tile of each pipeline stage is allowed to cost.
//!
//! These are the tests `docs/notes/tile-raster-cost.md` ends by saying do not
//! exist ("The benchmarks were reverted, not committed — nothing in the tree
//! measures this"). They are not benchmarks. They assert allocation counts and
//! rasteriser input, both of which are exact and machine-independent, so they run
//! as ordinary tests in the same GPU-less container that cannot render a frame —
//! and in CI, where a wall-clock number would only ever flake.
//!
//! Read `perf.rs` for what these numbers do and do not prove. The short version:
//! they catch allocation-shaped regressions (a `Vec` per triangle, a `String` per
//! tag, a `Value` walk) and they catch geometry being handed to the rasteriser
//! twice; they do **not** catch a rasteriser that got slower per pixel.
//!
//! **The budgets are measured, not chosen.** Each was read off
//! `print_tile_pipeline_cost` below and then given ~2% of headroom — enough to
//! absorb an unrelated commit shifting a `HashMap` growth threshold, and no more,
//! because these counts are reproducible to the unit. A tenth was tried first and
//! rejected on evidence: it let an injected regression through. See the note beside
//! the constants. Re-run that hatch after any deliberate change:
//!
//! ```text
//! cargo test -p cartopolis_geo print_tile_pipeline_cost -- --ignored --nocapture
//! ```

use crate::perf::{self, Cost};
use crate::vector_tiles::{SubTile, TileData};

/// The tile every measurement in `docs/notes/tile-raster-cost.md` was taken on:
/// 495 KB of Groningen at z14, 8,552 buildings and 95 streets.
const GRONINGEN: &[u8] = include_bytes!("../tests/fixtures/groningen_z14.mvt");

/// A sparser tile, so a regression that only shows up on dense data is
/// distinguishable from one that scales with the fixed overhead of a parse.
const LOWZOOM: &[u8] = include_bytes!("../tests/fixtures/lowzoom_10_526_334.mvt");

fn parse(bytes: &[u8]) -> TileData {
    TileData::parse(bytes).expect("fixture parses")
}

/// Feature count of a named layer, decoding it through the memo.
fn layer_features(tile: &TileData, name: &str) -> usize {
    tile.layer(name).map_or(0, |l| tile.features(l).len())
}

/// Decode every layer, so a later measurement sees rasterisation and not the
/// lazy decode landing inside it.
///
/// This is not bookkeeping — measuring without it conflates two stages and
/// produces a number that reads as nonsense (a 256² render appearing to cost
/// *more* than the same tile at 512², because whichever ran first paid for the
/// decode of every layer it touched). Warming makes each budget measure one thing.
fn warm(tile: &TileData) {
    let names: Vec<String> = tile.layers().iter().map(|l| l.name.clone()).collect();
    for name in names {
        let _ = layer_features(tile, &name);
    }
}

/// A sub-tile one eighth on each axis, i.e. 1/64 of the source tile's ground.
const SUB_1_64: SubTile = SubTile {
    factor: 8,
    x: 3,
    y: 3,
};

// ---------------------------------------------------------------------------
// Budgets
// ---------------------------------------------------------------------------

/// Decoding a tile body must not allocate per coordinate.
///
/// This is the guard on the parse half of the pipeline. It is a *ceiling on a
/// decode that is already known to be wasteful* — `mvt-reader` clones a `String`
/// for every tag key and every tag value of every feature — so the number is
/// large. That is deliberate: pinning today's cost is what makes the improvement
/// visible when someone vendors or patches that dependency, and stops the cost
/// from growing quietly in the meantime.
#[test]
fn parsing_a_dense_tile_stays_within_its_allocation_budget() {
    let (_tile, cost) = Cost::measure(|| parse(GRONINGEN));
    perf::within(cost, PARSE_DENSE_ALLOCS, "TileData::parse (Groningen z14)");
}

#[test]
fn parsing_a_sparse_tile_stays_within_its_allocation_budget() {
    let (_tile, cost) = Cost::measure(|| parse(LOWZOOM));
    perf::within(cost, PARSE_SPARSE_ALLOCS, "TileData::parse (z10 lowzoom)");
}

/// Reading a layer's features twice must cost the decode once.
///
/// `PAINT_ORDER` lists `buildings` twice (fill, then outline) and `streets` twice
/// (casing, then fill), and `TileData` carries a per-layer `OnceCell` precisely so
/// the second read is free. This asserts the memo actually holds: the second
/// `get_features` for the same layer must allocate a small constant, not another
/// 8,552-feature decode.
#[test]
fn a_second_read_of_a_layer_is_served_from_the_memo() {
    let tile = parse(GRONINGEN);
    // Warm it, off-budget.
    let first = layer_features(&tile, "buildings");
    assert!(first > 1000, "fixture should carry the building layer");

    let (second, cost) = Cost::measure(|| layer_features(&tile, "buildings"));
    assert_eq!(first, second, "the memo must return the same features");
    perf::within(cost, MEMO_REREAD_ALLOCS, "second features(buildings)");
}

/// Rasterising a whole tile must not allocate per pixel.
///
/// Measured at both raster sizes on purpose — see [`RENDER_DENSE_ALLOCS`], whose
/// comment carries the finding. If these two ever diverge, something started
/// scaling with area that did not before.
#[test]
fn rasterising_a_dense_tile_costs_the_same_at_both_raster_sizes() {
    let tile = parse(GRONINGEN);
    warm(&tile);

    let (_, small) =
        Cost::measure(|| crate::vector_tiles::render_mvt_sized(&tile, SubTile::WHOLE, 256));
    let (_, large) =
        Cost::measure(|| crate::vector_tiles::render_mvt_sized(&tile, SubTile::WHOLE, 512));

    perf::within(small, RENDER_DENSE_ALLOCS, "render_mvt_sized 256²");
    perf::within(large, RENDER_DENSE_ALLOCS, "render_mvt_sized 512²");

    let spread = small.allocs.abs_diff(large.allocs);
    assert!(
        spread * 100 < small.allocs,
        "256² allocated {} and 512² allocated {} — a spread of {spread} (>1%) means \
         rasterisation started allocating with area. This cost is supposed to be \
         per-feature; if it is not, the phone tier's `tile_px = 256` is buying less \
         than its doc comment claims.",
        small.allocs,
        large.allocs,
    );
}

/// A ceiling on the documented quadratic-capacity defect in `mvt-reader`.
///
/// See [`DECODE_SPARSE_TRAFFIC_BYTES`]. This test is expected to become wildly
/// slack when the dependency is vendored or patched; tighten it then.
#[test]
fn decoding_a_sparse_tile_stays_under_the_known_capacity_ceiling() {
    let tile = parse(LOWZOOM);
    let (_, cost) = Cost::measure(|| warm(&tile));
    assert!(
        cost.bytes <= DECODE_SPARSE_TRAFFIC_BYTES,
        "decoding the z10 fixture's layers moved {} MiB of allocation traffic, \
         ceiling is {} MiB. This tile is 68 KiB on disk. See \
         docs/notes/mvt-capacity-guard.md — the cause is `mvt-reader` reserving \
         capacity from a varint length it has not validated against the remaining \
         buffer.",
        cost.bytes / (1024 * 1024),
        DECODE_SPARSE_TRAFFIC_BYTES / (1024 * 1024),
    );
}

/// Rasterising a sub-tile must not build paths for features that cannot land on it.
///
/// This is the counter-based half, and it guards the cull that
/// `docs/notes/tile-raster-cost.md` records as the fix for the 35 ms overzoom row.
/// An allocation budget cannot see this: the cull's saving is scan conversion and
/// path construction, not heap traffic. Counting the paths handed to tiny-skia
/// can, and the ratio is the assertion — a 1/64-of-the-ground sub-tile must submit
/// dramatically fewer paths than the whole tile, or the cull has silently stopped
/// working.
#[test]
fn an_overzoomed_sub_tile_submits_far_fewer_paths_than_the_whole_tile() {
    let tile = parse(GRONINGEN);
    warm(&tile);

    let (_, whole_paths, _) =
        perf::paths::measure(|| crate::vector_tiles::render_mvt_sized(&tile, SubTile::WHOLE, 256));

    let (_, sub_paths, _) =
        perf::paths::measure(|| crate::vector_tiles::render_mvt_sized(&tile, SUB_1_64, 256));

    assert!(
        whole_paths > 0 && sub_paths > 0,
        "both renders must submit some geometry (whole={whole_paths}, sub={sub_paths})"
    );
    assert!(
        sub_paths * 4 < whole_paths,
        "a 1/64-ground sub-tile submitted {sub_paths} paths against the whole tile's \
         {whole_paths} — less than a 4× reduction means the overzoom cull \
         (`render_mvt_culled`) is not culling. See docs/notes/tile-raster-cost.md."
    );
}

// ---------------------------------------------------------------------------
// The measured numbers
// ---------------------------------------------------------------------------
//
// Read off `print_tile_pipeline_cost` and rounded up ~10%. Raise them in the same
// commit as the change that earns it, and say why in the body.

// Headroom is ~2%, not the ~10% this reached for first, and the difference is the
// whole usefulness of the file. These counts were verified **bit-identical over
// three consecutive runs** — they are a property of the input and the code, not of
// the machine — so the only thing headroom needs to absorb is an unrelated commit
// nudging a growth threshold, which is a handful of allocations, not thousands.
//
// The 10% version was tested against a deliberately injected regression (a
// surviving `Vec` allocation per filled path, the exact shape of the defects
// `docs/notes/wasm-performance.md` records) and **let it through**: the probe cost
// 8,688 allocations, 7.3%, comfortably inside a 10% ceiling. A budget loose enough
// never to complain is a budget that never catches anything.

/// `TileData::parse`, Groningen z14. Measured 54,364.
const PARSE_DENSE_ALLOCS: u64 = 55_500;

/// `TileData::parse`, z10 lowzoom. Measured 2,360.
const PARSE_SPARSE_ALLOCS: u64 = 2_410;

/// A second `features()` for a layer already decoded. Measured **0**, and asserted
/// as exactly 0 rather than with headroom — the `OnceCell` either returns the
/// memo or it does not, and "a few allocations" would mean it had stopped.
const MEMO_REREAD_ALLOCS: u64 = 0;

/// Rasterising the whole dense tile. Measured 118,905 at 256² and 118,906 at 512².
///
/// **The two sizes agreeing to within one allocation is the fact worth keeping**:
/// it says this cost is per *feature*, not per pixel — path construction, not scan
/// conversion. So halving `tile_px` for the phone tier (`quality::Budget::tile_px`)
/// buys a quarter of the scan conversion and **none** of this. That is the
/// measurement behind "per-feature overhead is roughly double the share of
/// `render_mvt` on a phone that it is on a desktop".
const RENDER_DENSE_ALLOCS: u64 = 121_300;

/// Decoding every layer of the *sparse* z10 tile, in bytes of allocation traffic.
///
/// 226 MiB — from a **68 KiB** tile, i.e. ~3,300× its own size, and 9× the traffic
/// of the 495 KiB dense tile that has 25× more features. That inversion is the
/// signature of `mvt-reader`'s quadratic `Vec::with_capacity`, which
/// `docs/notes/mvt-capacity-guard.md` documents as an open abort risk and which
/// nothing had ever measured.
///
/// Pinned as a **ceiling on a known defect**, not as an endorsement: it stops the
/// bug getting worse while it waits for the vendoring the note calls for, and it
/// is the test that will go green by a factor of hundreds when that lands. Lower
/// it hard at that point — a budget left at the broken value would let the fix
/// silently regress.
const DECODE_SPARSE_TRAFFIC_BYTES: u64 = 232 * 1024 * 1024; // measured 226 MiB

// ---------------------------------------------------------------------------
// The measuring hatch
// ---------------------------------------------------------------------------

/// Print the cost of every stage, as a table. `#[ignore]`, like every other
/// printing hatch in this crate (`bench_overzoom_cull`, `dump_png`,
/// `dump_layers`) — it is how the budgets above get their numbers, not a gate.
///
/// ```text
/// cargo test -p cartopolis_geo print_tile_pipeline_cost -- --ignored --nocapture
/// ```
#[test]
#[ignore = "printing hatch: it reports numbers, it does not assert them"]
fn print_tile_pipeline_cost() {
    println!("\n== allocation cost, one tile ==");

    for (label, bytes) in [("groningen z14", GRONINGEN), ("lowzoom z10", LOWZOOM)] {
        let (tile, cost) = Cost::measure(|| parse(bytes));
        println!("{}", cost.line(&format!("parse ({label})")));

        // The lazy per-layer decode, measured on its own rather than hidden inside
        // whichever render happens to run first.
        let (_, decode) = Cost::measure(|| warm(&tile));
        println!("{}", decode.line(&format!("  decode all layers ({label})")));

        let (_, reread) = Cost::measure(|| layer_features(&tile, "buildings"));
        println!("{}", reread.line(&format!("  re-read buildings ({label})")));

        for px in [256_u32, 512] {
            let (_, cost) =
                Cost::measure(|| crate::vector_tiles::render_mvt_sized(&tile, SubTile::WHOLE, px));
            println!("{}", cost.line(&format!("  render {px}² ({label})")));
        }

        let (_, cost) =
            Cost::measure(|| crate::vector_tiles::render_mvt_sized(&tile, SUB_1_64, 256));
        println!(
            "{}",
            cost.line(&format!("  render 256² 1/64 sub ({label})"))
        );
    }

    println!("\n== rasteriser input (paths / points) ==");
    let tile = parse(GRONINGEN);
    warm(&tile);
    for (label, sub) in [
        ("whole tile", SubTile::WHOLE),
        ("sub 1/64 (f8, 3,3)", SUB_1_64),
    ] {
        let (_, paths, points) =
            perf::paths::measure(|| crate::vector_tiles::render_mvt_sized(&tile, sub, 256));
        println!("{label:<34} {paths:>9} paths  {points:>9} points");
    }
    println!();
}
