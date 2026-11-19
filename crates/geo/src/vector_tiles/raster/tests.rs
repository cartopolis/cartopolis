
use super::*;
use crate::vector_tiles::bytes::render_mvt;

// A real z14 tile over Groningen, from the VersaTiles Shortbread source
// (now self-hosted at tiles.cartopolis.org; the archive is identical).
use crate::testdata::GRONINGEN_Z14;

/// **The painted road stops square at a tunnel mouth.** The painter does not
/// draw tunnels, so a round cap on the road that runs into one is a half-disc
/// of road on the ground over the lid; once the ground over a lid was put back
/// at street level it showed as an orange semicircle past every portal of
/// Groningen's ring (2026-09-23). Measured four metres either side of the
/// eastbound portal: on the road it is road, on the lid it must not be.
#[test]
fn a_road_is_painted_square_at_a_tunnel_mouth() {
    let tile = crate::testdata::groningen_ring();
    let (_, y) = crate::testdata::GRONINGEN_RING_TILE;
    let n = 2f64.powi(14);
    let lat = (std::f64::consts::PI * (1.0 - 2.0 * y as f64 / n))
        .sinh()
        .atan();
    #[allow(clippy::cast_possible_truncation)]
    let tile_m = (40_075_016.686 * lat.cos() / n) as f32;
    // Painted as the finest clipmap level paints it, an eighth of the tile
    // at 512 px, where a road is drawn to scale and its cap is metres wide.
    // At the whole-tile scale a trunk road is two pixels and the cap is lost
    // in one of them, which is how this test first passed without the fix.
    let size = 512u32;
    let sub = SubTile {
        factor: 8,
        x: 5,
        y: 1,
    };
    let rgba = render_mvt_culled(&tile, sub, size, tile_m, false).expect("renders");
    let px = |p: [f32; 2]| -> [u8; 3] {
        let f = sub.factor as f32;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let (x, y) = (
            ((p[0] / tile_m * f - sub.x as f32) * size as f32) as usize,
            ((p[1] / tile_m * f - sub.y as f32) * size as f32) as usize,
        );
        let i = (y * size as usize + x) * 4;
        [rgba[i], rgba[i + 1], rgba[i + 2]]
    };
    // The eastbound portal and the way its lid runs, from
    // `tunnels::tests::dump_the_fixture_bores` on this tile.
    let portal = [959.0f32, 338.0];
    let inward = [878.0f32, 395.0];
    let (dx, dy) = (inward[0] - portal[0], inward[1] - portal[1]);
    let l = dx.hypot(dy);
    let at = |m: f32| [portal[0] + dx / l * m, portal[1] + dy / l * m];
    let (on_road, on_lid) = (px(at(-4.0)), px(at(4.0)));
    assert_ne!(
        on_road, on_lid,
        "the ground four metres over the lid is painted the colour of the road \
             that runs into the tunnel — a round cap past the mouth"
    );
}

#[test]
fn renders_a_non_blank_tile() {
    let rgba = render_mvt(GRONINGEN_Z14, SubTile::WHOLE).expect("render");
    assert_eq!(rgba.len() as u32, TILE_PX * TILE_PX * 4);

    // A city tile must not come out as flat background: count distinct colours.
    let mut seen = std::collections::HashSet::new();
    for px in rgba.chunks_exact(4) {
        seen.insert((px[0], px[1], px[2]));
        if seen.len() > 8 {
            break;
        }
    }
    assert!(
        seen.len() > 8,
        "tile rendered nearly flat ({} colours) — style or geometry is wrong",
        seen.len()
    );
    assert!(
        rgba.chunks_exact(4).all(|p| p[3] == 255),
        "alpha must be opaque"
    );
}

/// A street is painted at its real width at every magnification, not only at
/// the one where tile units and pixels happen to coincide.
///
/// The ratio test below cannot see this: it compares the raster against
/// itself at two sizes, and a width in the wrong units is wrong at both. The
/// widths went through tiny-skia as path units — 4096 to a tile — so a whole
/// z14 tile drew a 7 m street a third of a pixel wide and the basemap that
/// stands in for the road meshes above 500 m had no roads in it.
#[test]
fn a_street_is_painted_at_its_real_width_at_every_zoom() {
    let street = synth::tile(&[synth::tagged_line_layer(
        "streets",
        4096,
        ("kind", "residential"),
        &[(-100, 1800), (4196, 1800)],
    )]);
    let real_px =
        road_width_m("residential", StreetFlags::default()) * TILE_PX as f32 / NOMINAL_Z14_TILE_M;
    // The tarmac, told apart from the paper and from the paler kerb casing
    // around it by the red channel's midpoint between tarmac and casing.
    let tarmac = RoadClass::Minor.flat_rgb() >> 16;
    let casing = palette::ROAD_CASING >> 16;
    let dark = (tarmac + casing) / 2;
    for (factor, y) in [(1u32, 0u32), (4, 1), (8, 3)] {
        let sub = SubTile { factor, x: 0, y };
        let rgba = render_mvt(&street, sub).expect("render");
        let column = TILE_PX as usize / 2;
        let painted = (0..TILE_PX as usize)
            .filter(|row| u32::from(rgba[(row * TILE_PX as usize + column) * 4]) < dark)
            .count() as f32;
        let want = real_px * factor as f32;
        assert!(
            (painted - want).abs() <= 1.5,
            "at {factor}× the street is {painted} px of tarmac, its real width is {want:.1} px"
        );
    }
}

/// Halving the raster must not double every road.
///
/// Stroke widths are in **pixels**, while the texture is stretched over the
/// same ground whatever its size — so a smaller raster with unscaled widths
/// draws the same city with roads twice as wide, and the globe (which is the
/// only caller that asks for a different size) would come up looking like a
/// different map style from the flat tiles it hands off to.
///
/// Ink coverage is the measure that catches it: the fraction of the tile that
/// is not background is a scale-free property of the *design*, so it must
/// survive a change of raster size. It would climb by roughly the size ratio
/// if `wf` went back to being `f`.
#[test]
fn stroke_widths_scale_with_the_raster_size() {
    let tile = TileData::parse(GRONINGEN_Z14).expect("parse");
    let paper = (
        ((palette::PAPER >> 16) & 0xFF) as u8,
        ((palette::PAPER >> 8) & 0xFF) as u8,
        (palette::PAPER & 0xFF) as u8,
    );
    let inked = |size: u32| {
        let rgba =
            render_mvt_sized(&tile, SubTile::WHOLE, size, NOMINAL_Z14_TILE_M).expect("render");
        assert_eq!(rgba.len() as u32, size * size * 4, "wrong buffer size");
        let n = rgba
            .chunks_exact(4)
            .filter(|p| (p[0], p[1], p[2]) != paper)
            .count();
        n as f64 / (size * size) as f64
    };

    let full = inked(TILE_PX);
    let half = inked(TILE_PX / 2);
    assert!(full > 0.2, "fixture rendered nearly blank at {TILE_PX} px");
    // Not exact — anti-aliasing over half the samples genuinely shifts a few
    // percent, and sub-pixel strokes land on more partial texels. A doubling
    // of every width is an order of magnitude outside this.
    assert!(
        (half / full - 1.0).abs() < 0.25,
        "ink coverage moved with the raster size ({full:.3} at {TILE_PX} px, \
             {half:.3} at {} px) — stroke widths are not scaling",
        TILE_PX / 2
    );
}

#[test]
fn overzoomed_render_differs_from_the_whole_tile() {
    let whole = render_mvt(GRONINGEN_Z14, SubTile::WHOLE).expect("render whole");
    let zoomed = render_mvt(
        GRONINGEN_Z14,
        SubTile {
            factor: 8,
            x: 3,
            y: 5,
        },
    )
    .expect("render sub");
    assert_ne!(whole, zoomed, "overzoom transform had no effect");
}

/// Culling a feature whose bounds miss the sub-tile window must be invisible
/// in the output — the whole point is that it is the *same* picture, cheaper.
///
/// This is the real gate on the margin arithmetic: get the stroke reach or the
/// anti-alias slack wrong and a road or a building edge that should bleed in
/// from just outside the window goes missing, which on the map reads as a seam
/// along a tile border and is exactly what a rendering change must not do.
/// A crop parcel painted into the basemap shows where it lies, stays out of
/// the rest of the tile, lands in the same place in every band and in an
/// overzoomed window, and sits *under* the streets and the water.
#[test]
fn an_overlay_fill_lands_under_the_streets_and_nowhere_else() {
    let tile = crate::testdata::groningen();
    const RED: u32 = 0xFF_00_00;
    let is_red = |px: &[u8]| px[0] == 0xFF && px[1] == 0 && px[2] == 0;
    let left_half = [OverlayFill {
        rings: vec![vec![[0.0, 0.0], [0.5, 0.0], [0.5, 1.0], [0.0, 1.0]]],
        rgb: RED,
    }];
    let n = TILE_PX as usize;
    for sub in [
        SubTile::WHOLE,
        SubTile {
            factor: 2,
            x: 0,
            y: 1,
        },
    ] {
        let one = render_mvt_banded(
            &tile,
            sub,
            TILE_PX,
            NOMINAL_Z14_TILE_M,
            false,
            1,
            &[],
            &left_half,
        )
        .expect("renders");
        let banded = render_mvt_banded(
            &tile,
            sub,
            TILE_PX,
            NOMINAL_Z14_TILE_M,
            false,
            4,
            &[],
            &left_half,
        )
        .expect("renders");
        let red_in = |rgba: &[u8], x0: usize, x1: usize| {
            (0..n)
                .flat_map(|y| (x0..x1).map(move |x| (y * n + x) * 4))
                .filter(|&i| is_red(&rgba[i..i + 4]))
                .count()
        };
        // The unit square's left half is the whole of this window for the
        // overzoomed one (x = 0 of 2), and its left half for the whole tile.
        let (inside, outside) = if sub.factor == 1 {
            (0..n / 2 - 2, n / 2 + 2..n)
        } else {
            (0..n, 0..0)
        };
        for rgba in [&one, &banded] {
            let covered = red_in(rgba, inside.start, inside.end);
            let area = n * inside.len();
            assert!(
                covered > area / 10,
                "{sub:?}: the fill is missing ({covered} of {area})"
            );
            assert!(covered < area, "{sub:?}: the fill went over the streets");
            assert_eq!(
                red_in(rgba, outside.start, outside.end),
                0,
                "{sub:?}: fill outside itself"
            );
        }
        // Banding moves no overlay pixel.
        let differ = one
            .chunks(4)
            .zip(banded.chunks(4))
            .filter(|(a, b)| is_red(a) != is_red(b))
            .count();
        assert!(
            differ < n,
            "{sub:?}: {differ} pixels changed between banded and single"
        );
    }
}

#[test]
fn banding_the_raster_changes_no_geometry() {
    let tile = TileData::parse(GRONINGEN_Z14).expect("parse fixture");
    // The whole tile and two overzoomed windows, so the band offset is
    // exercised against a non-zero sub-tile shift as well as against zero.
    // Each window on its own thread: they share nothing, and one after
    // another they were 1.4 s of renders.
    let tile = &tile;
    std::thread::scope(|s| {
        for (factor, x, y) in [(1, 0, 0), (4, 2, 1), (8, 3, 5)] {
            s.spawn(move || banding_changes_no_geometry_in(tile, SubTile { factor, x, y }));
        }
    });
}

/// One window of [`banding_the_raster_changes_no_geometry`].
fn banding_changes_no_geometry_in(tile: &TileData, sub: SubTile) {
    let factor = sub.factor;
    let whole = render_mvt_banded(tile, sub, TILE_PX, NOMINAL_Z14_TILE_M, false, 1, &[], &[])
        .expect("whole render");
    // 7 is in the list on purpose: it does not divide TILE_PX, so the
    // last band is short and the row arithmetic has to survive it.
    for bands in [2u32, 3, 4, 7] {
        let split = render_mvt_banded(
            tile,
            sub,
            TILE_PX,
            NOMINAL_Z14_TILE_M,
            false,
            bands,
            &[],
            &[],
        )
        .expect("banded render");
        assert_eq!(
            split.len(),
            whole.len(),
            "factor {factor} in {bands} bands produced a different sized raster"
        );

        let max_delta = split
            .iter()
            .zip(&whole)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        let differing = split.iter().zip(&whole).filter(|(a, b)| a != b).count();
        let stride = (TILE_PX * 4) as usize;
        let worst_row = split
            .chunks(stride)
            .zip(whole.chunks(stride))
            .map(|(a, b)| a.iter().zip(b).filter(|(p, q)| p != q).count())
            .max()
            .unwrap_or(0);

        // **Not bit-equality, and the reason is recorded rather than
        // waved at.** Shifting the transform by the band's first row
        // changes the magnitudes the coordinates are computed at, so f32
        // rounds a few edge samples differently and the anti-aliased
        // coverage moves by a step or two. Bleed rows remove the part
        // that is clipping (2,443 bytes to 312 on this fixture); this
        // residue is arithmetic and does not go away with more of them —
        // 128 rows of bleed still left 96, and 256 left 43.
        //
        // What must not happen is geometry going missing at a seam, and
        // that is a different shape entirely: it would be a whole row
        // disagreeing, at full channel range, not four bytes in a
        // thousand at a delta of three. Both are bounded here so the
        // failure is legible either way.
        // 8/255 is not a number picked to make this pass: it is the
        // figure `flush_batch` already records for the paint batching,
        // whose delta against a per-feature raster is likewise all
        // anti-aliased edges. An overzoomed window rounds coarser than
        // the whole tile — factor 4 reaches 5 where factor 1 reaches 3 —
        // because the coordinates are four times larger before they are
        // shifted.
        //
        // This is the weaker of the two checks. Lost geometry is caught
        // by `worst_row` below: a missing road is a long run of pixels
        // in one row at full channel range, not a handful at a delta of
        // five.
        //
        // Raised 8 → 12 on 2026-09-26, when the stroke widths stopped
        // being read as tile units (`flush_batch`). Until then every
        // street in a whole z14 tile was a sub-pixel hairline, so factor 1
        // had no stroke edges at a real width for the shift to round;
        // with them it measured 12 at seven bands, in at most 21 bytes of
        // any row and 399 of the tile. The seam bounds below are
        // unchanged and all hold.
        //
        // Raised 12 → 16 on 2026-09-27, when the land took realistic
        // tones. The residue is a fraction of the contrast across the
        // edge being rounded, and tarmac against a dark aerial green
        // is more contrast than it was against pale paper: the same
        // seven bands measured 15. The seam and row bounds below are
        // unchanged and hold.
        assert!(
                max_delta <= 16,
                "factor {factor} in {bands} bands moved a channel by {max_delta}/255 — past anti-aliasing, so a band lost or gained geometry"
            );
        // Scaled by the number of seams, because that is what the residue
        // is made of: one seam costs ~312 bytes on this fixture and six
        // cost ~1,259. An absolute bound would pass at four bands and
        // fail at seven for nothing worth failing over.
        assert!(
                differing * 2000 <= whole.len() * bands as usize,
                "factor {factor} in {bands} bands differs in {differing} of {} bytes — more than rounding at {} seams",
                whole.len(),
                bands - 1
            );
        assert!(
                worst_row * 4 <= stride,
                "factor {factor} in {bands} bands has a row with {worst_row} of                      {stride} bytes differing — that is a seam, not rounding"
            );
    }
}

#[test]
fn culling_off_screen_features_changes_no_pixels() {
    let tile = TileData::parse(GRONINGEN_Z14).expect("parse fixture");
    // Corners, edges and middle, across the overzoom range map_stream uses
    // (z15/z16/z17 over a z14 source → factor 2/4/8).
    for (factor, x, y) in [
        (2, 0, 0),
        (2, 1, 1),
        (4, 0, 3),
        (4, 2, 1),
        (8, 3, 5),
        (8, 7, 7),
        (8, 0, 0),
    ] {
        let sub = SubTile { factor, x, y };
        let culled = render_mvt_culled(&tile, sub, TILE_PX, NOMINAL_Z14_TILE_M, true)
            .expect("culled render");
        let full = render_mvt_culled(&tile, sub, TILE_PX, NOMINAL_Z14_TILE_M, false)
            .expect("unculled render");
        assert_eq!(
            culled.len(),
            full.len(),
            "factor {factor} ({x},{y}): raster size changed"
        );
        // **One count, and no more.** tiny-skia builds a batch's edges by
        // one of two routes — straight, when the whole path lies inside the
        // pixmap (`path_contained_in_clip`), or through its edge clipper
        // otherwise — and the two round a few edges a hair differently.
        // Culling the off-screen members of a batch can move it from the
        // second route to the first. Measured once real stroke widths
        // reached the overzoomed tiles (2026-09-26): one channel of one
        // pixel, by one, from a `bridges` outline wholly outside factor 4
        // (0,3). A feature the cull wrongly dropped is a different shape — a
        // run of pixels at the stroke's full contrast — and still fails.
        //
        // Counted in *pixels*, not in levels: how far that one edge pixel
        // moves is the contrast between the stroke and the ground under it,
        // which is a palette choice. The earthier ground of 2026-09-27 put
        // the same pixel 5 levels off instead of 1.
        let differing = culled
            .chunks_exact(4)
            .zip(full.chunks_exact(4))
            .filter(|(a, b)| a.iter().zip(b.iter()).any(|(p, q)| p.abs_diff(*q) > 1))
            .count();
        assert!(
            differing <= 1,
            "factor {factor} ({x},{y}): culling changed {differing} pixels by more than rounding"
        );
    }
}

/// Not a test — a measurement hatch for the overzoom cull, since its whole
/// justification is a number. `cargo test -p cartopolis_geo bench_overzoom_cull
/// -- --ignored --nocapture`.
#[test]
#[ignore = "printing hatch: it reports numbers, it does not assert them"]
fn bench_overzoom_cull() {
    let tile = TileData::parse(GRONINGEN_Z14).expect("parse fixture");
    for factor in [2u32, 4, 8] {
        let subs: Vec<SubTile> = (0..factor)
            .flat_map(|x| (0..factor).map(move |y| SubTile { factor, x, y }))
            .collect();
        for (label, cull) in [("cull ", true), ("full ", false)] {
            let t0 = std::time::Instant::now();
            for &sub in &subs {
                render_mvt_culled(&tile, sub, TILE_PX, NOMINAL_Z14_TILE_M, cull).expect("render");
            }
            let dt = t0.elapsed();
            println!(
                "factor {factor:>2} {label} {:>4} tiles  total {:>8.1} ms  per tile {:>6.2} ms",
                subs.len(),
                dt.as_secs_f64() * 1000.0,
                dt.as_secs_f64() * 1000.0 / subs.len() as f64,
            );
        }
    }
}

/// Not a test — a measurement hatch answering "which paint pass is the
/// 121 ms?", timed inside the real loop (`perf::passes`) because the one
/// prior per-layer attribution was a reconstruction and summed to more than
/// double the measured total. `cargo test -p cartopolis_geo --release
/// dump_paint_split -- --ignored --nocapture`.
#[test]
#[ignore = "printing hatch: it reports numbers, it does not assert them"]
fn dump_paint_split() {
    let tile = TileData::parse(GRONINGEN_Z14).expect("parse fixture");
    // Serial: `perf::passes` is thread-local — see `render_mvt_serial`.
    let whole = || {
        render_mvt_banded(
            &tile,
            SubTile::WHOLE,
            TILE_PX,
            NOMINAL_Z14_TILE_M,
            false,
            1,
            &[],
            &[],
        )
    };
    for _ in 0..2 {
        whole().expect("warmup");
    }
    const N: u32 = 50;
    let (_, passes) = crate::perf::passes::measure(|| {
        for _ in 0..N {
            whole().expect("render");
        }
    });
    let loop_total: f64 = passes
        .iter()
        .filter(|(l, _)| !l.starts_with('Σ'))
        .map(|(_, d)| d.as_secs_f64())
        .sum();
    println!("whole 512 px tile, mean of {N}:");
    for (label, d) in &passes {
        let ms = d.as_secs_f64() * 1000.0 / N as f64;
        if label.starts_with('Σ') {
            println!("  {label:<22} {ms:>7.2} ms/tile  (subset of the passes above)");
        } else {
            let pct = 100.0 * d.as_secs_f64() / loop_total;
            println!("  {label:<22} {ms:>7.2} ms/tile  {pct:>5.1}%");
        }
    }
    println!(
        "  {:<22} {:>7.2} ms/tile",
        "paint loop total",
        loop_total * 1000.0 / N as f64
    );
}

/// Not a test — the tiny-skia ↔ `vello_cpu` A/B recorded in
/// `docs/notes/tile-raster-cost.md`. Renders the same whole z14 tile — same
/// paint order, colours, widths, round caps and joins, same transform —
/// through both rasterisers and prints ms/tile, plus writes
/// `/tmp/vt_vello.png` so the output can be eyeballed against
/// `dump_png`'s. `cargo test -p cartopolis_geo --release bench_vello_cpu
/// -- --ignored --nocapture`.
#[test]
#[ignore = "printing hatch: it reports numbers, it does not assert them"]
fn bench_vello_cpu() {
    use vello_cpu::color::{AlphaColor, Srgb};
    use vello_cpu::kurbo::{Affine, BezPath, Cap, Join, Stroke as VStroke};
    use vello_cpu::peniko::Fill;
    use vello_cpu::{Pixmap as VPixmap, RenderContext, Resources};

    let tile = TileData::parse(GRONINGEN_Z14).expect("parse fixture");

    let solid = |rgb: u32| {
        AlphaColor::<Srgb>::from_rgba8(
            ((rgb >> 16) & 0xFF) as u8,
            ((rgb >> 8) & 0xFF) as u8,
            (rgb & 0xFF) as u8,
            255,
        )
    };
    fn bez_area(pb: &mut BezPath, geom: &Geometry<f32>) {
        let ring = |pb: &mut BezPath, ls: &geo_types::LineString<f32>| {
            let mut first = true;
            for c in ls.coords() {
                if !c.x.is_finite() || !c.y.is_finite() {
                    continue;
                }
                if first {
                    pb.move_to((c.x as f64, c.y as f64));
                    first = false;
                } else {
                    pb.line_to((c.x as f64, c.y as f64));
                }
            }
            if !first {
                pb.close_path();
            }
        };
        match geom {
            Geometry::Polygon(p) => {
                ring(pb, p.exterior());
                p.interiors().iter().for_each(|h| ring(pb, h));
            }
            Geometry::MultiPolygon(mp) => {
                for p in mp {
                    ring(pb, p.exterior());
                    p.interiors().iter().for_each(|h| ring(pb, h));
                }
            }
            _ => {}
        }
    }
    fn bez_lines(pb: &mut BezPath, geom: &Geometry<f32>) {
        let line = |pb: &mut BezPath, ls: &geo_types::LineString<f32>| {
            let mut first = true;
            for c in ls.coords() {
                if !c.x.is_finite() || !c.y.is_finite() {
                    continue;
                }
                if first {
                    pb.move_to((c.x as f64, c.y as f64));
                    first = false;
                } else {
                    pb.line_to((c.x as f64, c.y as f64));
                }
            }
        };
        match geom {
            Geometry::LineString(ls) => line(pb, ls),
            Geometry::MultiLineString(mls) => mls.iter().for_each(|ls| line(pb, ls)),
            Geometry::Polygon(_) | Geometry::MultiPolygon(_) => bez_area(pb, geom),
            _ => {}
        }
    }

    // The paint loop mirrored for vello_cpu: same style resolution as
    // `render_mvt_culled` on a whole z14 tile (no cull, no far style),
    // batching by (colour, width) run exactly as the real loop does.
    let vello_render = || -> VPixmap {
        let size = TILE_PX as u16;
        let mut ctx = RenderContext::new(size, size);
        let mut resources = Resources::new();
        let mut target = VPixmap::new(size, size);
        ctx.set_paint(solid(palette::PAPER));
        ctx.fill_path(&BezPath::from_vec(vec![
            vello_cpu::kurbo::PathEl::MoveTo((0.0, 0.0).into()),
            vello_cpu::kurbo::PathEl::LineTo((TILE_PX as f64, 0.0).into()),
            vello_cpu::kurbo::PathEl::LineTo((TILE_PX as f64, TILE_PX as f64).into()),
            vello_cpu::kurbo::PathEl::LineTo((0.0, TILE_PX as f64).into()),
            vello_cpu::kurbo::PathEl::ClosePath,
        ]));
        ctx.set_fill_rule(Fill::NonZero);
        for (layer_name, draw) in PAINT_ORDER {
            let Some(meta) = tile.layer(layer_name) else {
                continue;
            };
            let features = tile.features(meta);
            if features.is_empty() {
                continue;
            }
            let extent = TileData::extent(meta);
            let scale = (TILE_PX as f32 / extent) as f64;
            ctx.set_transform(Affine::new([scale, 0.0, 0.0, scale, 0.0, 0.0]));

            let street_order: Option<Vec<&Feature>> =
                matches!(draw, Draw::Streets { .. }).then(|| {
                    let mut v: Vec<&Feature> = features
                        .iter()
                        .filter(|feat| !crate::tunnels::underground(feat))
                        .collect();
                    v.sort_by(|a, b| {
                        layer_of(a)
                            .cmp(&layer_of(b))
                            .then(flag_of(a, "bridge").cmp(&flag_of(b, "bridge")))
                            .then(
                                street_style(a, NOMINAL_Z14_TILE_M)
                                    .0
                                    .total_cmp(&street_style(b, NOMINAL_Z14_TILE_M).0),
                            )
                    });
                    v
                });
            let ordered: Vec<&Feature> = match street_order {
                Some(list) => list,
                None => features.iter().collect(),
            };

            let mut batch = BezPath::new();
            let mut batch_key: Option<BatchKey> = None;
            let mut batch_points = 0usize;
            let flush = |ctx: &mut RenderContext, batch: &mut BezPath, key: Option<BatchKey>| {
                let Some((rgb, width_bits, _)) = key else {
                    return;
                };
                if batch.elements().is_empty() {
                    return;
                }
                ctx.set_paint(solid(rgb));
                match width_bits {
                    None => ctx.fill_path(batch),
                    Some(bits) => {
                        ctx.set_stroke(
                            VStroke::new(f32::from_bits(bits) as f64)
                                .with_caps(Cap::Round)
                                .with_join(Join::Round),
                        );
                        ctx.stroke_path(batch);
                    }
                }
                batch.truncate(0);
            };
            for feature in ordered {
                let kind = kind_of(feature);
                let style: Option<(u32, Option<f32>)> = match draw {
                    Draw::Fill(rgb) => Some((*rgb, None)),
                    Draw::FillByKind { default, kinds } => Some((
                        kind.and_then(|k| kinds.iter().find(|(n, _)| *n == k).map(|(_, c)| *c))
                            .unwrap_or(*default),
                        None,
                    )),
                    Draw::Line { color, width } => Some((*color, Some(*width))),
                    Draw::Outline { color, width } => Some((*color, Some(*width))),
                    Draw::Streets { casing } => {
                        let (w, fill, case) = street_style(feature, NOMINAL_Z14_TILE_M);
                        Some(if *casing {
                            (case, Some(w + 0.5))
                        } else {
                            (fill, Some(w))
                        })
                    }
                };
                let Some((rgb, width)) = style else { continue };
                let key = (rgb, width.map(f32::to_bits), false);
                let cap = if width.is_some() {
                    STROKE_BATCH_POINTS
                } else {
                    FILL_BATCH_POINTS
                };
                if batch_key != Some(key) || batch_points >= cap {
                    flush(&mut ctx, &mut batch, batch_key);
                    batch_key = Some(key);
                    batch_points = 0;
                }
                match width {
                    None => bez_area(&mut batch, &feature.geometry),
                    Some(_) => bez_lines(&mut batch, &feature.geometry),
                }
                batch_points += count_points(&feature.geometry, width.is_none());
            }
            flush(&mut ctx, &mut batch, batch_key);
        }
        ctx.flush();
        ctx.render(&mut target, &mut resources);
        target
    };

    let tiny_render =
        || render_mvt_culled(&tile, SubTile::WHOLE, TILE_PX, NOMINAL_Z14_TILE_M, false);

    for _ in 0..2 {
        tiny_render().expect("warmup");
        vello_render();
    }
    const N: u32 = 20;
    let t0 = std::time::Instant::now();
    for _ in 0..N {
        tiny_render().expect("render");
    }
    let tiny_ms = t0.elapsed().as_secs_f64() * 1000.0 / N as f64;
    let t0 = std::time::Instant::now();
    for _ in 0..N {
        vello_render();
    }
    let vello_ms = t0.elapsed().as_secs_f64() * 1000.0 / N as f64;
    println!("tiny-skia (batched): {tiny_ms:.2} ms/tile");
    println!("vello_cpu (batched): {vello_ms:.2} ms/tile");

    let out = vello_render();
    let rgba: Vec<u8> = out
        .data()
        .iter()
        .flat_map(|p| [p.r, p.g, p.b, p.a])
        .collect();
    image::save_buffer(
        "/tmp/vt_vello.png",
        &rgba,
        TILE_PX,
        TILE_PX,
        image::ExtendedColorType::Rgba8,
    )
    .expect("write png");
    println!("wrote /tmp/vt_vello.png");
}

/// Not a test — an eyeball hatch. `cargo test -p cartopolis_geo
/// dump_png -- --ignored --nocapture` writes the fixture at z14 and at an
/// overzoomed z17 so the styling can actually be looked at.
#[test]
#[ignore = "eyeball hatch: it writes PNGs to look at, it asserts nothing"]
fn dump_png() {
    for (name, sub) in [
        ("/tmp/vt_z14.png", SubTile::WHOLE),
        (
            "/tmp/vt_z17.png",
            SubTile {
                factor: 8,
                x: 3,
                y: 5,
            },
        ),
    ] {
        let rgba = render_mvt(GRONINGEN_Z14, sub).expect("render");
        image::save_buffer(
            name,
            &rgba,
            TILE_PX,
            TILE_PX,
            image::ExtendedColorType::Rgba8,
        )
        .expect("write png");
        println!("wrote {name}");
    }
}

/// The other eyeball hatch: print what a tile actually contains — layers,
/// feature counts, and one feature's attributes. Useful when a layer renders
/// blank and the question is "is it the style or the data?".
#[test]
#[ignore = "eyeball hatch: it prints a tile's contents, it asserts nothing"]
fn dump_layers() {
    let owned = std::env::var("CARTO_DUMP_MVT")
        .ok()
        .map(|p| std::fs::read(p).expect("read tile"));
    let tile: &[u8] = owned.as_deref().unwrap_or(GRONINGEN_Z14);
    let reader = Reader::new(tile.to_vec()).unwrap();
    let meta = reader.get_layer_metadata().unwrap();
    println!("tile bytes: {}", tile.len());
    if let Some(l) = meta.iter().find(|l| l.name == "bridges") {
        println!("\n`bridges` layer:");
        for f in reader.get_features(l.layer_index).unwrap().iter().take(8) {
            let shape = match &f.geometry {
                Geometry::LineString(ls) => format!("LineString {} pts", ls.0.len()),
                Geometry::MultiLineString(m) => format!("MultiLineString {} parts", m.0.len()),
                Geometry::Polygon(p) => format!("Polygon {} pts", p.exterior().0.len()),
                Geometry::MultiPolygon(m) => format!("MultiPolygon {} parts", m.0.len()),
                g => format!("{g:?}"),
            };
            // Deck footprint in metres over a 1000 m tile: is this an outline
            // we could stand a slab on, or a token?
            let s = 1000.0 / 4096.0;
            let mut area = 0.0f32;
            let mut bbox = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
            if let Geometry::MultiPolygon(mp) = &f.geometry {
                for poly in mp {
                    let r = poly.exterior();
                    for w in r.0.windows(2) {
                        area += (w[0].x * w[1].y - w[1].x * w[0].y) * 0.5;
                    }
                    for c in &r.0 {
                        bbox = [
                            bbox[0].min(c.x),
                            bbox[1].min(c.y),
                            bbox[2].max(c.x),
                            bbox[3].max(c.y),
                        ];
                    }
                }
            }
            println!(
                "  {shape} {:.0} m² bbox {:.0}×{:.0} m — {:?}",
                area.abs() * s * s,
                (bbox[2] - bbox[0]) * s,
                (bbox[3] - bbox[1]) * s,
                f.properties.as_ref().and_then(|p| p.get("name"))
            );
        }
    }
    // Every bridge-flagged street, with the length of its span in tile units
    // (÷ extent × tile metres for real length) — the number a deck height has
    // to be derived from.
    let idx = meta
        .iter()
        .find(|l| l.name == "streets")
        .unwrap()
        .layer_index;
    println!("\nbridge-flagged streets (length in metres over a 1000 m tile):");
    for f in reader.get_features(idx).unwrap().iter() {
        if !flag_of(f, "bridge") {
            continue;
        }
        let len: f32 = match &f.geometry {
            Geometry::LineString(ls) => {
                ls.0.windows(2)
                    .map(|w| ((w[1].x - w[0].x).powi(2) + (w[1].y - w[0].y).powi(2)).sqrt())
                    .sum()
            }
            _ => 0.0,
        };
        println!(
            "  {:>12} {:>7.0} m  {:?}",
            kind_of(f).unwrap_or("?"),
            len * 1000.0 / 4096.0,
            drawn_street(f)
        );
    }
    for l in &meta {
        println!(
            "  {:<26} {:>5} features  extent {}",
            l.name, l.feature_count, l.extent
        );
    }
    let idx = meta
        .iter()
        .find(|l| l.name == "streets")
        .unwrap()
        .layer_index;
    for f in reader.get_features(idx).unwrap().iter().take(400) {
        let props = f.properties.as_ref().unwrap();
        if props.contains_key("kind") {
            println!("\nsample feature from `streets`:");
            let mut kv: Vec<_> = props.iter().collect();
            kv.sort_by(|(a, _), (b, _)| a.cmp(b));
            for (k, v) in kv {
                println!("    {k} = {v:?}");
            }
            if let Geometry::LineString(ls) = &f.geometry {
                println!("    geometry = LineString, {} points; first 3:", ls.0.len());
                for c in ls.0.iter().take(3) {
                    println!("        ({}, {})", c.x, c.y);
                }
            }
            break;
        }
    }
}

/// **The image paints the surveyed ground and drops what it replaces.** Over a
/// cell whose survey carries carriageways, the residential streets the meshes
/// draw from the survey are not stroked in OSM's tarmac, and the paving shows
/// in its own colour where they were; the streets that stay OSM's still draw.
#[test]
fn the_survey_is_painted_and_replaces_its_streets() {
    let tile = crate::testdata::groningen();
    let size = 256;
    let minor = RoadClass::Minor.flat_rgb();
    let count = |px: &[u8], rgb: u32| {
        px.chunks_exact(4)
            .filter(|p| u32::from(p[0]) << 16 | u32::from(p[1]) << 8 | u32::from(p[2]) == rgb)
            .count()
    };
    let plain = render_mvt_surveyed(
        &tile,
        SubTile::WHOLE,
        size,
        NOMINAL_Z14_TILE_M,
        &[],
        &[],
        &NO_SURVEY,
    )
    .unwrap();
    let brick = 0x98_64_52;
    let survey = Survey {
        paving: vec![OverlayFill {
            rings: vec![vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]],
            rgb: brick,
        }],
        cells: vec![CoveredCell {
            bounds: [0.0, 0.0, 1.0, 1.0],
            cover: SurveyedCover::Streets,
        }],
    };
    let surveyed = render_mvt_surveyed(
        &tile,
        SubTile::WHOLE,
        size,
        NOMINAL_Z14_TILE_M,
        &[],
        &[],
        &survey,
    )
    .unwrap();
    // `tertiary` shares the minor tone and stays OSM's (it carries markings),
    // so what must go is the rest: everything the residential network drew.
    let only_paths = Survey {
        cells: vec![CoveredCell {
            bounds: [0.0, 0.0, 1.0, 1.0],
            cover: SurveyedCover::Paths,
        }],
        ..survey.clone()
    };
    let paths = render_mvt_surveyed(
        &tile,
        SubTile::WHOLE,
        size,
        NOMINAL_Z14_TILE_M,
        &[],
        &[],
        &only_paths,
    )
    .unwrap();
    let (before, after) = (count(&paths, minor), count(&surveyed, minor));
    assert!(
        before > 500,
        "the fixture strokes residential streets: {before}"
    );
    assert!(
        after * 10 < before * 7,
        "the survey's streets are not stroked in OSM's tarmac: {before} → {after}"
    );
    assert!(
        count(&paths, RoadClass::Path.flat_rgb()) * 2 < count(&plain, RoadClass::Path.flat_rgb()),
        "over a surveyed cell the paths are the survey's too"
    );
    assert!(
        count(&surveyed, brick) > count(&plain, brick) + 1000,
        "the paving shows in its own colour"
    );
    assert!(
        count(&surveyed, RoadClass::Major.flat_rgb()) * 2
            >= count(&plain, RoadClass::Major.flat_rgb()),
        "the marked roads stay OSM's"
    );
}
