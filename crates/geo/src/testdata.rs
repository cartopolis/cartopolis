//! The recorded tiles every geodata test measures against, embedded once.
//!
//! Eleven test modules used to carry their own `include_bytes!` of
//! `groningen_z14.mvt`, and `vector_tiles` and `perf_budgets` carried their own
//! of each low-zoom tile. `include_bytes!` does not deduplicate — two macros
//! naming one path produce two statics — so the `cartopolis_geo` test binary
//! embedded **twelve** copies of the 495 KB Groningen body and three of the
//! low-zoom pair: 6.1 MB of a 21.4 MB binary, all of it the same bytes, linked
//! and written out on every build of the suite.
//!
//! One module owns each file now. The duplicated `fixture()` parse helper comes
//! with it: every copy was `TileData::parse(..).expect(..)`, and a fixture that
//! fails to parse should say which file it was in one place.
//!
//! **`static`, not `const`, and that is the half that does the work.** A `const`
//! is substituted into each use site, so every mention of one re-materialises the
//! whole array and — across 256 codegen units — lands as another copy in the
//! binary. Collapsing eleven `const`s into one still left five copies of
//! Groningen for exactly that reason. A `static` is one address that every use
//! loads from, which is what takes it to one.
//!
//! `#[cfg(test)]`, so none of this reaches a shipped build — the same footing as
//! [`crate::perf_budgets`].

use crate::vector_tiles::TileData;

/// A real z14 tile over Groningen from the VersaTiles Shortbread source (now
/// self-hosted at tiles.cartopolis.org; the archive is identical): 495 KB,
/// 8,552 buildings and 95 streets. Every measurement in
/// `docs/notes/tile-raster-cost.md` was taken on it.
pub static GRONINGEN_Z14: &[u8] = include_bytes!("../tests/fixtures/groningen_z14.mvt");

/// [`GRONINGEN_Z14`]'s own tile coordinates, which the routing tests need in
/// order to place its integer lattice on the globe.
pub const GRONINGEN_TILE: (i64, i64) = (8490, 5319);

/// The z14 tile **east** of [`GRONINGEN_Z14`], which is the one with the road
/// tunnels in it: Groningen's southern ring at DUO.
///
/// `GRONINGEN_Z14` is the station tile and its only tunnel-flagged ways are
/// footpaths and arcades — four bores, 2.2 m wide, 3.6 m down. So every claim the
/// tunnel work made about *road* tunnels was measured on a live fetch nobody
/// could re-run, and the checked-in suite could not tell a carriageway that
/// descends from one that steps seven metres at the portal. This tile carries
/// `Weg der Verenigde Naties` as the thing it actually is: six lid runs of 172 m
/// to 242 m, in opposite-heading pairs, with the open cuttings between them.
pub static GRONINGEN_RING_Z14: &[u8] = include_bytes!("../tests/fixtures/groningen_ring_z14.mvt");

/// [`GRONINGEN_RING_Z14`]'s own tile coordinates.
pub const GRONINGEN_RING_TILE: (i64, i64) = (8491, 5321);

/// [`GRONINGEN_RING_Z14`], parsed. A fresh `TileData` per call, as
/// [`groningen`] is and for the same reason.
pub fn groningen_ring() -> TileData {
    TileData::parse(GRONINGEN_RING_Z14).expect("the groningen_ring_z14 fixture parses")
}

/// A sparser z10 tile, so a regression that only shows on dense data is
/// distinguishable from one that scales with the fixed overhead of a parse.
pub static LOWZOOM_526_334: &[u8] = include_bytes!("../tests/fixtures/lowzoom_10_526_334.mvt");

/// A second z10 tile. Both were added when a low-zoom body was found to decode
/// differently from a city one; they are kept as a pair so a guard has to pass
/// both.
pub static LOWZOOM_534_336: &[u8] = include_bytes!("../tests/fixtures/lowzoom_10_534_336.mvt");

/// The low-zoom pair with the names their failures should report.
pub static LOWZOOM_TILES: [(&str, &[u8]); 2] = [
    ("10/526/334", LOWZOOM_526_334),
    ("10/534/336", LOWZOOM_534_336),
];

/// [`GRONINGEN_Z14`], parsed.
///
/// A fresh `TileData` per call on purpose: the decode is lazily memoised per
/// layer, so a shared one would let whichever test ran first pay for the others'
/// decodes and make any allocation measurement depend on test order.
pub fn groningen() -> TileData {
    TileData::parse(GRONINGEN_Z14).expect("the groningen_z14 fixture parses")
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    /// The fixture files this module owns. Adding one means adding it here too —
    /// otherwise its copies are unguarded, which is the state the whole module
    /// exists to leave behind.
    const FIXTURE_FILES: [&str; 4] = [
        "groningen_z14.mvt",
        "groningen_ring_z14.mvt",
        "lowzoom_10_526_334.mvt",
        "lowzoom_10_534_336.mvt",
    ];

    /// Every `.rs` file under `crates/geo/src`, the way [`crate::vector_tiles`]'
    /// sibling scans in `cartopolis::systems::theme` walk their own crate.
    fn source_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                source_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    fn sources() -> Vec<(PathBuf, String)> {
        let mut files = Vec::new();
        source_files(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("src").as_path(),
            &mut files,
        );
        assert!(
            files.len() > 10,
            "the scan found {} source files, so it is not looking where it thinks",
            files.len()
        );
        files
            .into_iter()
            .map(|p| {
                let body = std::fs::read_to_string(&p).expect("a source file reads");
                (p, body)
            })
            .collect()
    }

    /// **Each fixture is embedded exactly once, and this module is where.**
    ///
    /// `include_bytes!` does not deduplicate: eleven modules naming
    /// `groningen_z14.mvt` put eleven copies of 495 KB in the test binary, and
    /// nothing said so — a binary that is 6 MB larger than it needs to be looks
    /// exactly like one that is not. The copies accumulated one test module at a
    /// time over months, each one reasonable on its own, which is why this is a
    /// test and not a convention.
    #[test]
    fn every_fixture_is_embedded_exactly_once() {
        let here = Path::new(file!()).file_name().unwrap().to_owned();
        let sources = sources();
        for fixture in FIXTURE_FILES {
            let sites: Vec<_> = sources
                .iter()
                .filter(|(_, body)| body.contains("include_bytes!") && body.contains(fixture))
                .map(|(p, _)| p.clone())
                .collect();
            assert_eq!(
                sites.len(),
                1,
                "{fixture} is embedded by {} files; it belongs to {} alone: {:?}",
                sites.len(),
                here.to_string_lossy(),
                sites
            );
            assert_eq!(
                sites[0].file_name().unwrap(),
                here,
                "{fixture} is embedded by {:?}, not by this module",
                sites[0]
            );
        }
    }

    /// **The fixtures are `static`, not `const`, and the distinction is 3.5 MB.**
    ///
    /// A `const` is substituted into each use site, so the array is
    /// re-materialised per mention and — across this profile's 256 codegen units
    /// — lands as several copies anyway. Collapsing the eleven `include_bytes!`
    /// into one `const` still left five copies of Groningen in the binary; making
    /// it a `static`, which is one address every use loads from, took it to one.
    /// Nothing about the code reads differently either way, which is exactly why
    /// it needs pinning.
    #[test]
    fn the_fixture_bodies_are_statics() {
        let body = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("src")
                .join("testdata.rs"),
        )
        .expect("this module reads");
        // Anchored at `pub `, because this test reads its own source: an
        // unanchored search for the macro name would find the line doing the
        // searching.
        let defs: Vec<&str> = body
            .lines()
            .map(str::trim_start)
            .filter(|l| l.starts_with("pub ") && l.contains("include_bytes!"))
            .collect();
        assert_eq!(
            defs.len(),
            FIXTURE_FILES.len(),
            "every fixture in FIXTURE_FILES needs a body here, and nothing else does: {defs:?}"
        );
        for line in defs {
            assert!(
                line.starts_with("pub static "),
                "a fixture body must be a `static`, or it is copied per use site: {}",
                line.trim()
            );
        }
    }
}
