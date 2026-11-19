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
    // …and the paths on a deck that the survey draws — see
    // [`RoadTessellation::paths_on_deck`].
    let mut paths_on_deck: Vec<Approach> = Vec::new();
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
        let bridge = flag_of(feature, "bridge");
        if cover.supersedes(kind_of(feature).unwrap_or("")) {
            if !decks.is_empty() {
                let lines: Vec<&geo_types::LineString<f32>> = match &feature.geometry {
                    Geometry::LineString(ls) => vec![ls],
                    Geometry::MultiLineString(mls) => mls.iter().collect(),
                    _ => Vec::new(),
                };
                paths_on_deck.extend(
                    lines
                        .into_iter()
                        .flat_map(|ls| {
                            let pts: Vec<[f32; 2]> =
                                ls.0.iter()
                                    .filter(|c| c.x.is_finite() && c.y.is_finite())
                                    .map(|c| [c.x, c.y])
                                    .collect();
                            clip_polyline(&pts, 0.0, extent)
                        })
                        .flat_map(|run| {
                            let metres: Vec<[f32; 2]> =
                                run.iter().map(|p| [p[0] * scale, p[1] * scale]).collect();
                            if bridge {
                                split_by_deck(&metres, decks)
                            } else {
                                carried_onto_deck(metres, decks, &bridge_ends)
                            }
                        })
                        .filter_map(|(deck, points)| {
                            #[allow(clippy::cast_possible_truncation)]
                            deck.filter(|_| points.len() >= 2).map(|d| Approach {
                                deck: d as u32,
                                points,
                            })
                        }),
                );
            }
            continue;
        }
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
        paths_on_deck,
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
    /// The stretches on a deck of the paths the survey draws instead
    /// ([`SurveyedCover::supersedes`]): centre lines, no ribbon. Nothing is laid
    /// for them, but where they leave the deck is still where its railing must
    /// open — the H.N. Werkmanbrug's cycleway was one, and its railing ran
    /// across the station end (2026-11-05).
    pub paths_on_deck: Vec<Approach>,
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
mod tests;
