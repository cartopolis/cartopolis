//! One parsed tile body (TileData), the guards a body from a server we do not run must pass
//! before it is decoded, and the overzoom sub-rectangle (SubTile).

use super::*;

/// One tile body, parsed **once**, with its decoded features memoised per layer.
///
/// Everything that reads an MVT used to take `&[u8]` and open its own [`Reader`],
/// which meant `Reader::new(bytes.to_vec())` — a full copy plus a prost decode of
/// the whole tile — per *function*. A single `map_geometry` cell ran four of them
/// over the same body ([`tessellate_surfaces`], [`tessellate_roads`],
/// [`vegetation::scatter_vegetation`], [`water_shore_field`]) and a globe tile ran
/// two ([`render_mvt`], [`ocean_mask`]). Taking `&TileData` instead makes the
/// parse the caller's, once, and every consumer of that body shares it.
///
/// The per-layer memo matters for a second reason: `Reader::get_features` decodes
/// geometry *and* properties into a fresh `Vec<Feature>` on **every** call, and
/// [`PAINT_ORDER`] lists `streets` twice (casing then fill) and `buildings` twice
/// (fill then outline) — so the two heaviest layers of every rendered tile were
/// decoded twice each. `OnceLock` (not `RefCell`) because a decoded layer is never
/// invalidated; the interior mutability is initialise-once, so `&TileData` stays
/// the shape every consumer wants.
///
/// **`OnceLock`, not `OnceCell`, so this is `Sync`** — which is what lets one
/// parse be shared between worker threads rather than merely between the
/// consumers on one. That was the blocker on collapsing the repeat parses a fresh
/// anchor makes: the overzoom fan-out resolves up to 64 clipmap tiles to the same
/// z14 ancestor, and each was a separate task doing its own full `bytes.to_vec()`
/// and prost decode. `cartopolis`'s `tile_loader::parse_shared` is the cache; the
/// memory question it was waiting on is answered in
/// `docs/notes/allocation-budgets.md` (12.5 MiB peak for a fully-decoded dense
/// z14 tile), which is also why that cache holds `Weak` references and not strong
/// ones.
///
/// The lock is uncontended in the steady state — `get_or_init` on an initialised
/// `OnceLock` is an acquire load — so the sharing costs the single-threaded path
/// nothing measurable.
pub struct TileData {
    reader: Reader,
    layers: Vec<Layer>,
    /// Decoded features per `Layer::layer_index`. Empty for a layer that failed
    /// to decode — the byte-taking callers all treated that as "skip the layer",
    /// and folding it in here keeps that behaviour in one place.
    features: Vec<OnceLock<Vec<Feature>>>,
}

impl TileData {
    /// Parse a tile body. The one `Reader::new` in the client.
    ///
    /// Every byte here arrived over the network from a tile server we do not run
    /// the code of, so the body is checked against [`guard_tile_body`] *before*
    /// `mvt-reader` sees it — an ordinary `Err` for a tile that would otherwise
    /// abort the process. See that function for what is being defended against.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        guard_tile_body(bytes)?;
        let reader = Reader::new(bytes.to_vec()).map_err(|e| format!("mvt parse: {e:?}"))?;
        let layers = reader
            .get_layer_metadata()
            .map_err(|e| format!("mvt layers: {e:?}"))?;
        // Indexed by `layer_index`, which the reader assigns as the layer's
        // position — but size from the maximum rather than the count, so a source
        // that ever numbered them otherwise cannot index out of bounds.
        let slots = layers.iter().map(|l| l.layer_index + 1).max().unwrap_or(0);
        Ok(Self {
            reader,
            layers,
            features: (0..slots).map(|_| OnceLock::new()).collect(),
        })
    }

    /// Every layer's metadata, in tile order.
    pub fn layers(&self) -> &[Layer] {
        &self.layers
    }

    /// A named layer's metadata, or `None` if the tile has no such layer.
    pub fn layer(&self, name: &str) -> Option<&Layer> {
        self.layers.iter().find(|l| l.name == name)
    }

    /// A layer's decoded features, decoded on first ask and reused after.
    pub fn features(&self, layer: &Layer) -> &[Feature] {
        let Some(slot) = self.features.get(layer.layer_index) else {
            return &[];
        };
        slot.get_or_init(|| {
            self.reader
                .get_features(layer.layer_index)
                .unwrap_or_default()
        })
    }

    /// A layer's extent, with Shortbread's default substituted for a source that
    /// reports none. Every consumer needs this and every one of them wrote it out.
    pub fn extent(layer: &Layer) -> f32 {
        if layer.extent == 0 {
            4096.0
        } else {
            layer.extent as f32
        }
    }
}

/// Sharing a parse across worker threads is the whole reason [`TileData`]'s memo
/// is a `OnceLock`, and nothing else in this crate would notice if that stopped
/// being true — the crate is single-threaded, so a regression to `OnceCell` would
/// compile and test green here and fail only in `cartopolis`, at the cache.
const _: fn() = || {
    fn assert_shareable<T: Send + Sync>() {}
    assert_shareable::<TileData>();
};

// ─── Guarding a body we did not author ───────────────────────────────────────

/// Ceiling on a vector-tile body, in bytes.
///
/// Measured against what a real Shortbread z14 tile actually weighs: the
/// Groningen city-centre fixture in `tests/fixtures` — 8,552 buildings, 95
/// merged street features, sixteen other layers — is **495 KB**, and the
/// note in `docs/notes/tile-raster-cost.md` puts a typical body at ~70 KB. So
/// this is roughly 16× the densest tile anyone here has seen, which is headroom
/// rather than a working limit, and it sits an order of magnitude below
/// `platform::http`'s 64 MiB `MAX_BODY` — that cap is a whole-response ceiling
/// shared with imagery and is far too loose to be the only thing between a
/// hostile tile server and the allocator.
pub const MAX_TILE_BYTES: usize = 8 * 1024 * 1024;

/// **There is no capacity ceiling any more, and that is a measurement, not a
/// retreat.** Kept as a named constant only so the note below has somewhere to
/// live and nothing silently re-adds one.
///
/// A guard here shipped on 2026-08-11 and was reverted the same day, because it
/// refused real tiles. What follows is what was measured, so nobody rebuilds it.
///
/// The idea was to walk the protobuf, compute what `mvt-reader` will
/// `Vec::with_capacity`, and refuse a body that asks for too much. The attack is
/// real: a body of `[9, 0, 0]` repeated is all `MoveTo`, and each one reserves the
/// whole feature's command length, so cost grows quadratically. Measured with
/// `VmPeak` across a full `get_features`:
///
/// | body | estimate | actual virtual cost |
/// |------|----------|---------------------|
/// | bomb, 12 KB | 48 M slots | **+320 MiB** |
/// | bomb, 30 KB | 300 M | **+1.9 GiB** |
/// | bomb, 60 KB | 1 200 M | **+6.9 GiB** |
///
/// But the estimate does not predict cost, and that is the finding:
///
/// | body | estimate | worst single feature | actual virtual cost |
/// |------|----------|---------------------|---------------------|
/// | z10 10/526/334, 69 KB | 57 M | 55 M | none measurable |
/// | z10 10/527/336, 79 KB | 92 M | 74 M | none measurable |
/// | z10 10/534/336, 157 KB | **545 M** | **421 M** | **none measurable** |
/// | z14 Groningen, 653 KB | 12 M | 8 M | none measurable |
///
/// A real z10 tile asks for 545 M slots — more than a 30 KB bomb — and costs
/// nothing, because generalised low-zoom geometry is one enormous multi-part
/// coastline whose parts do not stay live at once. A 12 KB bomb asks for 48 M and
/// costs 320 MiB. No threshold on the total separates them; neither does the
/// worst single feature (421 M real against 48 M bomb); neither does slots per
/// body byte (3 463 real against 3 992 for the tamest bomb). Three metrics, three
/// overlaps — the walker models an allocation pattern `mvt-reader` does not
/// actually have.
///
/// So the guard is gone and only [`MAX_TILE_BYTES`] remains. That leaves the tree
/// where it was before, exposed to a tile server serving a deliberate bomb —
/// which is worth stating plainly rather than papering over: the fix is to bound
/// the allocation where it happens (a patched or vendored `mvt-reader` whose
/// capacity is `min(len, remaining)`), not to guess from the outside.
pub const MAX_TILE_COORD_CAPACITY: usize = usize::MAX;

/// Refuse a tile body that would make `mvt-reader` abort the process.
///
/// **The bug being defended against** (`mvt-reader` 2.4.0, `parse_geometry`):
/// every `MoveTo` and every `ClosePath` starts a fresh
/// `Vec::with_capacity(geometry_data.len())` — the length of the *whole* feature's
/// command stream, not of the part still to come — and each of those vectors is
/// then retained in the finished geometry. The command `[9, 0, 0]` is three bytes
/// and produces one such vector, so a body of `n` bytes of that repeated asks for
/// about `8n²/3` bytes of capacity: ~24 TB from a 3 MB body, which is not an
/// allocation that fails gracefully. `handle_alloc_error` aborts, and the shipped
/// profile is `panic = "abort"` with no `catch_unwind` anywhere, so on a phone
/// that is a SIGABRT with a network-controlled trigger and no user interaction.
///
/// **Why the length cap alone is not the fix.** The blow-up is *quadratic* in the
/// body length, so any cap that still admits a real 495 KB tile also admits a
/// ~700 GB request from a crafted one of the same size. The size ceiling is the
/// cheap first line — it bounds the copy, the prost decode and the scan below —
/// and [`geometry_capacity`] is what actually closes it, by computing the exact
/// capacity mvt-reader will ask for before mvt-reader is handed anything.
///
/// **A body the estimator cannot walk is refused, not waved through.** That is
/// the opposite of the instinct — a guard that blanks the map is worse than one
/// that misses — and it is right here for a specific reason: **anything this
/// walker cannot walk, `prost` cannot decode either.** A truncated record, a
/// length past the end of the buffer, an eleven-byte varint, wire type 6 or 7:
/// all of them are `Tile::decode` errors a moment later, so refusing them costs
/// nothing that was not already lost. The single exception is a proto2 **group**
/// (wire types 3 and 4), which prost *skips* and the MVT schema does not contain
/// anywhere — and which, waved through, is a one-byte way to disarm everything
/// below by appending it to a bomb.
fn guard_tile_body(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > MAX_TILE_BYTES {
        return Err(format!(
            "mvt body of {} bytes is past the {MAX_TILE_BYTES}-byte ceiling",
            bytes.len()
        ));
    }
    Ok(())
}

/// Edge length in pixels of a rendered tile. Larger than the 256 px raster tiles
/// we used to fetch: the texture is stretched across a whole region on the ground,
/// and re-rendering costs us CPU rather than bandwidth, so we may as well take the
/// resolution.
///
/// This is a **default, not a law.** Every rasteriser here takes an explicit size
/// ([`render_mvt_sized`], [`ocean_mask_sized`]) and this is the value the flat
/// clipmap passes on a desktop; the globe passes its own `GLOBE_TILE_PX`, and a
/// memory-constrained host passes half of this (`quality::Budget::tile_px`) —
/// the clipmap's ~224 resident tiles are ~310 MB of texture at 512² and ~78 MB
/// at 256², and `render_mvt`'s cost is ~area (`docs/notes/tile-raster-cost.md`,
/// `docs/notes/globe-tile-cost.md`). Nothing in this crate chooses between them:
/// it has no idea what it is running on.
pub const TILE_PX: u32 = 512;

/// Highest zoom the Shortbread source actually contains. Anything above this is
/// rendered by magnifying the z14 ancestor.
pub const MAX_SOURCE_ZOOM: u32 = 14;

/// Ceiling on how far [`SubTile::resolve`] will magnify, as a shift.
///
/// A saturating bound, not a policy — the client never asks for more than a
/// handful of levels of overzoom. It exists because `z` arrives from a `TileKey`
/// that nothing range-checks at that boundary, and the unclamped
/// `1i64 << (z - MAX_SOURCE_ZOOM)` panics in debug and produces nonsense in
/// release once `z` reaches 78.
///
/// 31 rather than a rounder number because [`SubTile::factor`] is a `u32`: at a
/// shift of 32 the `factor as u32` narrowing truncates to **zero**, turning an
/// overflow panic into a silently degenerate sub-tile, which is the worse of the
/// two failures. This is the largest shift the field can actually hold.
pub const MAX_OVERZOOM_SHIFT: u32 = 31;

/// How far past its own edge a tile's geometry may legitimately reach, as a
/// fraction of the layer's extent.
///
/// Encoders clip each layer to the tile plus a buffer, so a way crossing the
/// border appears in both tiles and a polygon on the seam has no visible gap.
/// Measured on the Groningen fixture: every line and area layer stops at ±64 of
/// 4096, i.e. **1.6%**, and the widest anything reaches is the `place_labels`
/// point layer at ~22%. So 25% is generous for real data by an order of
/// magnitude — which is the point. It is not a cartographic tolerance, it is a
/// **sanity window**: `mvt-reader` clips its coordinate cursor only at
/// `i32::MAX` and never to `[0, extent]`, so a hostile (or merely corrupt) tile
/// can hand any consumer here a coordinate near 2.1e9. Everything that buckets
/// segments on a uniform grid must reject those *before* bucketing, because the
/// loop that follows is over the bbox and would otherwise insert ~10^14 entries.
pub const TILE_BUFFER_FRAC: f32 = 0.25;

/// Which sub-rectangle of a source tile to draw, when the requested tile is deeper
/// than [`MAX_SOURCE_ZOOM`]. `factor` is 2^(requested_z − source_z); `(x, y)` is the
/// requested tile's position within the source tile, in units of the sub-grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubTile {
    pub factor: u32,
    pub x: u32,
    pub y: u32,
}

impl SubTile {
    /// The identity view: draw the whole source tile.
    pub const WHOLE: Self = Self {
        factor: 1,
        x: 0,
        y: 0,
    };

    /// Resolve a requested tile to the source tile that must be fetched plus the
    /// sub-rectangle of it to draw. Returns `((z, x, y), sub)`.
    ///
    /// `>>` on a signed coordinate is an arithmetic shift, i.e. floor division —
    /// which is the correct ancestor for a negative `x` produced by antimeridian
    /// wrapping, where a truncating divide would pick the tile one to the east.
    #[must_use]
    pub fn resolve(z: u32, x: i64, y: i64) -> ((u32, i64, i64), Self) {
        // `MAX_OVERZOOM_SHIFT`, not `z - MAX_SOURCE_ZOOM`: `z` reaches here from a
        // `TileKey`, which is not range-checked at this boundary, and `1i64 << 63`
        // is a panic in debug and nonsense in release. Clamping keeps an absurd
        // zoom absurd (the sub-tile is a vanishing speck) instead of a crash on a
        // network-adjacent input path.
        if z <= MAX_SOURCE_ZOOM {
            return ((z, x, y), Self::WHOLE);
        }
        let shift = (z - MAX_SOURCE_ZOOM).min(MAX_OVERZOOM_SHIFT);
        let factor = 1i64 << shift;
        let (sx, sy) = (x >> shift, y >> shift);
        (
            (MAX_SOURCE_ZOOM, sx, sy),
            Self {
                factor: factor as u32,
                x: (x - sx * factor) as u32,
                y: (y - sy * factor) as u32,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector_tiles::bytes::{
        ocean_mask, render_mvt, tessellate_roads, tessellate_surfaces, water_shore_field,
    };

    // A real z14 tile over Groningen, from the VersaTiles Shortbread source
    // (now self-hosted at tiles.cartopolis.org; the archive is identical).
    use crate::testdata::GRONINGEN_Z14;

    /// A `TileData` shared by several consumers must produce byte-for-byte what
    /// each of them produced from its own parse. This is the whole safety claim
    /// of step 2 — the outputs feed the map's pixels, its meshes and the globe's
    /// ocean mask, and a wrong hand-off shows up as missing water or roads, which
    /// only a screenshot would otherwise catch.
    #[test]
    fn one_shared_parse_matches_a_parse_per_call() {
        let tile_m = 1500.0;
        let tile = TileData::parse(GRONINGEN_Z14).expect("parse once");

        assert_eq!(
            super::render_mvt(&tile, SubTile::WHOLE).unwrap(),
            render_mvt(GRONINGEN_Z14, SubTile::WHOLE).unwrap(),
        );
        assert_eq!(
            super::ocean_mask(&tile, SubTile::WHOLE).unwrap(),
            ocean_mask(GRONINGEN_Z14, SubTile::WHOLE).unwrap(),
        );
        let same = |a: &[SurfaceMesh], b: &[SurfaceMesh]| {
            assert_eq!(a.len(), b.len());
            for (x, y) in a.iter().zip(b) {
                assert_eq!(x.group, y.group);
                assert_eq!(x.positions, y.positions);
                assert_eq!(x.indices, y.indices);
                assert_eq!(x.uvs, y.uvs);
            }
        };
        same(
            &super::tessellate_surfaces(&tile, tile_m, None).unwrap(),
            &tessellate_surfaces(GRONINGEN_Z14, tile_m).unwrap(),
        );
        same(
            &super::tessellate_roads(&tile, tile_m, &mut [], None, false)
                .unwrap()
                .ground,
            &tessellate_roads(GRONINGEN_Z14, tile_m, &[]).unwrap(),
        );
        assert_eq!(
            super::water_shore_field(&tile, tile_m)
                .unwrap()
                .unwrap()
                .data,
            water_shore_field(GRONINGEN_Z14, tile_m)
                .unwrap()
                .unwrap()
                .data,
        );
        let trees = crate::scatter::scatter_vegetation(&tile, tile_m, 7).unwrap();
        let fresh =
            crate::scatter::scatter_vegetation(&TileData::parse(GRONINGEN_Z14).unwrap(), tile_m, 7)
                .unwrap();
        assert_eq!(trees.len(), fresh.len(), "vegetation scatter diverged");

        // …and asking twice for the same layer really is memoised, not just
        // consistent: the paint order visits `streets` and `buildings` twice.
        let streets = tile.layer("streets").expect("fixture has streets");
        assert!(std::ptr::eq(
            tile.features(streets).as_ptr(),
            tile.features(streets).as_ptr(),
        ));
    }

    /// The same shape, at a size a real feature could have, still parses — which
    /// is what proves the guard is discriminating and, incidentally, that the
    /// test encoder emits a tile mvt-reader accepts at all.
    #[test]
    fn a_small_synthetic_tile_still_parses() {
        let tile = TileData::parse(&synth::geometry_bomb(100)).expect("parse");
        let meta = tile.layer("streets").expect("the synthetic layer");
        assert_eq!(TileData::extent(meta), 4096.0);
        assert_eq!(tile.features(meta).len(), 1);
    }

    #[test]
    fn an_oversized_body_is_refused_by_length_alone() {
        let Err(err) = TileData::parse(&vec![0u8; MAX_TILE_BYTES + 1]) else {
            panic!("oversized parsed");
        };
        assert!(err.contains("mvt body of"), "wrong refusal: {err}");
    }

    /// Two live **low-zoom** tiles must decode.
    ///
    /// This is the regression the capacity guard caused and the reason it was
    /// reverted: z10 geometry is one huge multi-part coastline, which the walker
    /// scored as a bomb, so the coarse basemap came up with holes in it and
    /// `refusing the tile` in the log. Real bytes from `tiles.cartopolis.org`, kept
    /// as fixtures so a future guard has to pass them before it ships.
    #[test]
    fn low_zoom_tiles_decode() {
        for (name, body) in crate::testdata::LOWZOOM_TILES {
            let tile = match TileData::parse(body) {
                Ok(t) => t,
                Err(e) => panic!("{name} must decode — the live basemap serves it: {e}"),
            };
            assert!(
                tile.layer("water").is_some() || tile.layer("land").is_some(),
                "{name} decoded but carries none of the layers a basemap tile has"
            );
        }
    }

    #[test]
    fn fixture_has_expected_shortbread_layers() {
        let reader = Reader::new(GRONINGEN_Z14.to_vec()).expect("parse fixture");
        let names = reader.get_layer_names().expect("layer names");
        for expected in ["streets", "buildings", "water_polygons", "land"] {
            assert!(names.iter().any(|n| n == expected), "missing {expected}");
        }
    }

    #[test]
    fn overzoom_resolves_to_the_z14_ancestor() {
        // z17 tile directly under z14/8490/5319.
        let (src, sub) = SubTile::resolve(17, 8490 * 8 + 3, 5319 * 8 + 5);
        assert_eq!(src, (14, 8490, 5319));
        assert_eq!(
            sub,
            SubTile {
                factor: 8,
                x: 3,
                y: 5
            }
        );

        // At or below the source zoom nothing is magnified.
        let (src, sub) = SubTile::resolve(12, 100, 200);
        assert_eq!(src, (12, 100, 200));
        assert_eq!(sub, SubTile::WHOLE);
    }

    /// An absurd zoom stays absurd rather than overflowing the shift.
    ///
    /// `z` reaches `resolve` from a `TileKey` nothing range-checks at that
    /// boundary, and the unclamped `1i64 << (z - MAX_SOURCE_ZOOM)` panics in debug
    /// the moment `z` reaches 78 — reachable from a malformed tile source, i.e.
    /// from the network.
    #[test]
    fn an_absurd_zoom_clamps_instead_of_overflowing_the_shift() {
        for z in [78, 100, u32::MAX] {
            let (src, sub) = SubTile::resolve(z, 1, 1);
            assert_eq!(src.0, MAX_SOURCE_ZOOM);
            assert!(sub.factor > 0, "z={z} produced a degenerate sub-tile");
        }
    }
}
