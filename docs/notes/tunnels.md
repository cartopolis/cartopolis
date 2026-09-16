---
topic: roads
triggers: [tunnels.rs, tunnel, tunnel, entrance, cut_sheet, opening_digest, clipmap hole, basemap over a tunnel, building_passage, underground, drawn_street, street_shape, TunnelRoad, TunnelStructure, Tunnels, cut_ground, opening, floor_lift, layer, "the tunnel isn't rendering", "drive through the tunnel"]
updated: 2026-09-16
---

# Tunnels: what the tiles know, and everything they do not

Until 2026-09-15 a tunnel was a **gap**. `vector_tiles::drawn_street` refused any
way the tiles flagged, the raster painter skipped it, and `extract_street_labels`
withheld its name, so the southern ring road at Groningen — `Weg der Verenigde
Naties`, eight of whose segments are flagged — was simply absent between its
entrances: no carriageway, no markings, no name, nothing to walk into.

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

The second point has two consequences. A tunnel's depth cannot be read, so
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

The tile answers it, because a passage runs through a building and a tunnel does
not: `tunnels::building_passages` tests points along each flagged way against the
tile's own `buildings` polygons and calls it a passage on a majority. Those stay
on the ground sheet and are drawn as ordinary road — which is *more* than they had
before, since the old rule dropped them with everything else.

It fails safely. A tunnel mistaken for a passage is drawn where it always was; a
passage mistaken for a tunnel is the visible error, and needs the way to spend most
of its length inside a building footprint to happen.

## The entrance is invented, and it has to be

OSM tags the tunnel at full depth for its whole length, and the road that descends
to the entrance is an ordinary surface way with nothing on it to say it is a ramp.
Drawn literally that is a carriageway seven metres down meeting one at ground
level with a vertical step between them — a hole in the road.

[bridge-heights.md](bridge-heights.md) hits the same constraint from the other
side and **accepts** the step: a deck with abrupt ends still reads as a deck. A
tunnel cannot, because the step is the way in. So the ramp is built inside the
tagged tunnel at `MAX_GRADE` (6 %), which puts a 7 m descent inside 120 m.

Three things follow:

- A tunnel shorter than two ramps never reaches full depth. Honest for an
  underpass, wrong for a tunnel under a hill, and the tiles cannot tell those apart.
- The opening sits *inside* the tagged length rather than at its end.
- An end is a entrance only if the line genuinely ends there: a run the tile border
  cut is held at full depth (so the two halves meet flush at the border), and an
  end another tunnel run also ends on is a tagging split, not a opening.

## A tunnel is covered, and it took two wrong rules to get there

The roof rule went wrong twice, and both times the symptom was the same picture:
a long open ditch where a tunnel should be.

**First: roofing only under a building.** The tiles do not say where a lid is, and
they do say where a building is, so a way under a building was treated as covered
and everything else as an open cut. Groningen's ring runs under a *park*, so
nothing was ever covered. The observation that pushed me there — roofing by depth
"buries the whole ring, with nothing to see" — is not a bug report. It is what a
tunnel looks like from above. Depth decides now; the buildings layer only tells an
arcade from a real tunnel.

**Second: a dual carriageway could never surface.** A road only opens where
`open_ends` decides it genuinely ends, and that test asked "is there another
tunnel's end within 25 m pointing the same way?" — meaning a way somebody had split
in two. The two halves of a dual carriageway end *side by side* at the entrance,
about eighteen metres apart, pointing the same way. Each saw the other, both were
called splits, and neither got an entrance: measured over Groningen, **every 7 m
road tunnel in the streamed map was sealed from edge to edge, with no entrance
anywhere**. Beside is not beyond, and the test now requires the offset between the
two ends to lie *along* the road. Same tile after the fix: four entrances on the
deep roads, 6.0–6.7 m down, in opposite-heading pairs.

## Known wrong: the lid floats over its own ground

The measured ground says that road is in a nine-metre open trench, because it was
one when AHN flew it. The tag says tunnel, so it now gets a lid at street level.
The ground beside it follows the measurement down and the lid does not, so the
covered stretch reads as a raised platform rather than as ground you can walk over.

Two ways to settle it and it is a question about sources, not a bug:

- **Believe the tag** and raise the ground over the covered stretch to the
  surrounding level, so the lid is flush. The Ring Zuid lid is newer than the
  elevation survey, which argues for this.
- **Believe the measurement** and only cover where the ground above is genuinely
  continuous. Truer to the laser, and gives back the ditch.

## The roof stops where it would surface, and that is the opening

There is no "how long is the opening" constant. The ceiling is drawn wherever it
fits under the ground (`CEILING_MAX_Y`) and is absent where the rising deck would
push it through — which is exactly the open approach. The plan corridor of that
roofless stretch is handed back as a *opening*, and `cut_ground` subtracts it from
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
and needs no new channel — `interiors::support_level` asks the tunnels first.

What keeps the two apart is the rule that was already there: a surface more than
`STEP_UP_M` above you cannot pick you up. From the pavement over a tunnel the deck
is unreachable, so you walk over the tunnel; from inside, the ground over your head
is unreachable, so you walk along it. The only way between them is the ramp, which
rises continuously out of the deck — exactly as the only way upstairs is the
flight.

The walls have to be real collision. Without them, walking into one leaves the
carriageway, finds no deck under the next step, and the floor returns to ground
level — which reads as being fired seven metres up through the road. So a tunnel is
a **hollow** `collision::Footprint` while the avatar is inside it
(`tunnels::inside`), and no collider at all while it is not: on the surface it must
not be something to walk into, and underground it must be the only thing that is.
The two collision worlds are exclusive in both movement paths.

## Verified, and not

![The southern ring in open cut beside DUO](../img/tunnel-cut.png)

Captured over the Groningen ring on 2026-09-15 and again on 2026-09-16 with the
basemap cut (`--shot`, lavapipe, 53.20548 6.57498 at 35 m, pitch −25): 41 tunnels
standing in the streamed cells, deepest deck −7.0 m, 68 clipmap tiles carrying a
hole, and the sunken dual carriageway of `Weg der Verenigde Naties` in the trench
with its markings and its traffic on it, retaining edges either side, and the
ground unbroken where a building covers it.

**Aiming a camera at a tunnel is harder than building one**, and two days of this
work went on it. A shallow look *along* a tunnel — the angle that should see
furthest into an open cut — is blocked by the trees standing along the corridor:
at 18° above the horizon the frame is a wall of canopy. Deriving the camera from
the way's own geometry (an Overpass query, the bearing across the midpoint, the
camera placed back along the view direction) fixes the *direction* and does not
fix this. What works is the picture above: close, and steep enough to look over
the trees into the cut. `docs/grade-shots.toml` is the list, and its tunnel
entries are the ones still worth re-aiming.

The floor rule is unit-tested in all four directions it can go wrong
(`interiors::tests`): the pavement over a covered tunnel holds, the ground overhead
cannot lift you off the deck, the opening drops you onto the ramp, and ordinary
ground beside a tunnel is unchanged.

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
the tunnel onto a copy of the map: the 2026-09-15 capture over the merged ring
trench shows pale ground with the city's own cars drawn across the opening.

So `tunnels::cut_sheet` takes the same rings out of a clipmap tile's own mesh, in
`terrain::displaced_grid` — the one place a tile's triangles exist. Three things
follow:

- **Every level is cut, not just the finest.** The pyramid stacks its levels 0.4 m
  apart and sinks the unrelieved ones to just under the terrain field's lowest
  point, a few metres in a polder. Cut only the top one and the tunnel opens onto
  z10 imagery instead of z17 imagery.
- **A tile that needs neither relief nor curve now needs a mesh anyway.** It used
  to share one quad per zoom; a hole cannot be shared, so an overlapped tile gets
  its own and goes back to the shared quad when the opening leaves.
- **What a tile rebuilds on is a digest of the rings that reach *it*, not a global
  revision.** Every vector cell that lands changes the set of openings somewhere,
  and one counter would re-mesh every cut tile in the pyramid per cell of a fill —
  the coarse levels cover the whole city. `Tunnels::opening_digest` is quiet
  unless this tile's own hole moved.

## Photographing one is harder than building one

Worth knowing before the next person spends a day on it. Across this work,
**eleven cameras aimed at a tunnel missed** — by 28, 42, 56, 62, 64, 103, 111,
122, 136, 140 and 178 metres — landing on a park, a rail yard and the side of an
office block, each costing minutes of software rendering to discover. Three causes,
all now removed:

- **Hand-converting the tessellator's tile-local metres** into a latitude and
  longitude. Two hand-written conversions disagreed with each other and both
  disagreed with the client. `Tunnels::nearest_entrance` answers instead, and
  `tunnel_entrance_lat`/`lng`/`heading` in `--dump-state` is the answer.
- **Aiming by pitch at a shallow angle.** What lands in frame is
  `altitude / tan(pitch)` metres ahead, and `altitude` is above the map anchor's
  ground while the subject stands on the measured ground; with the terrain layer
  on those differ by metres, and at six degrees three metres of error moves the aim
  thirty. `--look-at lat,lng` computes the direction from where the camera really
  is (`shot_harness::aim_scripted_camera`).
- **Aiming at the middle of an opening**, which is by definition the stretch that
  has *not* reached depth — a road at lawn level.

`tunnel_ahead_m` is the check: zero means the capture is pointed at a tunnel, and
anything else means the picture is of something else. And the pictures are barely
evidence anyway on a software rasteriser at handset quality — the profile printed
by `tunnels::tests::dump_the_fixture_bores` says more in ten lines than any of
them did.

## What is not done

- **Nothing collides with the roof**, so flying up through it works.
- **The routing graph is unchanged**: `RoadGraph` is built from the same
  `streets` features and never knew about the tunnel flag either way, so a route
  through one is drawn on the surface (`navigation.rs` says why a negative grade
  is not pushed down).
- **Depth is a constant, and it need not be.** See below.

## Everything vertical here is invented, and two sources we already stream could replace it

The tiles carry one bit. So the plan position of a tunnel is exact and *every*
vertical fact in the picture is a convention: `LEVEL_DEPTH_M` (7 m), `MAX_GRADE`
(6 %), `CLEARANCE_M`, and "roof it where a building is above". That is a
plausible tunnel, not that tunnel, and it is why a capture of one is
unconvincing however the camera is aimed.

Both halves of the real answer were checked on 2026-09-16 and both exist:

**The survey publishes the footprint.** `tunneldeel` is one of the BGT's 49
collections — the covered section as a *polygon*, with `relatieve_hoogteligging`
giving the layer ordering the tiles lack. Seven live polygons over the Groningen
ring box, all at level −1; one is 183 × 103 m centred on 53.20536, 6.57503, which
is the DUO cut. That replaces `under_a_building` — a heuristic standing in for a
surveyed fact — with the fact. `overbruggingsdeel` beside it is the same answer
for [bridge-heights.md](bridge-heights.md). It needs a fetch in cartopy's
pipeline, i.e. another hand-synced seam
([where-the-map-data-is-built.md](where-the-map-data-is-built.md)).

**AHN measures the trench, at half a metre.** `dtm_05m` over that same polygon,
54,291 valid samples in a 120 m box:

```
min -3.64   median 5.39   max 11.05  (m NAP)      range 14.69 m
cross-section every 4 m:  5.5  -0.9  -0.4  -0.1  0.9  2.4  2.6  3.4  3.9  4.8  5.3  5.5 …
```

The ramp is *in the data*: the ground falls from +5.5 m to −0.9 m over about
forty metres and bottoms at −3.6 m. About nine metres of cut, against the seven
that is hard-coded.

**Which suggests deleting the constant rather than tuning it.** In an open cut a
tunnel road is not a special object — it is an ordinary road lying on ground that
happens to be nine metres down. So the measured terrain should carry it and
`depth_at` should not run there at all, leaving a convention only under the
surveyed covered footprint, where the laser sees the roof and no instrument can
see the floor. That also disposes of the double-counting this bullet used to
warn about: today the terrain would dig the cut *and* the deck would be sunk into
it. Two things are missing for it: the `tunneldeel` fetch above, and the fine
half-metre AHN tier feeding **geometry** rather than only the shading normal it
feeds today (`systems::ground_detail`).
