use super::*;
use crate::vector_tiles::bytes::{
    render_mvt, roads_and_decks, tessellate_bridges, tessellate_roads,
};

// A real z14 tile over Groningen, from the VersaTiles Shortbread source
// (now self-hosted at tiles.cartopolis.org; the archive is identical).
use crate::testdata::GRONINGEN_Z14;

/// **A segment that crosses a deck between two vertices is split onto it.**
/// Both ends are off the deck, so asked only at its vertices the whole
/// segment read as ground.
#[test]
fn a_segment_crossing_a_deck_between_its_vertices_rides_it() {
    let ring = vec![
        [0.0, -5.0],
        [60.0, -5.0],
        [60.0, 5.0],
        [0.0, 5.0],
        [0.0, -5.0],
    ];
    let deck = BridgeDeck {
        positions: Vec::new(),
        indices: Vec::new(),
        rings: vec![ring],
        min: [0.0, -5.0],
        max: [60.0, 5.0],
    };
    for line in [[[-10.0, 0.0], [70.0, 0.0]], [[70.0, 0.0], [-10.0, 0.0]]] {
        let on: f32 = split_by_deck(&line, std::slice::from_ref(&deck))
            .iter()
            .filter(|(o, _)| o.is_some())
            .map(|(_, p)| crate::tunnels::polyline_len(p))
            .sum();
        assert!(on > 59.0, "only {on:.1} m of a 60 m span is on its deck");
    }
}

/// **The street that runs on from a bridge way inside its outline is on the
/// deck to the outline's edge**, and a street meeting no bridge end — one
/// passing under the structure — stays on the ground whole.
#[test]
fn a_street_carrying_on_from_a_bridge_way_rides_the_deck_to_its_edge() {
    let ring = vec![
        [0.0, -5.0],
        [24.0, -5.0],
        [24.0, 5.0],
        [0.0, 5.0],
        [0.0, -5.0],
    ];
    let deck = BridgeDeck {
        positions: Vec::new(),
        indices: Vec::new(),
        rings: vec![ring],
        min: [0.0, -5.0],
        max: [24.0, 5.0],
    };
    let decks = std::slice::from_ref(&deck);
    // The bridge way stops at x = 4, four metres inside the deck; the street
    // carries on west from there.
    let ends = [(0usize, [4.0, 0.0]), (0usize, [20.0, 0.0])];
    let parts = carried_onto_deck(vec![[4.0, 0.0], [-30.0, 0.0]], decks, &ends);
    assert_eq!(parts.len(), 2, "{parts:?}");
    assert_eq!(parts[0].0, Some(0));
    assert!(
        (crate::tunnels::polyline_len(&parts[0].1) - 4.0).abs() < 0.1,
        "{parts:?}"
    );
    assert_eq!(parts[1].0, None);
    // Joined at its *tail*, the same.
    let parts = carried_onto_deck(vec![[-30.0, 0.0], [4.0, 0.0]], decks, &ends);
    assert_eq!(parts.last().map(|p| p.0), Some(Some(0)), "{parts:?}");
    // A street under the deck, crossing it, joins no bridge end.
    let under = carried_onto_deck(vec![[12.0, -30.0], [12.0, 30.0]], decks, &ends);
    assert_eq!(under.len(), 1, "{under:?}");
    assert_eq!(under[0].0, None);
    // A street that joins at one end and crosses a second deck further on
    // keeps that crossing on the ground, in one piece with the rest.
    let far = BridgeDeck {
        rings: vec![vec![
            [-20.0, -5.0],
            [-14.0, -5.0],
            [-14.0, 5.0],
            [-20.0, 5.0],
            [-20.0, -5.0],
        ]],
        min: [-20.0, -5.0],
        max: [-14.0, 5.0],
        ..deck.clone()
    };
    let two = [deck.clone(), far];
    let parts = carried_onto_deck(vec![[4.0, 0.0], [-30.0, 0.0]], &two, &ends);
    assert_eq!(parts.len(), 2, "{parts:?}");
    assert_eq!((parts[0].0, parts[1].0), (Some(0), None));
}

/// **A bridge's approach runs along the road beyond its abutment**, not only
/// along the sliver of the bridge way that overhangs the deck: OSM's bridge
/// way stops at the abutment, and every approach found that way over four
/// Groningen tiles was 0.0–0.3 m long (2026-09-23), so each embankment ended
/// in a cliff. Over the fixture at least one approach must now reach well
/// past the grading's own twenty-metre dilation.
#[test]
fn a_bridge_approach_follows_the_road_past_the_abutment() {
    let tile = crate::testdata::groningen();
    let tile_m = 1471.0;
    let mut decks = super::tessellate_bridges(&tile, tile_m).expect("bridges tessellate");
    assert!(
        !decks.is_empty(),
        "the fixture has no bridge deck to approach"
    );
    let roads =
        super::tessellate_roads(&tile, tile_m, &mut decks, None, false).expect("roads tessellate");
    let longest = roads
        .approaches
        .iter()
        .map(|a| crate::tunnels::polyline_len(&a.points))
        .fold(0.0f32, f32::max);
    assert!(
        longest >= 40.0,
        "the longest approach is {longest:.1} m — still the overhanging sliver"
    );
    for a in &roads.approaches {
        assert!((a.deck as usize) < decks.len());
        assert!(
            crate::tunnels::polyline_len(&a.points) <= APPROACH_REACH_M + 1.0,
            "an approach ran past the reach"
        );
    }
}

/// **A fork past the abutment is followed down every branch.** An arm leaves
/// the deck heading east and splits 20 m on into two slip roads, one bearing
/// north-east and one south-east; a road turning off at a right angle is not
/// the approach. Stopping at the fork graded 20 m of ramp and left the rest
/// of a 7 m arm as a cliff.
#[test]
fn an_approach_is_followed_down_both_branches_of_a_fork() {
    let line = |pts: Vec<[f32; 2]>| crate::tunnels::SurfaceLine {
        pts,
        own: [true, true],
        bridge: false,
    };
    let lines = vec![
        line(vec![[0.0, 0.0], [20.0, 0.0]]),
        line(vec![[20.0, 0.0], [80.0, 25.0]]),
        line(vec![[20.0, 0.0], [80.0, -25.0]]),
        line(vec![[20.0, 0.0], [20.0, 60.0]]),
    ];
    let mut paths = Vec::new();
    super::follow_approach(
        &lines,
        [0.0, 0.0],
        [1.0, 0.0],
        &mut Vec::new(),
        0.0,
        Vec::new(),
        &mut paths,
    );
    assert_eq!(paths.len(), 2, "{paths:?}");
    let ends: Vec<[f32; 2]> = paths.iter().map(|p| *p.last().unwrap()).collect();
    assert!(
        ends.contains(&[80.0, 25.0]) && ends.contains(&[80.0, -25.0]),
        "{ends:?}"
    );
}

/// Sizing data for the geometry path (ADR-035 option B): how many vertices and
/// triangles each layer would cost if tessellated instead of rasterised, and
/// what the raster path costs in CPU time for comparison.
#[test]
#[ignore = "printing hatch: it reports numbers, it does not assert them"]
fn dump_geometry_cost() {
    let reader = Reader::new(GRONINGEN_Z14.to_vec()).unwrap();
    let meta = reader.get_layer_metadata().unwrap();

    fn count(geom: &Geometry<f32>) -> (usize, usize, usize) {
        // (vertices, earcut-triangles for areas, ribbon-triangles for lines)
        match geom {
            Geometry::Polygon(p) => {
                let n: usize =
                    p.exterior().0.len() + p.interiors().iter().map(|r| r.0.len()).sum::<usize>();
                (n, n.saturating_sub(2), 0)
            }
            Geometry::MultiPolygon(mp) => mp
                .iter()
                .map(|p| count(&Geometry::Polygon(p.clone())))
                .fold((0, 0, 0), |a, b| (a.0 + b.0, a.1 + b.1, a.2 + b.2)),
            Geometry::LineString(ls) => (ls.0.len(), 0, ls.0.len().saturating_sub(1) * 2),
            Geometry::MultiLineString(mls) => mls
                .iter()
                .map(|ls| count(&Geometry::LineString(ls.clone())))
                .fold((0, 0, 0), |a, b| (a.0 + b.0, a.1 + b.1, a.2 + b.2)),
            _ => (0, 0, 0),
        }
    }

    println!(
        "{:<24} {:>8} {:>9} {:>10}",
        "layer", "features", "vertices", "triangles"
    );
    let mut tri_total = 0usize;
    let mut tri_without_buildings = 0usize;
    for l in &meta {
        let feats = reader.get_features(l.layer_index).unwrap();
        let (v, ta, tl) = feats
            .iter()
            .map(|f| count(&f.geometry))
            .fold((0, 0, 0), |a, b| (a.0 + b.0, a.1 + b.1, a.2 + b.2));
        let tris = ta + tl;
        tri_total += tris;
        if l.name != "buildings" {
            tri_without_buildings += tris;
        }
        if v > 0 {
            println!(
                "{:<24} {:>8} {:>9} {:>10}",
                l.name, l.feature_count, v, tris
            );
        }
    }
    println!("\ntriangles/tile, all layers      : {tri_total}");
    println!("triangles/tile, minus buildings : {tri_without_buildings}");

    // Routing needs stable feature identity across tile seams — check whether
    // the source preserves OSM ids, and how many streets touch a tile edge.
    let sidx = meta
        .iter()
        .find(|l| l.name == "streets")
        .unwrap()
        .layer_index;
    let streets = reader.get_features(sidx).unwrap();
    let with_id = streets.iter().filter(|f| f.id.is_some()).count();
    let touches_edge = streets
        .iter()
        .filter(|f| match &f.geometry {
            Geometry::LineString(ls) => {
                ls.0.iter()
                    .any(|c| c.x <= 0.0 || c.y <= 0.0 || c.x >= 4096.0 || c.y >= 4096.0)
            }
            Geometry::MultiLineString(mls) => mls.iter().any(|ls| {
                ls.0.iter()
                    .any(|c| c.x <= 0.0 || c.y <= 0.0 || c.x >= 4096.0 || c.y >= 4096.0)
            }),
            _ => false,
        })
        .count();
    println!("\nstreets with a feature id : {with_id}/{}", streets.len());
    println!(
        "streets clipped at a seam : {touches_edge}/{}",
        streets.len()
    );

    // Raster path CPU cost, for comparison.
    let t0 = std::time::Instant::now();
    const N: u32 = 20;
    for _ in 0..N {
        let _ = render_mvt(GRONINGEN_Z14, SubTile::WHOLE).unwrap();
    }
    println!(
        "\nrasterise {TILE_PX}px tile: {:.1} ms/tile",
        t0.elapsed().as_secs_f64() * 1000.0 / f64::from(N)
    );

    let t0 = std::time::Instant::now();
    for _ in 0..N {
        let r = Reader::new(GRONINGEN_Z14.to_vec()).unwrap();
        for l in r.get_layer_metadata().unwrap() {
            let _ = r.get_features(l.layer_index).unwrap();
        }
    }
    println!(
        "decode all layers        : {:.1} ms/tile",
        t0.elapsed().as_secs_f64() * 1000.0 / f64::from(N)
    );
}

/// Over a cell whose paving was surveyed, the footway and cycle ribbons must
/// not be drawn — drawing them twice puts a constant-width line down the
/// middle of a real pavement. Every other *class* is OSM's and must survive
/// the flag: `Minor` stays, because a residential street is still OSM's even
/// though the pedestrian streets inside that class now go
/// ([`surveyed_paving_also_drops_the_pedestrian_ribbons`] is what pins those).
#[test]
fn surveyed_paving_drops_the_path_ribbons_and_no_whole_class_else() {
    let tile = TileData::parse(GRONINGEN_Z14).expect("fixture");
    let groups = |surveyed: bool| -> Vec<SurfaceGroup> {
        let mut g: Vec<SurfaceGroup> =
            super::tessellate_roads(&tile, 1472.0, &mut [], None, surveyed)
                .expect("roads")
                .ground
                .into_iter()
                .map(|m| m.group)
                .collect();
        g.sort_by_key(|x| format!("{x:?}"));
        g.dedup();
        g
    };
    let with_osm = groups(false);
    let with_survey = groups(true);
    assert!(
        with_osm.contains(&SurfaceGroup::Road(RoadClass::Path)),
        "the fixture must have paths to drop, else this proves nothing"
    );
    assert!(!with_survey.contains(&SurfaceGroup::Road(RoadClass::Path)));
    assert!(!with_survey.contains(&SurfaceGroup::RoadCasing(RoadClass::Path)));
    for group in &with_osm {
        if matches!(
            group,
            SurfaceGroup::Road(RoadClass::Path) | SurfaceGroup::RoadCasing(RoadClass::Path)
        ) {
            continue;
        }
        assert!(
            with_survey.contains(group),
            "{group:?} is OSM's and must still be drawn"
        );
    }
}

/// A pedestrian street is paving, and the survey draws it — so its ribbon has
/// to go the same way a footway's does.
///
/// It is **not** a [`RoadClass::Path`], so the class-level rule above missed
/// it entirely and laid a 5 m tarmac ribbon with wheel tracks and a kerb band
/// over every surveyed square. Groningen's Grote Markt is mapped as fourteen
/// separate `pedestrian` ways, so what it drew was a scatter of tarmac stubs
/// — each one ending where its way ended, kerbed all the way round the cut —
/// lying on the pink paving the survey had already put there (2026-09-11).
///
/// The assertion is on the **`Minor` vertex count** rather than on the group
/// list, because that is the only place the loss can show: pedestrian shares
/// its class with every residential and service street in the tile, so the
/// group survives and only gets smaller.
#[test]
fn surveyed_paving_also_drops_the_pedestrian_ribbons() {
    let tile = TileData::parse(GRONINGEN_Z14).expect("fixture");
    let pedestrian = tile.layer("streets").map_or(0, |meta| {
        tile.features(meta)
            .iter()
            .filter(|f| kind_of(f) == Some("pedestrian"))
            .count()
    });
    assert!(
        pedestrian > 0,
        "the fixture carries no pedestrian street — this test proves nothing"
    );
    let minor_vertices = |surveyed: bool| -> usize {
        super::tessellate_roads(&tile, 1472.0, &mut [], None, surveyed)
            .expect("roads")
            .ground
            .iter()
            .filter(|m| {
                matches!(
                    m.group,
                    SurfaceGroup::Road(RoadClass::Minor)
                        | SurfaceGroup::RoadCasing(RoadClass::Minor)
                )
            })
            .map(|m| m.positions.len())
            .sum()
    };
    let with_osm = minor_vertices(false);
    let with_survey = minor_vertices(true);
    assert!(
        with_survey < with_osm,
        "the pedestrian ribbons must be dropped over surveyed paving \
             ({with_osm} vertices before, {with_survey} after)"
    );
    assert!(
        with_survey > 0,
        "only the pedestrian ways go — the residential streets in the same \
             class are OSM's and must still be drawn"
    );
}

#[test]
fn the_road_sheet_is_a_partition() {
    // The claim `road_stitch` exists to make: no two road ribbons cover the
    // same patch of ground, so nothing is left for the depth buffer to
    // arbitrate and no camera angle can change which one is drawn.
    //
    // Rasterise every road and kerb triangle top-down and count how often each
    // pixel is covered. Before the cut this measured 76.1 % of road pixels
    // covered twice and 12.5 % three times or more (2026-08-26).
    const PX: usize = 1200;
    let tile_m = 1000.0f32;
    let meshes = tessellate_roads(GRONINGEN_Z14, tile_m, &[]).expect("tessellate");
    let mut count: Vec<u16> = vec![0; PX * PX];
    for m in meshes
        .iter()
        .filter(|m| matches!(m.group, SurfaceGroup::Road(_) | SurfaceGroup::RoadCasing(_)))
    {
        for t in m.indices.chunks_exact(3) {
            let p: Vec<[f32; 2]> = t
                .iter()
                .map(|i| {
                    let v = m.positions[*i as usize];
                    [v[0] / tile_m * PX as f32, v[2] / tile_m * PX as f32]
                })
                .collect();
            let area = (p[1][0] - p[0][0]) * (p[2][1] - p[0][1])
                - (p[2][0] - p[0][0]) * (p[1][1] - p[0][1]);
            if area.abs() < 1e-9 {
                continue;
            }
            let lo = |f: &dyn Fn(&[f32; 2]) -> f32| {
                p.iter().map(f).fold(f32::MAX, f32::min).floor().max(0.0) as usize
            };
            let hi = |f: &dyn Fn(&[f32; 2]) -> f32| {
                (p.iter().map(f).fold(f32::MIN, f32::max).ceil()).min(PX as f32 - 1.0) as usize
            };
            let (x0, x1) = (lo(&|q: &[f32; 2]| q[0]), hi(&|q: &[f32; 2]| q[0]));
            let (y0, y1) = (lo(&|q: &[f32; 2]| q[1]), hi(&|q: &[f32; 2]| q[1]));
            for y in y0..=y1 {
                for x in x0..=x1 {
                    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                    let s = |a: &[f32; 2], b: &[f32; 2]| {
                        (b[0] - a[0]) * (py - a[1]) - (b[1] - a[1]) * (px - a[0])
                    };
                    let (d0, d1, d2) = (s(&p[0], &p[1]), s(&p[1], &p[2]), s(&p[2], &p[0]));
                    let inside = (d0 >= 0.0 && d1 >= 0.0 && d2 >= 0.0)
                        || (d0 <= 0.0 && d1 <= 0.0 && d2 <= 0.0);
                    if inside {
                        count[y * PX + x] = count[y * PX + x].saturating_add(1);
                    }
                }
            }
        }
    }
    let covered = count.iter().filter(|c| **c > 0).count();
    let doubled = count.iter().filter(|c| **c > 1).count();
    let frac = doubled as f32 / covered.max(1) as f32;
    println!(
        "road overdraw: {doubled}/{covered} px = {:.2}%",
        frac * 100.0
    );
    // 0.37 % measured (2026-09-11), down from 0.81 % while a ribbon was still
    // exempt from being cut against *itself*. That exemption was excused as
    // invisible — one material, one colour, one run of texture coordinates —
    // and the last clause is false: `v` is metres travelled, so a ribbon that
    // doubles back carries different lane markings on its two layers at the
    // same depth. What is left is the overlap kept on purpose, a flyover and
    // the road beneath it. The bound is a little over the measurement, to
    // catch the cut being skipped rather than a fixture that gains a street.
    assert!(
        frac < 0.01,
        "{:.2}% of covered road pixels are covered twice",
        frac * 100.0
    );
    // **Roads carry no height of their own**, and that is what lets the cut be
    // a plain two-dimensional one: every ribbon lies in the same plane, so
    // "who covers this ground" is the whole question. A stray lift here would
    // put the ribbons back on separate planes and quietly make the cut a lie.
    for m in meshes
        .iter()
        .filter(|m| matches!(m.group, SurfaceGroup::Road(_) | SurfaceGroup::RoadCasing(_)))
    {
        for p in &m.positions {
            assert_eq!(p[1], 0.0, "a road vertex was lifted off the road plane");
        }
        // The rung is a vertex attribute, and a mesh that carries the wrong
        // number of them fails pipeline validation — which bevy answers by
        // skipping the pass, i.e. the roads are simply not drawn, with healthy
        // mesh counts and nothing in the log.
        assert_eq!(
            m.rungs.len(),
            m.positions.len(),
            "a road mesh must carry one rung per vertex",
        );
    }
}

/// Eyeball hatch for junction geometry: rasterise the tessellated ribbons
/// top-down and colour each pixel by how many ribbons cover it, so stubs,
/// gaps and overlaps are visible without a GPU.
///
/// `cargo test -p cartopolis_geo dump_road_ribbons -- --ignored
/// --nocapture`; `CARTO_DUMP_WIN="x,z,size"` (tile metres) zooms to a junction.
#[test]
#[ignore = "eyeball hatch: it writes a PNG to look at, it asserts nothing"]
fn dump_road_ribbons() {
    const PX: usize = 1200;
    let tile_m = 1000.0f32;
    let (wx, wz, ws) = match std::env::var("CARTO_DUMP_WIN") {
        Ok(s) => {
            let v: Vec<f32> = s.split(',').map(|p| p.trim().parse().unwrap()).collect();
            (v[0], v[1], v[2])
        }
        Err(_) => (0.0, 0.0, tile_m),
    };
    // `CARTO_DUMP_MVT=/tmp/some.mvt` looks at a tile fetched from the live
    // source instead of the fixture — which is how you answer "is this street
    // missing because of the styling or because of the data?" for a place you
    // are actually looking at.
    let owned = std::env::var("CARTO_DUMP_MVT")
        .ok()
        .map(|p| std::fs::read(p).expect("read tile"));
    let tile: &[u8] = owned.as_deref().unwrap_or(GRONINGEN_Z14);

    // What the tile *says* about its streets, tallied — the tags are what
    // decide how wide each one is drawn and whether it is drawn at all, so
    // this is the difference between "the styling lost that road" and "the
    // data never had it".
    let parsed = TileData::parse(tile).unwrap();
    let streets = parsed.layer("streets").unwrap();
    let mut tally: std::collections::BTreeMap<String, usize> = Default::default();
    for f in parsed.features(streets) {
        let flags = ["bridge", "tunnel", "oneway", "oneway_reverse", "link"]
            .iter()
            .filter(|n| flag_of(f, n))
            .copied()
            .collect::<Vec<_>>()
            .join("+");
        let drawn = match drawn_street(f) {
            Some((class, half)) => format!("{class:?} {:.1} m wide", half * 2.0),
            None => "not drawn".to_string(),
        };
        *tally
            .entry(format!(
                "{:>12} {flags:<22} → {drawn}",
                kind_of(f).unwrap_or("?")
            ))
            .or_default() += 1;
    }
    for (what, n) in &tally {
        println!("  {n:>3} × {what}");
    }

    // Decks first, exactly as the streamer does it: a bridge's own span rides
    // the deck at the deck's height, so the ribbons this dumps are the ones
    // that really reach the ground.
    let decks = tessellate_bridges(tile, tile_m).expect("bridges");
    println!("  {} bridge decks", decks.len());
    let meshes = tessellate_roads(tile, tile_m, &decks).expect("tessellate");

    let mut count = vec![0u16; PX * PX];
    for m in &meshes {
        // Per class as well as in total: two ribbons of *different* classes
        // overlapping is a crossing, which the depth rungs settle. Two of the
        // same class is the same road drawn twice.
        let mut own = vec![0u16; PX * PX];
        for tri in m.indices.chunks_exact(3) {
            let p: Vec<[f32; 2]> = tri
                .iter()
                .map(|i| {
                    let v = m.positions[*i as usize];
                    [(v[0] - wx) / ws * PX as f32, (v[2] - wz) / ws * PX as f32]
                })
                .collect();
            let (x0, x1) = (
                p.iter()
                    .map(|q| q[0])
                    .fold(f32::MAX, f32::min)
                    .floor()
                    .max(0.0) as usize,
                p.iter()
                    .map(|q| q[0])
                    .fold(f32::MIN, f32::max)
                    .ceil()
                    .min(PX as f32 - 1.0) as usize,
            );
            let (y0, y1) = (
                p.iter()
                    .map(|q| q[1])
                    .fold(f32::MAX, f32::min)
                    .floor()
                    .max(0.0) as usize,
                p.iter()
                    .map(|q| q[1])
                    .fold(f32::MIN, f32::max)
                    .ceil()
                    .min(PX as f32 - 1.0) as usize,
            );
            let area = (p[1][0] - p[0][0]) * (p[2][1] - p[0][1])
                - (p[2][0] - p[0][0]) * (p[1][1] - p[0][1]);
            if area.abs() < 1e-9 {
                continue;
            }
            for y in y0..=y1 {
                for x in x0..=x1 {
                    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                    let w0 = ((p[1][0] - p[0][0]) * (py - p[0][1])
                        - (px - p[0][0]) * (p[1][1] - p[0][1]))
                        / area;
                    let w1 = ((px - p[0][0]) * (p[2][1] - p[0][1])
                        - (p[2][0] - p[0][0]) * (py - p[0][1]))
                        / area;
                    if w0 >= 0.0 && w1 >= 0.0 && w0 + w1 <= 1.0 {
                        count[y * PX + x] += 1;
                        own[y * PX + x] += 1;
                    }
                }
            }
        }
        let covered = own.iter().filter(|c| **c > 0).count();
        let doubled = own.iter().filter(|c| **c > 1).count();
        println!(
            "  {:?}: {covered} px, {doubled} drawn twice by its own class ({:.1}%)",
            m.group,
            100.0 * doubled as f32 / covered.max(1) as f32
        );
    }

    let mut rgba = Vec::with_capacity(PX * PX * 4);
    let mut overlapped = 0usize;
    for c in &count {
        let px: [u8; 3] = match c {
            0 => [0x1A, 0x22, 0x1A],
            1 => [0x8A, 0x88, 0x86],
            2 => {
                overlapped += 1;
                [0xFD, 0x92, 0x12]
            }
            _ => {
                overlapped += 1;
                [0xE0, 0x20, 0x20]
            }
        };
        rgba.extend_from_slice(&[px[0], px[1], px[2], 0xFF]);
    }
    let covered = count.iter().filter(|c| **c > 0).count();
    println!(
        "ribbon pixels {covered}, of which overlapped {overlapped} ({:.1}%)",
        100.0 * overlapped as f32 / covered.max(1) as f32
    );
    image::save_buffer(
        "/tmp/ribbons.png",
        &rgba,
        PX as u32,
        PX as u32,
        image::ExtendedColorType::Rgba8,
    )
    .expect("write png");
    println!("wrote /tmp/ribbons.png");

    // …and the same window again, coloured by **what** covers each pixel
    // rather than by how many things do. The overlap image cannot answer the
    // question a junction actually raises — *is the kerb running across the
    // opening of the street that joins here?* — because a kerb crossing a
    // carriageway is a partition either way and shows up as one clean pixel
    // in both. This one draws the carriageways pale and the kerb bands dark,
    // so a opening is either open tarmac or a bar across it.
    let mut kind = vec![0u8; PX * PX];
    for m in &meshes {
        // A footway is drawn apart from a carriageway, because it is not one
        // — it does not break a kerb, and reading it as tarmac makes a kerb
        // running correctly along the edge of a pavement look like a line
        // ruled down the middle of a road.
        let tone = match m.group {
            SurfaceGroup::RoadCasing(_) => 2u8,
            SurfaceGroup::Road(RoadClass::Path) => 3,
            SurfaceGroup::Road(_) => 1,
            _ => continue,
        };
        for tri in m.indices.chunks_exact(3) {
            let p: Vec<[f32; 2]> = tri
                .iter()
                .map(|i| {
                    let v = m.positions[*i as usize];
                    [(v[0] - wx) / ws * PX as f32, (v[2] - wz) / ws * PX as f32]
                })
                .collect();
            let area = (p[1][0] - p[0][0]) * (p[2][1] - p[0][1])
                - (p[2][0] - p[0][0]) * (p[1][1] - p[0][1]);
            if area.abs() < 1e-9 {
                continue;
            }
            let lo = |f: &dyn Fn(&[f32; 2]) -> f32| {
                p.iter().map(f).fold(f32::MAX, f32::min).floor().max(0.0) as usize
            };
            let hi = |f: &dyn Fn(&[f32; 2]) -> f32| {
                p.iter()
                    .map(f)
                    .fold(f32::MIN, f32::max)
                    .ceil()
                    .min(PX as f32 - 1.0) as usize
            };
            for y in lo(&|q| q[1])..=hi(&|q| q[1]) {
                for x in lo(&|q| q[0])..=hi(&|q| q[0]) {
                    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                    let s = |a: &[f32; 2], b: &[f32; 2]| {
                        (b[0] - a[0]) * (py - a[1]) - (b[1] - a[1]) * (px - a[0])
                    };
                    let (d0, d1, d2) = (s(&p[0], &p[1]), s(&p[1], &p[2]), s(&p[2], &p[0]));
                    if (d0 >= 0.0 && d1 >= 0.0 && d2 >= 0.0)
                        || (d0 <= 0.0 && d1 <= 0.0 && d2 <= 0.0)
                    {
                        kind[y * PX + x] = tone;
                    }
                }
            }
        }
    }
    let kerb = kind.iter().filter(|k| **k == 2).count();
    let tarmac = kind.iter().filter(|k| **k == 1).count();
    println!("carriageway {tarmac} px, kerb band {kerb} px");

    // Two numbers the overlap image cannot give, both of which are what a
    // person means by "the joints look wrong":
    //
    // * **kerb over tarmac** — a kerb band surviving on ground a carriageway
    //   also claims. After `cut_band` the only legitimate case is a bridge,
    //   which is cut against bridges alone.
    // * **a crack** — bare ground with road on both sides of it, i.e. a
    //   hairline the cut opened between two pieces that should abut.
    let mut road_cov = vec![false; PX * PX];
    let mut kerb_cov = vec![false; PX * PX];
    for m in &meshes {
        let sink = match m.group {
            SurfaceGroup::RoadCasing(_) => &mut kerb_cov,
            SurfaceGroup::Road(_) => &mut road_cov,
            _ => continue,
        };
        for tri in m.indices.chunks_exact(3) {
            let p: Vec<[f32; 2]> = tri
                .iter()
                .map(|i| {
                    let v = m.positions[*i as usize];
                    [(v[0] - wx) / ws * PX as f32, (v[2] - wz) / ws * PX as f32]
                })
                .collect();
            let area = (p[1][0] - p[0][0]) * (p[2][1] - p[0][1])
                - (p[2][0] - p[0][0]) * (p[1][1] - p[0][1]);
            if area.abs() < 1e-9 {
                continue;
            }
            let lo = |f: &dyn Fn(&[f32; 2]) -> f32| {
                p.iter().map(f).fold(f32::MAX, f32::min).floor().max(0.0) as usize
            };
            let hi = |f: &dyn Fn(&[f32; 2]) -> f32| {
                p.iter()
                    .map(f)
                    .fold(f32::MIN, f32::max)
                    .ceil()
                    .min(PX as f32 - 1.0) as usize
            };
            for y in lo(&|q| q[1])..=hi(&|q| q[1]) {
                for x in lo(&|q| q[0])..=hi(&|q| q[0]) {
                    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                    let s = |a: &[f32; 2], b: &[f32; 2]| {
                        (b[0] - a[0]) * (py - a[1]) - (b[1] - a[1]) * (px - a[0])
                    };
                    let (d0, d1, d2) = (s(&p[0], &p[1]), s(&p[1], &p[2]), s(&p[2], &p[0]));
                    if (d0 >= 0.0 && d1 >= 0.0 && d2 >= 0.0)
                        || (d0 <= 0.0 && d1 <= 0.0 && d2 <= 0.0)
                    {
                        sink[y * PX + x] = true;
                    }
                }
            }
        }
    }
    let both = road_cov
        .iter()
        .zip(kerb_cov.iter())
        .filter(|(r, k)| **r && **k)
        .count();
    let covered_any = road_cov
        .iter()
        .zip(kerb_cov.iter())
        .filter(|(r, k)| **r || **k)
        .count();
    let any = |i: usize| road_cov[i] || kerb_cov[i];
    let mut cracks = 0usize;
    let mut crack_at = vec![false; PX * PX];
    for y in 1..PX - 1 {
        for x in 1..PX - 1 {
            let i = y * PX + x;
            if any(i) {
                continue;
            }
            let horiz = any(i - 1) && any(i + 1);
            let vert = any(i - PX) && any(i + PX);
            if horiz || vert {
                cracks += 1;
                crack_at[i] = true;
            }
        }
    }
    println!(
        "kerb over tarmac {both} px ({:.2} % of the sheet); one-pixel cracks {cracks}",
        100.0 * both as f32 / covered_any.max(1) as f32,
    );
    let mut rgba = Vec::with_capacity(PX * PX * 4);
    for (i, k) in kind.iter().enumerate() {
        // A crack is painted in the brand's accent orange so it is findable
        // at a glance: a one-pixel hairline in a grey-on-grey image is not.
        let px: [u8; 3] = if crack_at[i] {
            [0xFD, 0x60, 0x12]
        } else {
            match k {
                1 => [0xB4, 0xB1, 0xAC],
                2 => [0x33, 0x3A, 0x40],
                3 => [0x6E, 0x6A, 0x63],
                _ => [0x1A, 0x22, 0x1A],
            }
        };
        rgba.extend_from_slice(&[px[0], px[1], px[2], 0xFF]);
    }
    image::save_buffer(
        "/tmp/ribbon_kinds.png",
        &rgba,
        PX as u32,
        PX as u32,
        image::ExtendedColorType::Rgba8,
    )
    .expect("write png");
    println!("wrote /tmp/ribbon_kinds.png");
}

/// A bridge's span leaves the ground sheet and arrives on the deck — it is
/// not *lost*. Getting that backwards is what drew every viaduct as a bare
/// grey slab with the markings stopping at its edge.
///
/// The road *under* a flyover keeps every metre of its own ribbon, which is
/// what the "only a little of it" bound below is watching.
#[test]
fn a_deck_carries_the_street_rather_than_replacing_it() {
    let decks = tessellate_bridges(GRONINGEN_Z14, 1000.0).expect("bridges");
    let bare = tessellate_roads(GRONINGEN_Z14, 1000.0, &[]).expect("roads");
    let (roads, _) = roads_and_decks(GRONINGEN_Z14, 1000.0, &decks).expect("roads");
    let count = |ms: &[SurfaceMesh]| ms.iter().map(|m| m.indices.len()).sum::<usize>();
    assert!(
        count(&roads.ground) < count(&bare),
        "the decks took no ribbon off the ground at all"
    );
    // …but only a little of it: the tile's bridges are a handful of spans, not
    // a fifth of its streets. A big drop here means ordinary roads are being
    // eaten by a deck they merely pass under.
    let lifted = 1.0 - count(&roads.ground) as f32 / count(&bare) as f32;
    assert!(
        lifted < 0.1,
        "decks took {:.0}% of the road mesh off the ground",
        lifted * 100.0
    );
    // And every metre of it is on a deck, drawn as a road: a `Road` group with
    // its texture coordinates intact is the whole point — a deck that carried
    // only casings, or nothing, is the old bare slab wearing a new type.
    let on_deck: usize = roads
        .on_deck
        .iter()
        .filter(|(_, m)| matches!(m.group, SurfaceGroup::Road(_)))
        .map(|(_, m)| m.indices.len())
        .sum();
    assert!(on_deck > 0, "no carriageway rides any deck");
    assert!(
        roads
            .on_deck
            .iter()
            .all(|(_, m)| m.uvs.len() == m.positions.len()),
        "an on-deck ribbon lost its texture coordinates"
    );
}

/// The split lands on the deck's edge, not on the nearest vertex — a stretch
/// of road belonging to neither piece is exactly the gap the slab used to
/// hide.
#[test]
fn a_street_is_split_at_the_deck_edge_not_at_a_vertex() {
    // A 20 m square deck at the origin, and a street crossing it in two long
    // segments whose vertices are nowhere near its edges.
    let ring = vec![
        [-10.0, -10.0],
        [10.0, -10.0],
        [10.0, 10.0],
        [-10.0, 10.0],
        [-10.0, -10.0],
    ];
    let deck = BridgeDeck {
        positions: Vec::new(),
        indices: Vec::new(),
        rings: vec![ring],
        min: [-10.0, -10.0],
        max: [10.0, 10.0],
    };
    let line = [[-60.0, 0.0], [0.0, 0.0], [60.0, 0.0]];
    let runs = split_by_deck(&line, std::slice::from_ref(&deck));
    let owners: Vec<Option<usize>> = runs.iter().map(|(o, _)| *o).collect();
    assert_eq!(owners, vec![None, Some(0), None], "on, off, on");
    // Each seam within a centimetre of x = ±10, and shared exactly by the two
    // runs that meet there.
    assert!((runs[0].1.last().unwrap()[0] + 10.0).abs() < 0.01);
    assert_eq!(runs[0].1.last(), runs[1].1.first());
    assert!((runs[1].1.last().unwrap()[0] - 10.0).abs() < 0.01);
    assert_eq!(runs[1].1.last(), runs[2].1.first());
    // A street that never meets the deck comes back in one piece.
    let clear = split_by_deck(&[[-60.0, 40.0], [60.0, 40.0]], std::slice::from_ref(&deck));
    assert_eq!(clear.len(), 1);
    assert_eq!(clear[0].0, None);
}

/// The real tile must produce ribbons for several classes, and every one of
/// them must be a well-formed textured mesh.
#[test]
fn roads_tessellate_into_textured_ribbons() {
    let meshes = tessellate_roads(GRONINGEN_Z14, 1000.0, &[]).expect("tessellate");
    assert!(
        meshes.len() >= 3,
        "a city tile should yield several road classes, got {}",
        meshes.len()
    );
    assert!(
        meshes
            .iter()
            .any(|m| matches!(m.group, SurfaceGroup::RoadCasing(_))),
        "kerbed classes must emit their casing band"
    );
    for m in &meshes {
        assert!(
            matches!(m.group, SurfaceGroup::Road(_) | SurfaceGroup::RoadCasing(_)),
            "{:?}",
            m.group
        );
        assert_eq!(
            m.uvs.len(),
            m.positions.len(),
            "every road vertex needs a UV or the markings cannot register"
        );
        assert!(m.indices.len() % 3 == 0 && !m.indices.is_empty());
        assert!(
            m.indices.iter().all(|i| (*i as usize) < m.positions.len()),
            "index out of range"
        );
        // `u` is the across-road axis and must span the full carriageway.
        assert!(m.uvs.iter().any(|uv| uv[0] == 0.0));
        assert!(m.uvs.iter().any(|uv| uv[0] == 1.0));
        // Ground geometry: exactly flat — the anti-z-fight ladder is a depth
        // offset now and takes no height at all — and inside the tile plus
        // the junction stubs.
        assert!(m.positions.iter().all(|p| p[1] == 0.0));
        assert_eq!(
            m.rungs.len(),
            m.positions.len(),
            "every road vertex needs a ladder rung or it cannot be ordered"
        );
        assert!(m
            .positions
            .iter()
            .all(|p| p[0] > -40.0 && p[0] < 1040.0 && p[2] > -40.0 && p[2] < 1040.0));
    }
}
