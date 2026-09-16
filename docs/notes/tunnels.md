---
topic: roads
triggers: [tunnels.rs, tunnel, bore, portal, cut_sheet, mouth_digest, clipmap hole, basemap over a tunnel, building_passage, underground, drawn_street, street_shape, TunnelDeck, TunnelShell, TunnelBores, cut_ground, mouth, floor_lift, layer, "the tunnel isn't rendering", "drive through the tunnel"]
updated: 2026-09-16
---

# Tunnels: what the tiles know, and everything they do not

Until 2026-09-15 a tunnel was a **gap**. `vector_tiles::drawn_street` refused any
way the tiles flagged, the raster painter skipped it, and `extract_street_labels`
withheld its name, so the southern ring road at Groningen — `Weg der Verenigde
Naties`, eight of whose segments are flagged — was simply absent between its
portals: no carriageway, no markings, no name, nothing to walk into.

That refusal was right about one thing and silent about the rest. Painting a
tunnel **on the ground** rules a pale band across every junction it passes under,
which is a lie about what is up there. Nothing ever drew it anywhere else.

## The tiles carry one bit

Measured on `crates/geo/tests/fixtures/groningen_z14.mvt`, 2026-09-15, and pinned
by `tunnels::tests::the_tiles_say_which_streets_are_tunnels`:

- **Ten** streets in that tile are flagged `tunnel`.
- The `streets` attribute set is `kind, bridge, tunnel, rail, oneway,
  oneway_reverse, link, surface` (plus `service`, `bicycle` on some). There is
  **no `layer`** on any feature in the layer.

The second point has two consequences. A bore's depth cannot be read, so
`LEVEL_DEPTH_M` (7 m) is a convention applied to every tunnel equally — the one
number here a real source could replace. And `vector_tiles::layer_of` answers 0
for every street on this tileset, which makes the raster's "OSM layer first, so a
flyover paints over what it crosses" sort a **no-op**. Its doc comment says
Shortbread carries `layer` on `streets`; on this tileset it does not. That is a
separate thing to fix and is not fixed here.

## An arcade is flagged a tunnel too

OSM's `tunnel` also covers `building_passage` — a way *through* a building, at
ground level, that people walk down. Over the Groningen ring box roughly a quarter
of the flagged ways are those (12 of 44, Overpass, 2026-09-14). With no `layer` to
separate them, sinking every flagged way seven metres puts a hole under a shopping
street.

The tile answers it, because a passage runs through a building and a bore does
not: `tunnels::building_passages` tests points along each flagged way against the
tile's own `buildings` polygons and calls it a passage on a majority. Those stay
on the ground sheet and are drawn as ordinary road — which is *more* than they had
before, since the old rule dropped them with everything else.

It fails safely. A bore mistaken for a passage is drawn where it always was; a
passage mistaken for a bore is the visible error, and needs the way to spend most
of its length inside a building footprint to happen.

## The portal is invented, and it has to be

OSM tags the tunnel at full depth for its whole length, and the road that descends
to the portal is an ordinary surface way with nothing on it to say it is a ramp.
Drawn literally that is a carriageway seven metres down meeting one at ground
level with a vertical step between them — a hole in the road.

[bridge-heights.md](bridge-heights.md) hits the same constraint from the other
side and **accepts** the step: a deck with abrupt ends still reads as a deck. A
tunnel cannot, because the step is the way in. So the ramp is built inside the
tagged tunnel at `MAX_GRADE` (6 %), which puts a 7 m descent inside 120 m.

Three things follow:

- A tunnel shorter than two ramps never reaches full depth. Honest for an
  underpass, wrong for a bore under a hill, and the tiles cannot tell those apart.
- The mouth sits *inside* the tagged length rather than at its end.
- An end is a portal only if the line genuinely ends there: a run the tile border
  cut is held at full depth (so the two halves meet flush at the border), and an
  end another tunnel run also ends on is a tagging split, not a mouth.

## The roof stops where it would surface, and that is the opening

There is no "how long is the mouth" constant. The ceiling is drawn wherever it
fits under the ground (`CEILING_MAX_Y`) and is absent where the rising deck would
push it through — which is exactly the open approach. The plan corridor of that
roofless stretch is handed back as a *mouth*, and `cut_ground` subtracts it from
the cell's ground meshes on the same worker, on `road_stitch`'s integer millimetre
lattice and for the same reason: a boundary produced by subtracting one shape from
another is built from that shape's own vertices, so the hole's edge and the wall
standing in it are one line rather than two opinions about one line.

**Without the cut there is no tunnel**, only a room with a lid over its own
entrance.

## You walk in because a tunnel floor is a negative storey

`interiors::InteriorFloors` already answered "how high is the ground here" for a
building's upper storeys, and both movement paths add the answer
(`AvatarState::floor_lift`) to the terrain. A deck is that with the sign flipped,
and needs no new channel — `interiors::support_level` asks the bores first.

What keeps the two apart is the rule that was already there: a surface more than
`STEP_UP_M` above you cannot pick you up. From the pavement over a bore the deck
is unreachable, so you walk over the tunnel; from inside, the ground over your head
is unreachable, so you walk along it. The only way between them is the ramp, which
rises continuously out of the deck — exactly as the only way upstairs is the
flight.

The walls have to be real collision. Without them, walking into one leaves the
carriageway, finds no deck under the next step, and the floor returns to ground
level — which reads as being fired seven metres up through the road. So a bore is
a **hollow** `collision::Footprint` while the avatar is inside it
(`tunnels::inside`), and no collider at all while it is not: on the surface it must
not be something to walk into, and underground it must be the only thing that is.
The two collision worlds are exclusive in both movement paths.

## Verified, and not

Captured over the Groningen ring on 2026-09-15 (`--shot`, lavapipe): 41 bores
standing in the streamed cells, the ground cut away over both approaches, and the
sunken dual carriageway of `Weg der Verenigde Naties` visible in the cut from
directly above with its markings and its traffic on it.

The floor rule is unit-tested in all four directions it can go wrong
(`interiors::tests`): the pavement over a covered bore holds, the ground overhead
cannot lift you off the deck, the opening drops you onto the ramp, and ordinary
ground beside a bore is unchanged.

**Walking through one has not been seen end to end.** Standing the harness's
avatar on a deck needs it to spawn on a specific 12 m strip of carriageway, and it
spawns ~40 m from the `--at` point for a reason this work did not run down —
`avatar_floor_m` in `--dump-state` reads 0 in every attempt, which is the honest
report of an avatar on the surface, not of a floor that failed. The instrument is
there for whoever aims it: `avatar_floor_m` is the tunnel deck's level when the
avatar is on one.

## The basemap needs the same hole, and every level of it

Cutting the vector ground is not the whole cut. Under those surfaces lies the
raster clipmap — `y = -0.03` for the finest level and 0.4 m lower per coarser one
(`map_stream::level_y`) — painted with a picture of the same streets. All of it is
above a seven-metre carriageway, so a cut that opens only the vector sheet opens
the bore onto a copy of the map: the 2026-09-15 capture over the merged ring
trench shows pale ground with the city's own cars drawn across the opening.

So `tunnels::cut_sheet` takes the same rings out of a clipmap tile's own mesh, in
`terrain::displaced_grid` — the one place a tile's triangles exist. Three things
follow:

- **Every level is cut, not just the finest.** The pyramid stacks its levels 0.4 m
  apart and sinks the unrelieved ones to just under the terrain field's lowest
  point, a few metres in a polder. Cut only the top one and the bore opens onto
  z10 imagery instead of z17 imagery.
- **A tile that needs neither relief nor curve now needs a mesh anyway.** It used
  to share one quad per zoom; a hole cannot be shared, so an overlapped tile gets
  its own and goes back to the shared quad when the opening leaves.
- **What a tile rebuilds on is a digest of the rings that reach *it*, not a global
  revision.** Every vector cell that lands changes the set of openings somewhere,
  and one counter would re-mesh every cut tile in the pyramid per cell of a fill —
  the coarse levels cover the whole city. `TunnelBores::mouth_digest` is quiet
  unless this tile's own hole moved.

## What is not done

- **Nothing collides with the roof**, so flying up through it works.
- **The routing graph is unchanged**: `RoadGraph` is built from the same
  `streets` features and never knew about the tunnel flag either way, so a route
  through one is drawn on the surface (`navigation.rs` says why a negative grade
  is not pushed down).
- **Depth is a constant.** AHN measures the real trench where a Dutch tunnel is an
  open cut — the Groningen ring reads −7.6 m NAP against +2.3 m either side
  (2026-09-14) — so the measured field could supply the depth over the
  Netherlands. It would also double-count wherever the terrain already dug the
  cut, which is the thing to work out before wiring it.
