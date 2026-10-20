//! Road tessellation: a tile's streets layer into ribbons on the depth ladder, cut against
//! each other and the ground, split where they ride a bridge deck, with the approaches that
//! ramp up to one.

use super::*;

/// Tessellate a tile's `streets` layer into textured road ribbons, one mesh per
/// [`RoadClass`].
///
/// The companion to [`tessellate_surfaces`]: same tile bytes, same tile-local
/// metre convention, same one-mesh-per-group batching. Roads are lines rather
/// than areas, so the work is different in three ways:
///
/// * **Width is real.** [`road_width_m`] is metres of carriageway, not the raster
///   path's exaggerated stroke weight.
/// * **Lines must be clipped, not clamped.** [`tessellate_polygon`] can flatten a
///   polygon's overhang against the tile border because the neighbour continues
///   the same surface; doing that to a line would drag a road along the seam.
///   [`clip_polyline`] cuts instead, so the neighbouring tile's copy of the same
///   street meets it edge to edge.
/// * **UVs matter.** They are what puts the centre line down the centre.
/// * **Ends have to know what they meet.** A ribbon that simply stops leaves a
///   notch at every junction; one that always overshoots leaves a flap at every
///   dead end. [`street_nodes`] is the difference between the two.
///
/// `decks` are the tile's [`BridgeDeck`]s, and a bridge-flagged street that runs
/// over one **keeps its ribbon** — it comes back in
/// [`RoadTessellation::on_deck`], tagged with the deck, for the caller to lift to
/// that deck's measured height. It used to be dropped instead, on the theory that
/// "the deck is the road surface up there", and the theory does not survive being
/// looked at: a deck is a flat untextured slab, so every viaduct and every canal
/// bridge was a bare grey polygon with the lane markings, the tarmac and the kerb
/// stopping dead at its edge and starting again on the far side. The deck is the
/// *structure*; the road on it is a road.
///
/// So the carriageway is cut **out of** the deck (`decks` is taken by `&mut` for
/// exactly this), which is why the two can share a plane without a depth fight:
/// the same partition rule [`crate::road_stitch`] applies to the ground sheet,
/// applied to the deck. What is left of the slab is the verge and the parapet
/// either side of the road — which is what a bridge looks like from it.
///
/// Only `bridge` features are moved onto a deck, so the road *under* a viaduct
/// keeps every metre of its own ground-level ribbon.
pub fn tessellate_roads(
    tile: &TileData,
    tile_m: f32,
    decks: &mut [BridgeDeck],
    max_bytes: Option<usize>,
    surveyed_paving: bool,
) -> Result<RoadTessellation, String> {
    let cover = if surveyed_paving {
        SurveyedCover::Paths
    } else {
        SurveyedCover::Nothing
    };
    tessellate_roads_surveyed(tile, tile_m, decks, max_bytes, cover)
}

/// [`tessellate_roads`], told how much of the cell the survey draws
/// ([`SurveyedCover`]) so it can leave those ribbons out.
pub fn tessellate_roads_surveyed(
    tile: &TileData,
    tile_m: f32,
    decks: &mut [BridgeDeck],
    max_bytes: Option<usize>,
    cover: SurveyedCover,
) -> Result<RoadTessellation, String> {
    let Some(meta) = tile.layer("streets") else {
        return Ok(RoadTessellation::default());
    };
    let features = tile.features(meta);
    let extent = TileData::extent(meta);
    let scale = tile_m / extent;
    let nodes = street_nodes(&features, scale);
    // The tunnel-flagged ways that are really arcades through a building: they are
    // on the ground and are drawn here, while the rest are tunnels and belong to
    // `crate::tunnels`, which draws them under it.
    let passages = crate::tunnels::building_passages(tile, &features);
    // Rungs for the lines that could otherwise land coplanar, decided across the
    // whole tile before any of them is built — see `assign_road_ties`.
    let ties = assign_road_ties(&features, scale);

    // (group, positions, uvs, indices, rungs) — see `SurfaceMesh::rungs`.
    type RoadBuilder = (
        SurfaceGroup,
        Vec<[f32; 3]>,
        Vec<[f32; 2]>,
        Vec<u32>,
        Vec<f32>,
    );
    let mut builders: Vec<RoadBuilder> = Vec::new();
    // Groups cut loose mid-tile, over the frame's upload budget. See the sweep in
    // the emit loop at the bottom.
    let mut full: Vec<SurfaceMesh> = Vec::new();
    let slot_for = |builders: &mut Vec<RoadBuilder>, group: SurfaceGroup| match builders
        .iter()
        .position(|(g, ..)| *g == group)
    {
        Some(i) => i,
        None => {
            builders.push((group, Vec::new(), Vec::new(), Vec::new(), Vec::new()));
            builders.len() - 1
        }
    };
    // Every ribbon is laid into a buffer of its own first, because the thing that
    // settles a junction now is `road_stitch`, and it can only cut a ribbon it can
    // still tell apart from its neighbours. They are merged into the per-group
    // meshes after the cut.
    let mut staged: Vec<crate::road_stitch::Ribbon> = Vec::new();
    // …and the stretches that ride a deck, one bucket per deck. They are a
    // separate sheet at a separate height, so they are cut among themselves and
    // against their own deck, never against the ground.
    let mut staged_on_deck: Vec<Vec<crate::road_stitch::Ribbon>> =
        (0..decks.len()).map(|_| Vec::new()).collect();
    // The ground stretches that run up to a deck — see [`Approach`].
    let mut approaches: Vec<Approach> = Vec::new();
    // Where a bridge-flagged street ends *inside* a deck's outline, metres: the
    // street that carries on from there is still on the structure
    // ([`carried_onto_deck`]).
    let bridge_ends: Vec<(usize, [f32; 2])> = if decks.is_empty() {
        Vec::new()
    } else {
        features
            .iter()
            .filter(|f| flag_of(f, "bridge") && drawn_street(f).is_some())
            .flat_map(|f| match &f.geometry {
                Geometry::LineString(ls) => vec![ls],
                Geometry::MultiLineString(mls) => mls.iter().collect(),
                _ => Vec::new(),
            })
            .flat_map(|ls| [ls.0.first().copied(), ls.0.last().copied()])
            .flatten()
            .map(|c| [c.x * scale, c.y * scale])
            .filter_map(|p| decks.iter().position(|d| d.contains(p)).map(|d| (d, p)))
            .collect()
    };

    for (fi, feature) in features.iter().enumerate() {
        // A tunnel is refused by `drawn_street` because it is not on the ground —
        // unless it is one of the arcades, which are.
        let shape = match drawn_street(feature) {
            Some(shape) => Some(shape),
            None if passages.contains(&fi) => street_shape(feature),
            None => None,
        };
        let Some((class, half)) = shape else {
            continue;
        };
        // Over a cell whose paving was surveyed, the footways, cycle paths and
        // pedestrian streets are drawn from that survey as real shapes
        // (`crate::paving`), so the constant-width ribbon for the same ground must
        // not also be drawn. The test is on the *kind*, not on the class: a
        // pedestrian street is a `RoadClass::Minor`, so a class-level rule left it
        // paving squares in tarmac. Everything else on the vehicle network stays
        // with OSM, marked and cased, everywhere.
        if cover.supersedes(kind_of(feature).unwrap_or("")) {
            continue;
        }
        let bridge = flag_of(feature, "bridge");
        // Traffic runs against the way's own direction, so the across-road axis
        // has to be mirrored before the (asymmetric) carriageway texture lands on
        // it. See the flip at the ribbon below.
        let reversed = flag_of(feature, "oneway_reverse");
        // Kerbed classes get a casing ribbon under the fill; paths and rails
        // have no kerb to draw.
        let cased = !matches!(class, RoadClass::Path | RoadClass::Rail);

        let mut lines: Vec<&geo_types::LineString<f32>> = Vec::new();
        match &feature.geometry {
            Geometry::LineString(ls) => lines.push(ls),
            Geometry::MultiLineString(mls) => lines.extend(mls.iter()),
            // The `streets` layer is lines; a stray polygon belongs to
            // `street_polygons`, which the raster path fills.
            _ => continue,
        }

        for (li, ls) in lines.into_iter().enumerate() {
            let pts: Vec<[f32; 2]> =
                ls.0.iter()
                    .filter(|c| c.x.is_finite() && c.y.is_finite())
                    .map(|c| [c.x, c.y])
                    .collect();
            let (Some(head), Some(tail)) = (pts.first().copied(), pts.last().copied()) else {
                continue;
            };
            // A roundabout, turning circle or loop road closes on itself. Its
            // start and end are one node that the road runs *through*, so it
            // takes no junction stub — an extension there fires a flap of
            // tarmac off along the tangent — and its ribbon closes instead of
            // meeting itself with two square ends.
            let closed = pts.len() >= 4 && (head[0] - tail[0]).hypot(head[1] - tail[1]) <= 1e-4;
            // The tie on the anti-z-fight ladder (see `road_rung`) — per
            // *linestring*, so the crossing parts of one merged feature separate.
            let tie = ties
                .get(&(fi, li))
                .copied()
                .map_or_else(|| line_tie_seed(head, tail), u64::from);
            // The rung is a *depth* offset, not a height: the ribbon itself lies
            // flat on the road plane and `systems::road_depth` biases it in the
            // vertex stage. Lifting it geometrically is what used to stop working
            // past a couple of hundred metres — see `road_rung`.
            let rung = road_rung(class, half * 2.0, bridge, tie) as f32;
            // The casing's own rung, at the foot of the same class block rather
            // than on a separate plane under every fill in the tile.
            let casing_rung = road_casing_rung(class, bridge) as f32;
            // The line's outward direction at each end (tile units — uniform
            // scale, so angles match the metre frame), for the crossing-angle
            // term in `junction_extent`.
            let out_dir = |from: [f32; 2], to: [f32; 2]| -> [f32; 2] {
                let (dx, dy) = (to[0] - from[0], to[1] - from[1]);
                let len = dx.hypot(dy);
                if len > 1e-6 {
                    [dx / len, dy / len]
                } else {
                    [0.0, 0.0]
                }
            };
            let head_out = (pts.len() >= 2).then(|| out_dir(pts[1], pts[0]));
            let tail_out = (pts.len() >= 2).then(|| out_dir(pts[pts.len() - 2], tail));
            for run in clip_polyline(&pts, 0.0, extent) {
                // Only the run holding an *original* end of the line can be at a
                // junction; the rest begin and end at the tile border, where the
                // neighbour's copy of the same street abuts this one exactly and
                // an extension would draw the same tarmac twice.
                //
                // A continuation takes precedence over a stub: where the street
                // runs on through the node as another feature there is no notch
                // to fill, and the mitred cap is the join.
                let end_join = |at: [f32; 2], out: Option<Option<[f32; 2]>>| -> EndJoin {
                    let Some(Some(d)) = out else {
                        return EndJoin::default();
                    };
                    match continuation(&nodes, at, fi, d, class, half) {
                        Some(cont) => EndJoin {
                            extend: 0.0,
                            cont: Some(cont),
                        },
                        None => EndJoin {
                            extend: junction_extent(&nodes, at, fi, d),
                            cont: None,
                        },
                    }
                };
                let join_head = end_join(
                    head,
                    (!closed && run.first() == Some(&head)).then_some(head_out),
                );
                let join_tail = end_join(
                    tail,
                    (!closed && run.last() == Some(&tail)).then_some(tail_out),
                );
                // Only an *unclipped* run is still a ring; a clipped one is an
                // ordinary open polyline that happens to come from a loop.
                let run_is_ring = closed && run.len() == pts.len();
                // Tile units → metres, once, before any geometry is built.
                let metres: Vec<[f32; 2]> =
                    run.iter().map(|p| [p[0] * scale, p[1] * scale]).collect();
                // A bridge's span is carried *by* the deck: the same ribbon, at the
                // deck's height. `split_by_deck` says which stretches those are.
                let parts = if decks.is_empty() {
                    vec![(None, metres)]
                } else if bridge {
                    split_by_deck(&metres, decks)
                } else {
                    carried_onto_deck(metres, decks, &bridge_ends)
                };
                // A run that was split at a deck edge now ends *at* the deck, which
                // is not a junction — extending there would push tarmac out past
                // the structure.
                let whole = parts.len() == 1 && parts[0].1.len() == run.len();
                // **The approaches, before `parts` is consumed.** A ground run
                // that neighbours a deck run *in the same street* is the road that
                // climbs onto that structure — which is the one fact nothing
                // downstream could recover, and the reason a raster ramp leaked
                // into every street near a bridge. `split_by_deck` has always
                // known it and only the deck tag survived.
                //
                // Ordered abutment-first, because the ramp is measured from the
                // deck outwards and a caller cannot tell which end of a run met
                // the structure by looking at it.
                for (pi, (on_deck, part)) in parts.iter().enumerate() {
                    // A way that ends on its deck's edge leaves a stretch of
                    // "ground" as long as the rounding. It points nowhere, so it
                    // cannot be followed down the road, and standing in as the
                    // approach it stopped `extend_approaches` from seeding the
                    // real one: the Europaplein's west ramp went unfound.
                    //
                    // Only a bridge-flagged way's: a street carried onto a deck
                    // from a bridge end (`carried_onto_deck`) is found from that
                    // end by `extend_approaches`, which bounds the walk; its own
                    // ground stretch runs as far as the street does.
                    if !bridge
                        || on_deck.is_some()
                        || part.len() < 2
                        || crate::tunnels::polyline_len(part) < MIN_APPROACH_M
                    {
                        continue;
                    }
                    // The deck is behind this run if the previous part is on one,
                    // and ahead of it if the next part is. Both is a short ground
                    // stretch between two spans, which is an approach to each.
                    let before = pi.checked_sub(1).and_then(|j| parts[j].0);
                    let after = parts.get(pi + 1).and_then(|p| p.0);
                    for (deck, reversed) in [(before, false), (after, true)] {
                        let Some(d) = deck else { continue };
                        let mut points = part.clone();
                        // Abutment first: a run whose deck is *behind* it already
                        // starts there, and one whose deck is ahead ends there.
                        if reversed {
                            points.reverse();
                        }
                        #[allow(clippy::cast_possible_truncation)]
                        approaches.push(Approach {
                            deck: d as u32,
                            points,
                        });
                    }
                }
                for (pi, (on_deck, part)) in parts.into_iter().enumerate() {
                    let ends = if whole {
                        [join_head, join_tail]
                    } else {
                        [EndJoin::default(); 2]
                    };
                    let lay = |sink: &mut Vec<crate::road_stitch::Ribbon>,
                               group: SurfaceGroup,
                               rung: f32,
                               priority: u32,
                               positions: Vec<[f32; 3]>,
                               uvs: Vec<[f32; 2]>,
                               indices: Vec<u32>| {
                        if indices.is_empty() {
                            return;
                        }
                        sink.push(crate::road_stitch::Ribbon {
                            group,
                            rung,
                            // `cut_band` separates the carriageways from the kerb
                            // bands and `road_rung` is the road hierarchy within
                            // them; the rest only has to make the order total, so
                            // that two ribbons which tie on the hierarchy still
                            // have a definite winner rather than a draw the
                            // renderer would have to break.
                            priority: (cut_band(group), priority, fi, li, pi),
                            bridge,
                            positions,
                            uvs,
                            // A road's tone is in its texture, not its vertices.
                            colors: Vec::new(),
                            indices,
                        });
                    };
                    // A stretch on a deck is staged apart from the ground sheet:
                    // it is cut against that deck and against the other ribbons
                    // riding it, never against the roads passing underneath, and
                    // it is lifted with the deck by the caller.
                    let sink: &mut Vec<crate::road_stitch::Ribbon> = match on_deck {
                        Some(d) => &mut staged_on_deck[d],
                        None => &mut staged,
                    };
                    let mut p = Vec::new();
                    let mut u = Vec::new();
                    let mut i = Vec::new();
                    ribbon(
                        &part,
                        half,
                        0.0,
                        class.texture_length_m(),
                        ends,
                        run_is_ring && whole,
                        &mut p,
                        &mut u,
                        &mut i,
                    );
                    // **Put the across-road axis the way the traffic faces.**
                    //
                    // `ribbon` runs `u` from the way's left to its right, which is
                    // the traffic's left-to-right only while the two agree. On an
                    // `oneway_reverse` feature they are opposite, and the
                    // carriageway texture is no longer symmetric: it carries a hard
                    // shoulder on the right. Unflipped, that shoulder lands against
                    // the median on about half the features, which reads as a
                    // texture bug rather than as a direction one.
                    //
                    // Mirroring `u` rather than reversing the geometry keeps every
                    // vertex, index, join and kerb zip exactly where it was — the
                    // texture is symmetric in `v` and the markings are bands, so
                    // there is nothing else across the road for this to disturb.
                    if reversed {
                        for uv in &mut u {
                            uv[0] = 1.0 - uv[0];
                        }
                    }
                    lay(
                        &mut *sink,
                        SurfaceGroup::Road(class),
                        road_depth_rung(bridge),
                        rung as u32,
                        p,
                        u,
                        i,
                    );
                    // No kerb band on a deck: what lies either side of the
                    // carriageway up there is the structure — its verge and its
                    // parapet — and that is the part of the deck the carriageway
                    // is cut out of, not a band this has to draw.
                    if cased && on_deck.is_none() {
                        // The kerb wraps the fill's stub ends too, so the band
                        // runs around the junction opening instead of stopping
                        // square at it. A mitred end needs no such treatment —
                        // the neighbour's kerb is mitred to the same bisector and
                        // meets this one exactly.
                        let ext = ends.map(|e| EndJoin {
                            extend: if e.extend > 0.0 {
                                e.extend + ROAD_CASING_M
                            } else {
                                0.0
                            },
                            cont: e.cont,
                        });
                        let mut p = Vec::new();
                        let mut u = Vec::new();
                        let mut i = Vec::new();
                        kerb_band(
                            &part,
                            half,
                            class.texture_length_m(),
                            ends,
                            ext,
                            run_is_ring && whole,
                            &mut p,
                            &mut u,
                            &mut i,
                        );
                        lay(
                            &mut *sink,
                            SurfaceGroup::RoadCasing(class),
                            road_depth_rung(bridge),
                            casing_rung as u32,
                            p,
                            u,
                            i,
                        );
                    }
                }
            }
        }
    }

    // Cut every ribbon back to the ground no higher-priority ribbon claims. After
    // this the road sheet is a partition: no two ribbons cover one patch, so
    // nothing is left for the depth ladder to arbitrate at ground level.
    crate::road_stitch::stitch(&mut staged);

    // Each deck settles its own little sheet: the ribbons riding it are cut
    // against each other, and the slab is cut against all of them. The deck goes
    // in as the lowest-priority ribbon in that cut, so it loses every patch of
    // ground a carriageway claims and keeps the rest — the verge and the parapet.
    let mut on_deck: Vec<(u32, SurfaceMesh)> = Vec::new();
    for (di, ribbons) in staged_on_deck.iter_mut().enumerate() {
        if ribbons.is_empty() {
            continue;
        }
        let deck = &mut decks[di];
        // Every road on the deck is lifted one *band* so that the deck can sit at
        // the very bottom of the order and lose to all of them. Shifting the whole
        // set by one keeps their order among themselves exactly as `cut_band` and
        // `road_rung` decided it — and it is the only way to be *strictly* below a
        // hierarchy whose own floor is zero.
        for r in ribbons.iter_mut() {
            r.priority.0 += 1;
        }
        let mut sheet = Vec::with_capacity(ribbons.len() + 1);
        sheet.push(crate::road_stitch::Ribbon {
            group: SurfaceGroup::BridgeDeck,
            rung: 0.0,
            // The weakest claim there is: `stitch` gives the ground to the
            // *larger* priority, so this yields to every road above.
            priority: (0, 0, 0, 0, 0),
            // The deck and the ribbons on it are one sheet, and `stitch` only
            // lets a bridge be cut by another bridge — which is exactly the
            // isolation this wants from the roads passing underneath.
            bridge: true,
            // The slab is flat-coloured and untextured, and both attribute sets
            // are optional — `stitch` carries across only what a ribbon actually
            // has.
            uvs: Vec::new(),
            colors: Vec::new(),
            positions: std::mem::take(&mut deck.positions),
            indices: std::mem::take(&mut deck.indices),
        });
        sheet.append(ribbons);
        crate::road_stitch::stitch(&mut sheet);
        let mut sheet = sheet.into_iter();
        let carved = sheet.next().expect("the deck was pushed first");
        deck.positions = carved.positions;
        deck.indices = carved.indices;

        let mut deck_builders: Vec<RoadBuilder> = Vec::new();
        for r in sheet {
            let slot = slot_for(&mut deck_builders, r.group);
            let (_, positions, uvs, indices, rungs) = &mut deck_builders[slot];
            let base = positions.len() as u32;
            positions.extend(r.positions);
            uvs.extend(r.uvs);
            indices.extend(r.indices.iter().map(|i| i + base));
            rungs.resize(positions.len(), r.rung);
        }
        for (group, positions, uvs, indices, rungs) in deck_builders {
            if positions.is_empty() || indices.is_empty() {
                continue;
            }
            on_deck.push((
                di as u32,
                SurfaceMesh {
                    group,
                    positions,
                    uvs,
                    indices,
                    normals: Vec::new(),
                    colors: Vec::new(),
                    rungs,
                },
            ));
        }
    }

    for r in staged {
        let slot = slot_for(&mut builders, r.group);
        let (_, positions, uvs, indices, rungs) = &mut builders[slot];
        let base = positions.len() as u32;
        positions.extend(r.positions);
        uvs.extend(r.uvs);
        indices.extend(r.indices.iter().map(|i| i + base));
        // One rung per vertex this ribbon just appended, leaving every earlier
        // one alone.
        rungs.resize(positions.len(), r.rung);

        // The cut is after the ribbon, not before it — its vertex count is not
        // known until it has been laid — so the bound is "the ceiling plus at most
        // one ribbon", which is what `scatter`'s test asserts. A road cell over
        // Groningen was 4.2 MB in one mesh (2026-08-19).
        if let Some(max) = max_bytes {
            if mesh_upload_bytes(positions.len(), indices.len()) >= max {
                full.push(SurfaceMesh {
                    group: r.group,
                    positions: std::mem::take(positions),
                    uvs: std::mem::take(uvs),
                    indices: std::mem::take(indices),
                    normals: Vec::new(),
                    colors: Vec::new(),
                    rungs: std::mem::take(rungs),
                });
            }
        }
    }

    Ok(RoadTessellation {
        ground: full
            .into_iter()
            .chain(
                builders
                    .into_iter()
                    .filter(|(_, p, _, i, _)| !p.is_empty() && !i.is_empty())
                    .map(|(group, positions, uvs, indices, rungs)| SurfaceMesh {
                        group,
                        positions,
                        uvs,
                        indices,
                        normals: Vec::new(),
                        colors: Vec::new(),
                        rungs,
                    }),
            )
            .collect(),
        on_deck,
        approaches: extend_approaches(tile, tile_m, decks, approaches),
    })
}

/// Longest an approach is followed from its abutment, metres: a tall viaduct's
/// clearance over the steepest grade an approach takes
/// (`road_grade::MAX_APPROACH_GRADE`) with room to spare. Past where the
/// embankment meets the ground the ramp raises nothing, so the extra is free.
const APPROACH_REACH_M: f32 = 160.0;

/// Shortest ground stretch of a bridge-flagged way that counts as its approach,
/// metres: below it the stretch is the rounding where the way meets its deck.
const MIN_APPROACH_M: f32 = 0.5;

/// The approaches, carried on along the roads beyond the structure.
///
/// **OSM's bridge way stops at the abutment**, so the only approach
/// `split_by_deck` could find was the sliver of the same way that happened to
/// overhang the deck outline — measured over four Groningen tiles on
/// 2026-09-23, every one 0.0–0.3 m long, and many decks (Laan 1940's real
/// abutments, the Beneluxweg's, the Emmaviaduct's north end) had none at all. So
/// the embankment ended at its grading's six-sample dilation, about twenty metres
/// out, in a cliff: 8.7 m of it at Laan 1940.
///
/// Each approach, and each bridge-flagged street's own end at a deck that no
/// approach starts from, is followed along the street that carries on from it
/// (the same walk `tunnels::find_approaches` does for a tunnel's ramp): through
/// further ways while exactly one continues, up to [`APPROACH_REACH_M`].
fn extend_approaches(
    tile: &TileData,
    tile_m: f32,
    decks: &[BridgeDeck],
    mut approaches: Vec<Approach>,
) -> Vec<Approach> {
    if decks.is_empty() {
        return approaches;
    }
    let lines = crate::tunnels::surface_lines(tile, tile_m);
    let near = |a: [f32; 2], b: [f32; 2], r: f32| (a[0] - b[0]).hypot(a[1] - b[1]) <= r;
    // The bridge ends at a deck that no approach starts from.
    let mut seeds: Vec<(u32, Vec<[f32; 2]>)> = Vec::new();
    for l in lines.iter().filter(|l| l.bridge) {
        for k in 0..2 {
            if !l.own[k] || l.pts.len() < 2 {
                continue;
            }
            let (p, inner) = if k == 0 {
                (l.pts[0], l.pts[1])
            } else {
                (l.pts[l.pts.len() - 1], l.pts[l.pts.len() - 2])
            };
            // On the deck, or just inside it along the way.
            let (dx, dz) = (inner[0] - p[0], inner[1] - p[1]);
            let len = dx.hypot(dz).max(1e-6);
            let probe = [p[0] + dx / len * 2.0, p[1] + dz / len * 2.0];
            let Some(d) = decks
                .iter()
                .position(|deck| deck.contains(p) || deck.contains(probe))
            else {
                continue;
            };
            if approaches
                .iter()
                .any(|a| a.points.first().is_some_and(|q| near(*q, p, 3.0)))
            {
                continue;
            }
            #[allow(clippy::cast_possible_truncation)]
            seeds.push((d as u32, vec![inner, p]));
        }
    }
    // Seeds go after the approaches the tile already had, and are kept only if
    // the walk found a road: each carries one segment pointing out of the deck,
    // whose inner vertex is dropped once it has served as a direction.
    let first_seed = approaches.len();
    for (deck, pts) in seeds {
        approaches.push(Approach { deck, points: pts });
    }
    let mut keep = vec![true; approaches.len()];

    let mut branches: Vec<Approach> = Vec::new();
    for (ai, a) in approaches.iter_mut().enumerate() {
        let n = a.points.len();
        if n < 2 {
            continue;
        }
        let (tail, before) = (a.points[n - 1], a.points[n - 2]);
        let (dx, dz) = (tail[0] - before[0], tail[1] - before[1]);
        let len = dx.hypot(dz);
        if len < 1e-3 {
            continue;
        }
        let mut paths = Vec::new();
        follow_approach(
            &lines,
            tail,
            [dx / len, dz / len],
            &mut Vec::new(),
            0.0,
            Vec::new(),
            &mut paths,
        );
        let seeded = ai >= first_seed;
        if paths.is_empty() {
            // A seed that found no road to follow is not an approach.
            keep[ai] = !seeded;
            continue;
        }
        if seeded {
            a.points.remove(0);
        }
        // **Every branch of a fork is an approach of its own.** An interchange
        // arm splits into slip roads right at the abutment, and each of them
        // leaves the deck at the deck's height; stopping at the fork gave such
        // an arm a few metres of ramp and then a cliff — 7.75 m at Laan
        // 1940–1945's north arm (2026-09-24).
        let mut paths = paths.into_iter();
        let first = paths.next().expect("non-empty");
        for extra in paths {
            let mut points = a.points.clone();
            points.extend(extra);
            branches.push(Approach {
                deck: a.deck,
                points,
            });
        }
        a.points.extend(first);
    }
    let mut k = keep.into_iter();
    approaches.retain(|_| k.next().unwrap_or(true));
    approaches.extend(branches);
    approaches
}

/// Most roads one approach is followed through, and most paths it may branch
/// into at forks — a bound on the walk, not a property of any real junction.
const APPROACH_WAYS: usize = 6;
const APPROACH_BRANCHES: usize = 4;

/// Follow the street carrying on from `here` along `heading`, through further
/// ways, pushing each complete path found into `out` (the points past `here`).
/// Where more than one way carries on — a fork — every one is followed, up to
/// [`APPROACH_BRANCHES`] paths in all.
fn follow_approach(
    lines: &[crate::tunnels::SurfaceLine],
    here: [f32; 2],
    heading: [f32; 2],
    used: &mut Vec<usize>,
    walked: f32,
    so_far: Vec<[f32; 2]>,
    out: &mut Vec<Vec<[f32; 2]>>,
) {
    let near = |a: [f32; 2], b: [f32; 2], r: f32| (a[0] - b[0]).hypot(a[1] - b[1]) <= r;
    let done = |out: &mut Vec<Vec<[f32; 2]>>, so_far: Vec<[f32; 2]>| {
        if !so_far.is_empty() && out.len() < APPROACH_BRANCHES {
            out.push(so_far);
        }
    };
    if used.len() >= APPROACH_WAYS || walked >= APPROACH_REACH_M - 0.5 {
        done(out, so_far);
        return;
    }
    let candidates: Vec<(usize, Vec<[f32; 2]>)> = lines
        .iter()
        .enumerate()
        .filter(|(i, l)| !l.bridge && !used.contains(i))
        .flat_map(|(i, l)| {
            [0usize, 1].into_iter().filter_map(move |k| {
                let end = if k == 0 {
                    l.pts[0]
                } else {
                    l.pts[l.pts.len() - 1]
                };
                if !l.own[k] || !near(end, here, 1.5) {
                    return None;
                }
                let mut pts = l.pts.clone();
                if k == 1 {
                    pts.reverse();
                }
                Some((i, pts))
            })
        })
        .filter(|(_, pts)| {
            crate::tunnels::leaving(pts)
                .is_some_and(|d| d[0] * heading[0] + d[1] * heading[1] >= 0.82)
        })
        .collect();
    if candidates.is_empty() {
        done(out, so_far);
        return;
    }
    for (i, mut pts) in candidates {
        if out.len() >= APPROACH_BRANCHES {
            return;
        }
        pts[0] = here;
        let pts = crate::tunnels::cut_at(&pts, APPROACH_REACH_M - walked);
        let m = pts.len();
        let step = crate::tunnels::polyline_len(&pts);
        let mut next_heading = heading;
        if m >= 2 {
            let (ex, ez) = (pts[m - 1][0] - pts[m - 2][0], pts[m - 1][1] - pts[m - 2][1]);
            let el = ex.hypot(ez).max(1e-6);
            next_heading = [ex / el, ez / el];
        }
        let end = pts[m - 1];
        let mut path = so_far.clone();
        path.extend(pts.into_iter().skip(1));
        used.push(i);
        // A way cut by the tile's edge carries on in the next tile, which walks
        // it from there: stop here.
        if !lines[i].own[0] || !lines[i].own[1] {
            done(out, path);
        } else {
            follow_approach(lines, end, next_heading, used, walked + step, path, out);
        }
        used.pop();
    }
}

/// What [`tessellate_roads`] hands back: the road sheet that lies on the ground,
/// and the stretches that ride a bridge deck.
///
/// Two lists rather than one because the second cannot be placed yet. A deck's
/// height is not in the tile — it is measured from AHN, per deck, some frames
/// later (`systems::map_geometry::resolve_bridge_decks`) — so the ribbons that
/// belong to it come back tagged with its index and at `y = 0`, for the caller to
/// lift with the slab they were cut out of.
#[derive(Debug, Default)]
pub struct RoadTessellation {
    /// Roads at ground level: one mesh per [`SurfaceGroup`], as before.
    pub ground: Vec<SurfaceMesh>,
    /// `(deck index, mesh)` — the index is into the `decks` slice that was passed
    /// in, whose slabs have had these meshes cut out of them.
    pub on_deck: Vec<(u32, SurfaceMesh)>,
    /// The ground stretches that run up to a deck, one per abutment.
    pub approaches: Vec<Approach>,
}

/// A road that climbs onto a structure: the ground stretch of a bridge-flagged
/// street immediately either side of the span, with the abutment first.
///
/// # What this is for, and why nothing else could supply it
///
/// A deck stands at a measured height and the road that reaches it is drawn flat,
/// so a viaduct ends in a vertical step. Grading the ground back down from the
/// abutment fixes that, and the question it immediately raises is *which* ground —
/// measured over the Groningen fixture, ramping through the whole road corridor
/// raised 18 % of the city's streets for five bridges, because a raster mask is
/// one connected shape and a ramp travels through everything touching it.
///
/// The distinction it needs is not a height and not a level: the approach and the
/// street passing underneath are both at grade and both unflagged
/// (`bridge-heights.md` — "the approach roads that climb to it in reality are
/// ordinary unflagged ways"), so neither `level_of` nor the survey can separate
/// them. What separates them is that the approach is **the same street feature as
/// the span**, which [`split_by_deck`] establishes and then used to discard.
///
/// Centre lines rather than ribbons, because the consumer rasterises them into a
/// corridor of its own width and a ribbon would have to be un-widened first.
#[derive(Debug, Clone)]
pub struct Approach {
    /// Index into the `decks` slice this run climbs to.
    pub deck: u32,
    /// The run in tile-local metres, **abutment first**. A caller cannot recover
    /// which end met the structure by looking at the geometry, and the ramp is
    /// measured from the deck outwards.
    pub points: Vec<[f32; 2]>,
}

/// Which deck a point stands on, if any.
fn deck_at(decks: &[BridgeDeck], p: [f32; 2]) -> Option<usize> {
    decks.iter().position(|d| d.contains(p))
}

/// How often [`split_by_deck`] asks which deck a segment is over, metres.
const DECK_PROBE_M: f32 = 1.0;

/// How many times the deck boundary is bisected for. Twelve halvings put the seam
/// within a fifty-metre segment's ten-thousandth — under a centimetre, which is an
/// order of magnitude finer than the millimetre lattice `road_stitch` then snaps
/// everything to.
const DECK_EDGE_BISECTIONS: u32 = 12;

/// Split a centre line into runs, each tagged with the deck it lies on (`None`
/// for the ones on the ground).
///
/// Per **stretch** rather than per feature: Shortbread merges ways by attribute,
/// so one `bridge` feature can be a chain of spans with ordinary road between
/// them, and treating the whole feature as one would put the approach roads up on
/// the structure.
///
/// The split lands on the deck's own edge, not on the nearest vertex. Vertices are
/// where the *street* bends and a deck edge is where the *structure* stops, so the
/// two coincide only by luck: cutting at the vertex left a stretch of road up to a
/// segment long belonging to neither piece, which was invisible only for as long as
/// the deck was drawn as an opaque slab over the top of it. The crossing is found
/// by bisecting on [`BridgeDeck::contains`] rather than by intersecting the ring,
/// so a hole, a self-touching outline and two decks that abut are all one case.
///
/// **Asked along each segment, not only at its vertices.** A segment that enters
/// and leaves a deck between two vertices used to be missed — both its ends
/// agree — on the grounds that a structure narrower than a vertex spacing has no
/// deck. A deck built from a way ([`street_decks`]) can be exactly that narrow
/// against a *different* way crossing it, and a long straight segment with both
/// ends just off a deck is ordinary in a generalised tile. Every segment is probed
/// each [`DECK_PROBE_M`], and a change of owner between two probes is bisected to
/// the edge as before.
/// How close a street's end must be to a bridge-flagged street's end to be the
/// same node, metres — a few tile units at z14.
const BRIDGE_END_JOIN_M: f32 = 0.5;

/// **A street at grade that runs on from a bridge's way inside its outline is
/// still on the bridge**, from that end to where it leaves the deck.
///
/// OSM's bridge way routinely stops short of the structure: the Oosterbrug's
/// ends a few metres inside its outline at the south, and the street that
/// carries on from there was drawn on the ground — cut out of the deck like any
/// road on it, and laid on the canal bed under it, so the slab had a hole of
/// open water across its end (2026-10-19). Only from an end that meets a bridge
/// way inside that deck: a street passing under a viaduct meets no bridge end,
/// and stays on the ground it runs on. `bridge_ends` is every bridge-flagged
/// street's end that lies inside a deck, with that deck.
fn carried_onto_deck(
    points: Vec<[f32; 2]>,
    decks: &[BridgeDeck],
    bridge_ends: &[(usize, [f32; 2])],
) -> Vec<(Option<usize>, Vec<[f32; 2]>)> {
    let joined = |p: Option<&[f32; 2]>| -> Option<usize> {
        let p = p?;
        bridge_ends
            .iter()
            .find(|(_, e)| (e[0] - p[0]).hypot(e[1] - p[1]) <= BRIDGE_END_JOIN_M)
            .map(|(d, _)| *d)
    };
    let (head, tail) = (joined(points.first()), joined(points.last()));
    if head.is_none() && tail.is_none() {
        return vec![(None, points)];
    }
    let parts = split_by_deck(&points, decks);
    let last = parts.len().saturating_sub(1);
    let mut out: Vec<(Option<usize>, Vec<[f32; 2]>)> = Vec::new();
    for (i, (owner, part)) in parts.into_iter().enumerate() {
        let kept = owner.filter(|&d| (i == 0 && head == Some(d)) || (i == last && tail == Some(d)));
        match out.last_mut() {
            // Consecutive ground stretches are one, as they were before the
            // split: a street under a second deck it does not join stays whole.
            Some((None, prev)) if kept.is_none() => prev.extend(part.into_iter().skip(1)),
            _ => out.push((kept, part)),
        }
    }
    out
}

pub(super) fn split_by_deck(
    points: &[[f32; 2]],
    decks: &[BridgeDeck],
) -> Vec<(Option<usize>, Vec<[f32; 2]>)> {
    let mut runs: Vec<(Option<usize>, Vec<[f32; 2]>)> = Vec::new();
    let Some(&first) = points.first() else {
        return runs;
    };
    let mut owner = deck_at(decks, first);
    let mut cur: Vec<[f32; 2]> = vec![first];
    for w in points.windows(2) {
        let (a, b) = (w[0], w[1]);
        let len = (b[0] - a[0]).hypot(b[1] - a[1]);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let steps = (len / DECK_PROBE_M).ceil().max(1.0) as usize;
        let mut prev = a;
        for k in 1..=steps {
            #[allow(clippy::cast_precision_loss)]
            let t = k as f32 / steps as f32;
            let probe = if k == steps {
                b
            } else {
                [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
            };
            let next = deck_at(decks, probe);
            if next != owner {
                // The edge between them, shared exactly by the run that ends on it
                // and the run that starts from it — one computation, so the two
                // cannot land apart.
                let edge = deck_edge_between(decks, prev, owner, probe);
                cur.push(edge);
                if cur.len() >= 2 {
                    runs.push((owner, std::mem::take(&mut cur)));
                } else {
                    cur.clear();
                }
                cur.push(edge);
                owner = next;
            }
            prev = probe;
        }
        cur.push(b);
    }
    if cur.len() >= 2 {
        runs.push((owner, cur));
    }
    runs
}

/// The point on `a`–`b` where the deck underfoot stops being `owner`.
fn deck_edge_between(
    decks: &[BridgeDeck],
    a: [f32; 2],
    owner: Option<usize>,
    b: [f32; 2],
) -> [f32; 2] {
    let at = |t: f32| [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..DECK_EDGE_BISECTIONS {
        let mid = 0.5 * (lo + hi);
        if deck_at(decks, at(mid)) == owner {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    at(0.5 * (lo + hi))
}

/// Read a boolean Shortbread property (`bridge`, `tunnel`, `oneway`, `link`),
/// treating a missing or non-boolean value as `false`.
pub(crate) fn flag_of(feature: &Feature, name: &str) -> bool {
    matches!(
        feature.properties.as_ref().and_then(|p| p.get(name)),
        Some(Value::Bool(true))
    )
}

#[cfg(test)]
mod tests {
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
        let roads = super::tessellate_roads(&tile, tile_m, &mut decks, None, false)
            .expect("roads tessellate");
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
                    let n: usize = p.exterior().0.len()
                        + p.interiors().iter().map(|r| r.0.len()).sum::<usize>();
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
}
