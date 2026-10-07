//! Water coverage at raster resolution: the globe's ocean mask and the
//! shore/bank distance fields the local water shader reads.

use super::*;

/// Layers whose polygons are water. Shared by [`tessellate_surfaces`],
/// [`water_shore_field`] and [`ocean_mask`] so the meshes, the shore field and
/// the globe's ocean can never disagree about what counts as water — a mismatch
/// would put a shoreline in open water.
pub const WATER_LAYERS: [&str; 2] = ["ocean", "water_polygons"];

/// Rasterise just the water coverage of a tile: `TILE_PX²` bytes, 255 where the
/// surface is water and 0 where it is land, in the same frame (and under the same
/// overzoom [`SubTile`]) as [`render_mvt`].
///
/// This is the globe tier's half of the water story (`systems::globe_ocean`).
/// From orbit a wave is many orders of magnitude below a pixel, so nothing about
/// the local wave shader transfers — what makes an ocean read as an ocean from
/// space is that it is *glossy and dark* where the land is matte, and that needs
/// one bit per texel, not a height field.
///
/// Anti-aliased on purpose: the mask feeds a roughness blend, and a hard-edged
/// coastline would put a stair-stepped gloss boundary along every shore.
pub fn ocean_mask(tile: &TileData, sub: SubTile) -> Result<Vec<u8>, String> {
    ocean_mask_sized(tile, sub, TILE_PX)
}

/// [`ocean_mask`] at an arbitrary edge length. It rides in the alpha channel of
/// the imagery, so it must be rasterised at whatever size
/// [`render_mvt_sized`] was given — a mask of a different length is dropped by
/// the caller and the tile silently loses its ocean.
pub fn ocean_mask_sized(tile: &TileData, sub: SubTile, size: u32) -> Result<Vec<u8>, String> {
    let mut pixmap = Pixmap::new(size, size).ok_or_else(|| "pixmap alloc failed".to_string())?;
    let paint = Paint {
        anti_alias: true,
        shader: Shader::SolidColor(tiny_skia::Color::WHITE),
        ..Paint::default()
    };

    let f = sub.factor as f32;
    let px = size as f32;
    for meta in tile.layers() {
        if !WATER_LAYERS.contains(&meta.name.as_str()) {
            continue;
        }
        let scale = px * f / TileData::extent(meta);
        let transform = Transform::from_row(
            scale,
            0.0,
            0.0,
            scale,
            -(sub.x as f32) * px,
            -(sub.y as f32) * px,
        );
        for feature in tile.features(meta) {
            fill_geometry(&mut pixmap, &paint, &feature.geometry, &transform);
        }
    }

    Ok(pixmap.pixels().iter().map(|p| p.alpha()).collect())
}

/// Edge length of the shore-distance field. At a z14 tile's ~1.5 km this is
/// ~1.5 m per texel, which is what a 10 m city canal needs: at half that
/// resolution its entire width is three texels and the shoreline treatment reads
/// as a blocky stripe instead of a bank.
pub const SHORE_FIELD_PX: u32 = 1024;

/// Distance from shore at which the field saturates. Everything past this is
/// simply "open water", and 60 m is comfortably more than the width of the
/// canals and harbour basins the near-field treatment is for.
pub const SHORE_MAX_DIST_M: f32 = 60.0;

/// Apron of texels rasterised around the shore field so banks just across the
/// tile border are seen by the distance transform. Sized to the ground the MVT
/// buffer actually carries: Shortbread encodes a 64/4096-of-extent buffer, and
/// `1024 / 64 = 16` texels covers exactly that.
pub const SHORE_FIELD_PAD: u32 = SHORE_FIELD_PX / 64;

/// Edge length of the bank-obstruction field.
///
/// A quarter of [`SHORE_FIELD_PX`] — ~5.9 m per texel at z14 — because the thing
/// it measures is a building, and the smallest building in the tile is wider than
/// that. The shore field needs its 1.5 m to resolve a 10 m canal; this one is
/// read at a single point, through a linear sampler, by a term that then softens
/// its answer over [`SHORE_MAX_DIST_M`]-scale distances. At the shore field's
/// size it would be the largest image the client holds per water cell all over
/// again, for no more information.
pub const BANK_FIELD_PX: u32 = SHORE_FIELD_PX / 4;

/// The same apron as [`SHORE_FIELD_PAD`], at the bank field's resolution: a
/// building just over the tile line is exactly as much in the mirror as one just
/// inside it.
pub const BANK_FIELD_PAD: u32 = BANK_FIELD_PX / 64;

/// A tile's two distance fields, both in the same tile-local frame as
/// [`tessellate_surfaces`]' output (origin north-west, +x east, +y south) and
/// both quantised 0 at the seed to 255 at [`SHORE_MAX_DIST_M`] or beyond.
#[derive(Debug, Clone)]
pub struct ShoreField {
    /// Edge length of [`Self::data`].
    pub size: u32,
    /// Distance to the nearest **bank**: `size²` bytes, 0 where the water ends.
    pub data: Vec<u8>,
    /// Edge length of [`Self::bank`].
    pub bank_size: u32,
    /// Distance to the nearest **building**, at [`BANK_FIELD_PX`].
    ///
    /// This is the half of the bank-in-the-mirror term that used to be a guess.
    /// The shader asks what a reflected ray runs into on its way off the water,
    /// and the shore field can only answer "land" — so the term assumed a
    /// storey or two of quay stood on every bank, which over a city canal is
    /// true and over a lake with a mown bank is not. A reflection darkened by a
    /// building that is not there is not a reflection; it is a rim of shadow
    /// around every shore, and from a few hundred metres up that is exactly how
    /// it read.
    ///
    /// Buildings only, and deliberately not the `land` layer's wood and park
    /// polygons: those are canopy *areas* whose edge is where the trees start,
    /// a treeline dims a reflection rather than stopping it, and seeding the
    /// transform with them would put most of the rim straight back. All 255 for
    /// a tile with no buildings in it at all, which is the honest answer and
    /// also the one that switches the term off.
    pub bank: Vec<u8>,
}

/// Rasterise the tile's water polygons and turn them into a distance-to-shore
/// field — plus a coarser distance-to-building one — or `None` when the tile
/// holds no water at all.
///
/// This is the piece of information the geometry alone cannot give the water
/// shader. Earcut emits nothing but the polygon's own boundary vertices, so
/// every triangle corner sits *on* the shore and a per-vertex distance would be
/// identically zero; the answer has to come from the interior of the shape,
/// which means rasterising it. With it, the shader can shelter a canal from the
/// swell, colour the shallows and lap a foam line against the bank — the three
/// things that separate "a blue surface with waves on it" from water in a place.
///
/// Distances are measured against land inside this tile **and its MVT buffer**.
/// For the interior that is simply the truth: in MVT, land is the absence of a
/// water polygon. Near the border it used to overestimate badly — the raster
/// stopped at the tile edge, so a bank two metres into the neighbouring tile
/// was invisible, the border strip saturated to "open water", and the shallow
/// tint (the most visible thing the field drives) stepped in a straight line
/// down every z14 cell edge. The tile's geometry already runs past the edge
/// into the buffer zone (that is what `clip_ring` exists to cut off), so
/// rasterising a [`SHORE_FIELD_PAD`]-texel apron around the field and cropping
/// it away after the transform makes the border strip truthful out to the
/// buffer's reach (~23 m at z14 — past `SHALLOW_M` and the foam band) for one
/// extra fill and a 6 % larger transform. Note the apron pads with the
/// *neighbour's actual geometry*, not with land: padding with land (the
/// obvious trick) would draw a shoreline straight down the middle of every
/// river that crosses a tile edge.
pub fn water_shore_field(tile: &TileData, tile_m: f32) -> Result<Option<ShoreField>, String> {
    let size = SHORE_FIELD_PX;
    let pad = SHORE_FIELD_PAD;
    let psize = size + 2 * pad;
    let mut pixmap = Pixmap::new(psize, psize).ok_or_else(|| "pixmap alloc failed".to_string())?;
    let paint = Paint {
        anti_alias: false,
        shader: Shader::SolidColor(tiny_skia::Color::WHITE),
        ..Paint::default()
    };

    let mut any_water = false;
    for meta in tile.layers() {
        if !WATER_LAYERS.contains(&meta.name.as_str()) {
            continue;
        }
        let extent = TileData::extent(meta);
        let s = size as f32 / extent;
        let transform = Transform::from_row(s, 0.0, 0.0, s, pad as f32, pad as f32);
        for feature in tile.features(meta) {
            // The fill itself answers "was there any water here", so the ring is
            // walked into a `PathBuilder` once instead of twice. This runs at
            // `SHORE_FIELD_PX + 2·PAD` (1056²) over every ocean/water polygon, and
            // on a low-zoom ocean tile the water polygon *is* the feature set.
            any_water |= fill_geometry(&mut pixmap, &paint, &feature.geometry, &transform);
        }
    }

    // **The narrow waterways too, stroked at the width they are drawn at.**
    //
    // `tessellate_surfaces` draws a river, canal or stream that Shortbread
    // carries as a *line* rather than an area, and this field is what every water
    // surface asks "how far am I from the bank?". A ribbon the field does not
    // know about samples as land along its whole length — zero distance from
    // shore, which is shallow water and foam everywhere. That is the mismatch
    // `WATER_LAYERS`' own doc warns of, in its other direction: not a shoreline in
    // open water, but open water that thinks it is all shoreline.
    if let Some(meta) = tile.layer("water_lines") {
        let extent = TileData::extent(meta);
        let s = size as f32 / extent;
        let transform = Transform::from_row(s, 0.0, 0.0, s, pad as f32, pad as f32);
        for feature in tile.features(meta) {
            let Some(half_m) = water_line_half_width_m(kind_of(feature).unwrap_or("")) else {
                continue;
            };
            // Field texels per metre: the stroke has to be the ribbon's width in
            // the *field's* units, not the tile's.
            let width_px = 2.0 * half_m * (extent / tile_m.max(1.0)) * s;
            let stroke = Stroke {
                width: width_px.max(1.0),
                line_cap: LineCap::Butt,
                line_join: LineJoin::Round,
                ..Stroke::default()
            };
            let mut lines: Vec<&geo_types::LineString<f32>> = Vec::new();
            match &feature.geometry {
                Geometry::LineString(ls) => lines.push(ls),
                Geometry::MultiLineString(mls) => lines.extend(mls.iter()),
                _ => continue,
            }
            for ls in lines {
                let mut pb = PathBuilder::new();
                let mut started = false;
                for c in &ls.0 {
                    if !c.x.is_finite() || !c.y.is_finite() {
                        continue;
                    }
                    if started {
                        pb.line_to(c.x, c.y);
                    } else {
                        pb.move_to(c.x, c.y);
                        started = true;
                    }
                }
                if let Some(path) = pb.finish() {
                    pixmap.stroke_path(&path, &paint, &stroke, transform, None);
                    any_water = true;
                }
            }
        }
    }
    if !any_water {
        return Ok(None);
    }
    // Seed the transform from every *land* texel — the unpainted ones.
    let data = distance_field(&pixmap, size, pad, tile_m, Seed::Unpainted);

    // And a second, coarser field of what *stands* on the bank, for the
    // reflection term. Only buildings seed it; see `ShoreField::bank`.
    let bank_size = BANK_FIELD_PX;
    let bank_pad = BANK_FIELD_PAD;
    let bpsize = bank_size + 2 * bank_pad;
    let mut banks = Pixmap::new(bpsize, bpsize).ok_or_else(|| "pixmap alloc failed".to_string())?;
    for meta in tile.layers() {
        if meta.name != "buildings" {
            continue;
        }
        let s = bank_size as f32 / TileData::extent(meta);
        let transform = Transform::from_row(s, 0.0, 0.0, s, bank_pad as f32, bank_pad as f32);
        for feature in tile.features(meta) {
            fill_geometry(&mut banks, &paint, &feature.geometry, &transform);
        }
    }
    let bank = distance_field(&banks, bank_size, bank_pad, tile_m, Seed::Painted);

    Ok(Some(ShoreField {
        size,
        data,
        bank_size,
        bank,
    }))
}

/// Which texels of a rasterised mask the distance is measured *from*.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Seed {
    /// The painted ones — "how far to the nearest building".
    Painted,
    /// The unpainted ones — "how far to the nearest bit of land", where the
    /// paint is the water.
    Unpainted,
}

/// Distance transform one padded mask into a cropped, quantised field.
///
/// `pixmap` is `size + 2·pad` square; the result is `size` square, in metres
/// scaled so 255 is [`SHORE_MAX_DIST_M`]. The apron is cropped away *after* the
/// transform, which is the whole reason it is rasterised: a seed just over the
/// tile line is as real as one just inside it, and without it every field
/// saturates in a straight line down the cell edge.
fn distance_field(pixmap: &Pixmap, size: u32, pad: u32, tile_m: f32, seed: Seed) -> Vec<u8> {
    let psize = size + 2 * pad;
    // "Unreached" is a large *finite* distance, not infinity: the envelope
    // subtracts one sample's value from another's, and ∞ − ∞ is a NaN that
    // quietly poisons a whole scanline. Two texel-grids is past any real answer
    // (nothing inside the field can be further than `psize` from a seed) while
    // staying exactly representable in f32 — and it is also what a mask with no
    // seeds at all leaves behind, which quantises to 255 everywhere, i.e. "there
    // is none of this anywhere near", which is the right answer.
    let unreached = (2 * psize) as f32;
    let mut sq = vec![0.0f32; (psize * psize) as usize];
    for (d, px) in sq.iter_mut().zip(pixmap.pixels()) {
        let is_seed = (px.alpha() > 127) == (seed == Seed::Painted);
        *d = if is_seed { 0.0 } else { unreached };
    }
    squared_distance_transform(&mut sq, psize as usize);

    let m_per_texel = tile_m / size as f32;
    let mut out = Vec::with_capacity((size * size) as usize);
    for y in pad..pad + size {
        let row = &sq[(y * psize + pad) as usize..(y * psize + pad + size) as usize];
        out.extend(row.iter().map(|d2| {
            let m = d2.sqrt() * m_per_texel;
            (m / SHORE_MAX_DIST_M).clamp(0.0, 1.0).mul_add(255.0, 0.5) as u8
        }));
    }
    out
}

/// Exact squared Euclidean distance transform, in place over a `size × size`
/// grid holding `0` at each seed and a saturation distance of at least `size`
/// everywhere else. On return each cell holds the squared distance to the
/// nearest seed.
///
/// Exact and O(n) — the same cost as the chamfer approximation people reach for
/// first, without its error, which here would show up as a shoreline band whose
/// width visibly depends on the bank's angle.
///
/// Felzenszwalb & Huttenlocher do this as two passes of a 1-D parabola lower
/// envelope, one per axis. Only the second pass needs one. The first sees
/// nothing but the binary seed mask, and the 1-D squared distance transform of a
/// binary function is just "distance to the nearest seed, squared" — two min
/// sweeps, no parabolas, and no division (the envelope divides once per sample,
/// which at a megapixel is where its time actually goes).
///
/// Running that first axis *down the rows* rather than along them is what makes
/// it worth doing: the recurrence is along y, so for a fixed y every x is
/// independent and contiguous, and the sweep vectorises. The original walked
/// columns with a `size`-element stride, gathering one f32 at a time.
///
/// Together: ~3x off the whole transform, for bit-identical output. That matters
/// because this is the single most expensive thing a water tile does.
fn squared_distance_transform(grid: &mut [f32], size: usize) {
    if size < 2 {
        return;
    }

    // Pass 1, along y. Forward then backward min-sweep leaves each cell holding
    // its distance to the nearest seed in its own column; squaring it gives the
    // 1-D transform the envelope would have produced.
    for y in 1..size {
        let (head, tail) = grid.split_at_mut(y * size);
        let above = &head[(y - 1) * size..];
        for (d, a) in tail[..size].iter_mut().zip(above) {
            *d = d.min(a + 1.0);
        }
    }
    for y in (0..size - 1).rev() {
        let (head, tail) = grid.split_at_mut((y + 1) * size);
        for (d, b) in head[y * size..].iter_mut().zip(&tail[..size]) {
            *d = d.min(b + 1.0);
        }
    }
    for d in grid.iter_mut() {
        *d *= *d;
    }

    // Pass 2, along x. Its input is an arbitrary function now, not a mask, so
    // this one really is the parabola envelope — over contiguous rows.
    //
    // **Every row is independent here, and pass 1 was not.** The y-sweeps above
    // carry a running minimum from one row to the next, so they have to be
    // walked in order; the envelope reads one row and writes that same row, with
    // scratch that belongs to the row. So this pass — and only this pass — is
    // split across threads. The shore field is 1,056 rows of 1,056 samples and
    // the envelope is the expensive half of the transform.
    #[cfg(not(target_arch = "wasm32"))]
    {
        // Below this the whole transform is well under a millisecond and the
        // threads cost more than the sweep.
        const PARALLEL_MIN_SIZE: usize = 256;
        let threads = crate::road_stitch::available_threads().min(4);
        if threads > 1 && size >= PARALLEL_MIN_SIZE {
            let rows = size.div_ceil(threads);
            std::thread::scope(|s| {
                for band in grid.chunks_mut(rows * size) {
                    s.spawn(move || {
                        // Per thread: the envelope's vertices and breakpoints are
                        // rebuilt for each row anyway, so sharing them would buy
                        // nothing and cost a lock.
                        let mut f = vec![0.0f32; size];
                        let mut d = vec![0.0f32; size];
                        let mut v = vec![0usize; size];
                        let mut z = vec![0.0f32; size + 1];
                        for row in band.chunks_mut(size) {
                            f.copy_from_slice(row);
                            lower_envelope(&f, &mut d, &mut v, &mut z);
                            row.copy_from_slice(&d);
                        }
                    });
                }
            });
            return;
        }
    }

    let mut f = vec![0.0f32; size];
    let mut d = vec![0.0f32; size];
    // Parabola vertices and the breakpoints between them; both need one extra slot.
    let mut v = vec![0usize; size];
    let mut z = vec![0.0f32; size + 1];
    for y in 0..size {
        f.copy_from_slice(&grid[y * size..(y + 1) * size]);
        lower_envelope(&f, &mut d, &mut v, &mut z);
        grid[y * size..(y + 1) * size].copy_from_slice(&d);
    }
}

/// One-dimensional squared distance transform: the lower envelope of the
/// parabolas rooted at each sample.
fn lower_envelope(f: &[f32], d: &mut [f32], v: &mut [usize], z: &mut [f32]) {
    let n = f.len();
    let mut k: usize = 0;
    v[0] = 0;
    z[0] = f32::NEG_INFINITY;
    z[1] = f32::INFINITY;
    for q in 1..n {
        loop {
            let p = v[k];
            let s = ((f[q] + (q * q) as f32) - (f[p] + (p * p) as f32))
                / (2.0 * q as f32 - 2.0 * p as f32);
            if s <= z[k] && k > 0 {
                k -= 1;
            } else {
                k += 1;
                v[k] = q;
                z[k] = s;
                z[k + 1] = f32::INFINITY;
                break;
            }
        }
    }
    k = 0;
    // `q` walks the samples while `k` walks the envelope's segments — the two
    // advance independently, so this is not an iteration over `d`.
    #[allow(clippy::needless_range_loop)]
    for q in 0..n {
        while z[k + 1] < q as f32 {
            k += 1;
        }
        let p = v[k];
        let dq = q as f32 - p as f32;
        d[q] = dq * dq + f[p];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector_tiles::bytes::water_shore_field;

    // A real z14 tile over Groningen, from the VersaTiles Shortbread source
    // (now self-hosted at tiles.cartopolis.org; the archive is identical).
    use crate::testdata::GRONINGEN_Z14;

    /// The ocean mask rides in the imagery's alpha, so it has to come out at the
    /// size the imagery did. A mismatch is not loud: `GlobeTilePayload::decode`
    /// drops a wrong-length mask and the tile silently loses its ocean.
    #[test]
    fn the_ocean_mask_follows_the_raster_size() {
        let tile = TileData::parse(GRONINGEN_Z14).expect("parse");
        for size in [TILE_PX, TILE_PX / 2, 128] {
            let mask = ocean_mask_sized(&tile, SubTile::WHOLE, size).expect("mask");
            assert_eq!(mask.len() as u32, size * size, "mask size {size} is wrong");
        }
    }

    #[test]
    fn shore_field_measures_distance_into_the_water() {
        let tile_m = 1500.0;
        let field = water_shore_field(GRONINGEN_Z14, tile_m)
            .expect("shore field")
            .expect("fixture has water");
        assert_eq!(field.data.len(), (field.size * field.size) as usize);

        // A canal city: some texels are dry land (0) and some sit far enough from
        // any bank to read as open water. A field that is all one value means the
        // rasterisation or the transform silently did nothing.
        assert!(field.data.iter().any(|&d| d == 0), "no land texels");
        assert!(
            field.data.iter().any(|&d| d > 8),
            "no texel more than a few metres from shore"
        );
    }

    /// The bank field answers a different question from the shore field, and the
    /// way it fails is by quietly answering the same one.
    ///
    /// Seed it from the water polygons by mistake — or from nothing, and let the
    /// unreached value leak through as a small number — and the reflection term
    /// darkens either every shore or none of them, which is precisely the bug it
    /// exists to end. So: buildings and land must disagree somewhere in a canal
    /// city, and a stretch of open water must be able to read "nothing standing
    /// anywhere near".
    #[test]
    fn the_bank_field_measures_buildings_and_not_the_shore() {
        let tile_m = 1500.0;
        let field = water_shore_field(GRONINGEN_Z14, tile_m)
            .expect("shore field")
            .expect("fixture has water");
        assert_eq!(
            field.bank.len(),
            (field.bank_size * field.bank_size) as usize
        );
        assert!(field.bank.iter().any(|&d| d == 0), "no building texels");
        assert!(
            field.bank.iter().any(|&d| d == u8::MAX),
            "every texel is within {SHORE_MAX_DIST_M} m of a building"
        );

        // And it must not be the shore field wearing a smaller size. Seed the
        // transform from the water polygons by mistake and every assert above
        // still holds — the giveaway is that the zeros would then coincide, so
        // look for **land that is not a building**: a street, a garden, a quay.
        // The first version of this test asked for open water instead, meaning a
        // texel 60 m from any bank, and a canal city does not have one.
        let step = field.size / field.bank_size;
        let mut land_without_a_building = 0usize;
        for by in 0..field.bank_size {
            for bx in 0..field.bank_size {
                let shore = field.data[((by * step) * field.size + bx * step) as usize];
                let bank = field.bank[(by * field.bank_size + bx) as usize];
                if shore == 0 && bank > 0 {
                    land_without_a_building += 1;
                }
            }
        }
        assert!(
            land_without_a_building > 0,
            "every land texel is also a building texel, so the bank field is \
             carrying the shore field's answer"
        );
    }

    #[test]
    fn shore_field_is_none_without_water() {
        // An empty MVT body parses to zero layers, so there is nothing to measure
        // against — the caller must get `None` rather than an all-shore field,
        // which would wrap the whole tile in foam.
        assert!(matches!(water_shore_field(&[], 1500.0), Ok(None)));
    }

    #[test]
    fn distance_transform_is_exact_for_a_single_seed() {
        // 1D-separable transforms are easy to get subtly wrong (off-by-one in the
        // breakpoints shows up as a distance field that leans one way), so check
        // against the closed form rather than against a golden image.
        let size = 16;
        let far = (4 * size * size) as f32;
        let mut grid = vec![far; size * size];
        grid[5 * size + 6] = 0.0;
        squared_distance_transform(&mut grid, size);
        for y in 0..size {
            for x in 0..size {
                let (dx, dy) = ((x as f32) - 6.0, (y as f32) - 5.0);
                assert!(
                    (grid[y * size + x] - (dx * dx + dy * dy)).abs() < 1e-3,
                    "wrong distance at ({x}, {y})"
                );
            }
        }
    }

    /// Even–odd point-in-polygon over a set of closed rings.
    /// The distance transform is the shore field's whole cost, so it was
    /// rewritten (binary min-sweep down the columns, envelope only along the
    /// rows). It is supposed to be *exact*, so check it against the definition
    /// itself — the O(n^4) brute force — rather than against the old code.
    #[test]
    fn distance_transform_is_exact() {
        for (size, seed_every) in [(16usize, 7usize), (32, 23), (17, 5), (2, 3)] {
            let unreached = (2 * size) as f32;
            let is_seed = |i: usize| i % seed_every == 0;
            let mut grid: Vec<f32> = (0..size * size)
                .map(|i| if is_seed(i) { 0.0 } else { unreached })
                .collect();
            squared_distance_transform(&mut grid, size);

            for y in 0..size {
                for x in 0..size {
                    let want = (0..size)
                        .flat_map(|sy| (0..size).map(move |sx| (sx, sy)))
                        .filter(|(sx, sy)| is_seed(sy * size + sx))
                        .map(|(sx, sy)| {
                            let (dx, dy) = (x as f32 - sx as f32, y as f32 - sy as f32);
                            dx * dx + dy * dy
                        })
                        .fold(f32::INFINITY, f32::min);
                    assert_eq!(
                        grid[y * size + x],
                        want,
                        "size {size}, seed every {seed_every}, at ({x}, {y})"
                    );
                }
            }
        }
    }

    /// A tile that is water all the way across has no land to measure against.
    /// The saturation value has to survive the transform intact, because the
    /// caller turns it into "open water, far from any bank" — an infinity here
    /// would come back as a NaN and take a whole scanline with it.
    #[test]
    fn a_seedless_grid_saturates_rather_than_going_infinite() {
        let size = 8;
        let unreached = (2 * size) as f32;
        let mut grid = vec![unreached; size * size];
        squared_distance_transform(&mut grid, size);
        assert!(grid.iter().all(|d| *d == unreached * unreached), "{grid:?}");
    }
}
