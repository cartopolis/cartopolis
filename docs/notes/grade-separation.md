---
topic: routing
triggers: [routing.rs, RoadGraph, finish, noding, NODE_TOL, level_of, node_levels, node_terminal, dangle, bridge, tunnel, viaduct, layer, interchange, "drove onto the motorway", "route goes up the flyover", grade]
updated: 2026-08-29
---

# Bridges and tunnels in the routing graph

The drawn map has understood grade since the bridge decks landed: `bridges` is its
own Shortbread polygon layer, `vector_tiles::tessellate_bridges` turns it into
decks that stand at a measured height, the bridge-flagged street's own span leaves
the ground sheet and is lifted onto that deck (`bridge-heights.md`), and tunnels are
refused outright by both the raster and the geometry. The **routing graph** was flat
until 2026-08-20, and this note is about why that mattered and what the fix rests
on.

## Why a flat graph invents roads that do not exist

`RoadGraph::finish` splits every segment at the nodes lying within `NODE_TOL`
(~0.8 m) of it — the noding pass, without which 42 % of the network is unreachable.
Shortbread merges ways by attribute and drops the way ids, so this pass has nothing
to check its inventions against. Run it unguarded and a viaduct's vertex sitting
over the street it crosses is a junction: a car leaves the underpass, climbs onto
the motorway, and nothing in the returned route says it happened. There is no wrong
turn to see, because the turn list only knows the graph.

## The two guards, and which one does the work

**Only a dangle may split.** The tee this pass exists to repair is a line that
*stops dead* on another line, its shared vertex generalised away. A line that
carries on past the contact was never touching it. That is the whole difference
between a junction and a crossing, and marking each line's own two ends recovers it
from data the tiles already carry. It is the standard noding rule — snap dangles,
not crossings — and it was missed on the first attempt at this, which reached for
the height instead and got a guard that could not see an interchange.

**…and only at its own grade.** Being a dangle is not sufficient: a slip road really
does end on the carriageway it joins, and a bridge's abutment is a dangle sitting
directly over the street it lands beside. Each segment carries a level and each node
the levels of the segments that end at it; a split needs one in common.

The drawn ribbon reached the same conclusion independently and a few days earlier —
[road-junction-geometry.md](road-junction-geometry.md), which measured that 58 % of
the "junctions" `street_nodes` finds on this fixture are continuations, and ends
"anything else that reasons about a road's ends owes the same distinction". This is
that debt paid on the routing side. The two are separate code and neither imports the
other's answer, so a change to one is worth checking against the other.

Refusing a split can never sever anything the tiles connected, because that
connectivity is node identity — a shared vertex is a shared node id and its edges are
emitted whatever this pass decides. So both guards are one-directional: they remove
junctions that were never there and cannot remove one that was. That asymmetry is
why they are safe to apply bluntly, and the numbers below confirm it rather than
establish it.

## Measured, Groningen z14 fixture, 2026-08-20

Each variant is the same tile with one guard relaxed
(`routing::tests::grade_separation_diagnostic`, `--ignored --nocapture`). "flat" is
the pass as it stood before this work.

| variant | edges | mid-span refused | cross-level refused | car | bike | foot |
|---|---|---|---|---|---|---|
| flat | 6348 | 0 | 0 | 0.9199 | 0.9561 | 0.9622 |
| level only | 6340 | 0 | 4 | 0.9199 | 0.9553 | 0.9614 |
| dangle only | 6230 | 59 | 0 | 0.9195 | 0.9560 | 0.9622 |
| **both** | **6226** | **59** | **2** | **0.9195** | **0.9552** | **0.9614** |

Three things this decided:

- **The dangle rule carries the fix.** 59 refusals against the grade rule's 4, and
  it costs 0.0004 of the car component to do it.
- **The grade rule still earns its place.** It refuses 2 junctions the dangle rule
  admits — genuine dangles at the wrong height, which is exactly the abutment shape.
  (4 alone, 2 alongside: two of the four were mid-span as well.)
- **Four grade refusals over a city tile is a fact about Groningen**, not a
  disappointing result. Its bridges cross *canals*, and a bridge over water meets no
  road to be fused with. The case the grade rule exists for is a motorway
  interchange, which this fixture does not contain.

The component share is what had to be watched, because the worry ran the other way:
bridge ramps whose junction with the ground road exists *only* because the noding
pass invents it. Two or three nodes per mode, against the ~2 500 in the tile.

## Rejected: keying the level into node identity

The obvious fix — make a node `(x, y, level)` instead of `(x, y)` — is worse than
the problem. A bridge meets the ground at its abutments through exactly such a
shared vertex, so a level-keyed node disconnects **every** bridge in the network
from both its ends. `a_bridge_meets_the_ground_at_its_abutments` is the test that
notices; the crossing and the abutment are geometrically identical and only the
shared vertex tells them apart.

The residue this leaves is real and unfixed: a bridge vertex that quantises onto a
street vertex fuses them into one node before the noding pass runs at all. At
0.55 m per lattice unit on independently generalised lines that is a coincidence,
not a pattern.

## Upstream: Shortbread ships no `layer`

`level_of` prefers OSM's `layer` and falls back to the `bridge` / `tunnel` flags.
Against the VersaTiles Shortbread basemap the fallback is the whole story — its
`streets` layer carries `bicycle, bridge, horse, kind, link, oneway,
oneway_reverse, rail, service, surface, tunnel` and no `layer` (checked 2026-08-20
on the fixture). The `layer` arm is kept because it costs nothing, it is what
`vector_tiles`' raster draw order already sorts on, and another source may carry it.

So every bridge lands on level 1, and the grade rule cannot separate two viaducts
crossing at an interchange. **The dangle rule can**, and does
(`two_viaducts_crossing_are_not_a_junction`) — the recoverable fact was never the
height, it was which line *ends* at the contact. This note previously said the
interchange case needed a source carrying `layer` and that nothing here could
recover it. That was wrong, and it was wrong because the first design reasoned about
grade when the question was about topology.

## The drawn line had the same bug, one layer down

A grade-aware graph routes a car over a viaduct; the ribbon was still drawn on the
ground plane, so the line ran *underneath* the structure it had just been routed
across. The deck heights existed — `resolve_bridge_decks` measures every one of them
— but it spent them into `build_deck_mesh` and forgot them.
`map_geometry::DeckPlates` keeps them instead, in the anchored world-XZ frame, with
the same shape and contract as `collision::BuildingColliders`: the streamer
publishes, consumers sample, an entry dies with its cell. Only *lifted* decks are
kept (`MIN_LIFT_M`) — a flat deck is a piece of road surface with nothing to climb
onto.

**The first attempt at the lift was wrong, and wrong in an instructive way.** It
sampled the deck height as a function of world XZ and added it to the terrain drape.
That reads as the obvious thing and it cannot work: a road on a viaduct and the road
underneath it are at the *same* world XZ, so any lift decided by position hoists the
underpass onto the structure above it. Position is not enough — the only thing that
knows which of the two a point is on is the graph edge it came from.

So the grade goes on `Edge` after all, and out through `Route::levels`, parallel to
`points`. `navigation::route_lift` consults `DeckPlates` **only** where the route's
own grade says it is on a bridge, and `ribbon` takes a per-point height rather than
building flat. Three consequences worth keeping:

- **`Route::points` and `Route::levels` are parallel arrays and every splice must
  keep them so** — the staged-route concatenation and `dedupe` both. Two arrays
  thinned independently drift apart at the first collapsed pair, and the symptom is a
  line that climbs a viaduct a few metres early. `dedupe` returns the surviving
  indices for exactly this reason.
- **The step is abrupt on purpose.** A deck that clears `MIN_LIFT_M` has vertical
  fascia and no modelled approach ramp, so a line that steps where the deck steps is
  a line drawn on the structure. Smoothing the climb would part it from the deck.
  The abutment counts as *ground* (`build_route` takes the lowest grade of the edges
  meeting at a point), which puts the step just inside the structure — where the
  fascia is.
- **`terrain::drape_mesh` adds rather than assigns**, which is what lets the ribbon
  arrive with a deck baked in and still follow the ground the deck stands on. It kept
  its original signature; the `extra` field the first attempt gave it is gone.

A tunnel is deliberately not pushed *down*. There is no modelled bore to sink into,
so the line would vanish under the map.

## Tunnels lost their labels too

`extract_street_labels` never filtered tunnels, so a buried street's name was painted
on the ground over whatever is genuinely up there — the same fault `drawn_street` and
the raster draw order had already fixed for the carriageway. Shortbread puts `tunnel`
on `street_labels`, so it was one `continue`.

The test for it has to assert on *geometry*, not on names: Shortbread merges by
attribute, so a street with a tunnel section keeps a separate surface feature under
the same name, and the fixture has nine tunnel-flagged label features and **no**
tunnel-only name. "Kastanjelaan is not labelled" is simply the wrong claim. It also
has to probe the middle of a *segment* rather than a vertex — a vertex is routinely
the portal, which the surface run legitimately shares, and a probe sitting on a
shared node reports a 0.0 m hit against a label that is drawn correctly.

## The ambient traffic carries a grade too, and for the same reason

Settled 2026-09-22. `systems::traffic` had both halves of the fault the route
line had: `extract_drive_lines` inherited `drawn_street`'s refusal of every
tunnel-flagged way, which is right for a ribbon and wrong for a *network* — it
cut the drive graph at every portal, so a car reaching Groningen's ring ran to
the end of its line and was recycled — and `drive_traffic` placed every car at
`terrain.ground_at + ride height`, so the traffic on a viaduct drove through the
rail yard underneath it.

**Both halves are fixed the way the route line's was, and the reasoning
transfers exactly.** A depth or a lift read by *position* cannot work, because a
street crossing over the ring and the carriageway seven metres under it are at
the same world XZ — the mirror image of `DeckPlates::lift_at` hoisting an
underpass onto the structure above it. So the line carries its own grade:
`DriveLine::deck` for the tunnel depth, resolved per `streets` feature by
`tunnels::SunkenWay`, and `DriveLine::on_deck` for the bridge, which is a flag
rather than a height because a deck's height is a measurement that arrives after
the worker has finished with the tile.

The one thing that differs from the route is that the drive network also covers
the **untagged** stretches: the open cuttings between a tunnel's lids are
ordinary surface ways that `cartopolis_geo::tunnels` has decided are seven metres
down ([tunnels.md](tunnels.md)), so a consumer reading the tunnel flag would put
its cars on the ground over an open trench.

## Still flat elsewhere

**Street labels on bridges cannot be fixed from this data.** `street_labels` carries
`tunnel` but no `bridge` (checked 2026-08-20 on the fixture), so a name on a viaduct
is indistinguishable from one on the road beneath it, and `DeckPlates::lift_at` on
its own would raise both. The route line escapes this only because it has the graph
behind it; the labels have no equivalent. A source carrying `bridge` on that layer is
what it would take.

**The avatar still walks under decks**, and the reason is the same one, one step
worse — note the ambient traffic escaped it only because a car belongs to a
*line*, and the avatar belongs to nothing. `AvatarState` has no grade and nothing to derive one from: the sim is
authoritative for position, holds no map data, and never will
(`docs/notes/terrain-relief.md` has the matching argument for the terrain lift, which
works precisely because a height field *is* a function of position and a deck is
not). Making a deck walkable needs the avatar to carry which level it is on, plus
transitions at the abutments — a feature, not a fix.
