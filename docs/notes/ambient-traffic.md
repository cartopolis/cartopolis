---
topic: ambient-traffic
triggers: [traffic.rs, "ambient traffic", "cars driving", "moving cars", "traffic",
  DriveNetwork, drive_traffic, oneway_reverse, "streets layer", data_layers.rs,
  "Life layer", life_enabled, cars_visible, people_enabled, "layer toggle"]
updated: 2026-08-29
---

# Cars and people that move, off the streets layer we already parse

`cartopolis_geo::traffic` + `systems::traffic` (2026-08-26). The first thing in
this tree that **moves on its own**: everything else streamed from the map is
placed and then stands still, and a city of stationary objects is most of what
makes a streamed map read as a model rather than a place. (Live transit vehicles
move too, but they are a real feed of real buses and only exist where OVapi has
one.)

It costs no request and no extra decode — the drivable centrelines come off the
same `streets` layer the road ribbons, the routing graph and the street labels
are already built from. Sibling notes: `docs/notes/parked-vehicles.md` and
`docs/notes/lit-windows.md`.

**These cars stay invented, and that was checked rather than assumed.** The real
counts are NDW's, and its 20,519 loop detectors sit on motorway mainlines — none
of them within the 260 m the pool places cars in, at three of the five points
measured. [ndw-traffic.md](ndw-traffic.md) has the numbers and the reason.

## `oneway` and `oneway_reverse` are different answers

Shortbread carries both. `vector_tiles::street_style` **ORs them together**, and
is right to: a painted road is the same width whichever way it runs. Traffic
cannot do that — the two send a car in opposite directions, and collapsing them
would run every `oneway_reverse` street's traffic backwards. `traffic::flow_from`
is a pure three-way function with a test on it for exactly this reason.

## Nothing may be built per frame

The documented device-loss failure in this tree is mass mesh churn
(`docs/notes/upload-bursts.md`), and a moving object is the obvious way to
reintroduce it. So:

- There is exactly **one** car mesh, built once at startup from the same
  `furniture::form::CAR` parts the parked cars use — a car waiting at a kerb and
  a car driving past it are the same object.
- A car is an entity with a `Transform`. Driving it writes that transform and
  nothing else. **After startup this layer uploads nothing, ever.**
- The pool is **fixed** and cars are **recycled**, never spawned or despawned on
  the move. A car that runs out of range is given a new road, not deleted.

`systems::grass` reached the same shape from the same constraint and states it as
a rule. The one thing that differs: a shared mesh cannot carry a per-instance
tint, so the paints are a handful of shared materials rather than vertex colours —
which is also what lets bevy batch 180 cars into a few draws.

## Two bugs, both silent, both found by looking at a capture

- **The pool was never created.** `setup_traffic` parents the cars to the
  `BigSpace` root, which `free_camera::setup_camera` spawns — and it was
  registered in the same `Startup` tuple with no ordering, so it ran first, found
  no root and returned. No cars, no error, nothing in the log. It is
  `.after(setup_camera)` now, as `setup_sky_dome` in that same tuple already was,
  and the unreachable arm **logs a warning** instead of returning in silence.
- **One car in view.** Placing uniformly over every candidate road spreads the
  pool across every service alley and back lane in the ring. The count went
  96 → 180 and placement now takes three draws and keeps the fastest road —
  `drive_speed` is already this codebase's encoding of road importance, so it is
  the natural weight.

## People walk the same machinery

Pedestrians (2026-08-26) are the *same* pool, the same steering and the same
recycling as the cars — `AgentKind` is the only thing that differs, and it answers
four questions: which lines this kind may use, how fast it goes, how far from the
centre line it sits, and how far out it is placed. Generalising was cheaper than a
second module and keeps one hand-off, one recycle rule and one set of tests.

Three things are specific to people:

- **Who may use a line is not an either/or.** A residential street carries cars on
  the carriageway *and* people on the pavement, and the tile does not model
  pavements at all — so `Uses` has two independent flags and the common case is
  both. A footway carries people and no cars; a motorway the reverse; a railway
  neither. `the_two_networks_differ_in_both_directions` pins it, because getting
  one list right does not imply the other.
- **The pavement is an offset, nothing more.** A walker sits at `half_width +
  clearance` — *outside* the kerb — and that offset is the entire mechanism
  putting a person beside a street rather than down the middle of it. A pavement
  is also two-way whatever the carriageway beside it does, which is what somebody
  walking up a one-way street is doing every day.
- **A person is placed closer than a car.** `place_radius` is 45% of the car's.
  A walker is a third of a car's size and legible at roughly a third of the
  range; at one radius for both, 220 walkers over a city-centre ring put *none*
  on the street in view while the metrics reported all 220 placed. That is also
  the clearest lesson about the metric itself — see below.

The head is deliberately grey rather than a skin tone. A crowd rendered in one
complexion is a claim about a city's people that a road centreline cannot support,
and six would be a worse claim rather than a better one. It reads as a figure, not
as somebody identifiable, which is the honest resolution for something inferred
from "a pavement exists here".

**No walk animation.** Three boxes sliding along a line. `avatar-game-feel.md`
documents how badly a sliding figure reads — but that is about a model you stand
directly behind, and these are seen from tens of metres where an upright
silhouette in motion is the whole signal. That is an argument, not a measurement,
and no still can settle it.

## `traffic_cars` / `traffic_walkers` mean *placed*, not *on screen*

`ShotMetrics` carries both. The test is `Visibility != Hidden`, which an agent
behind a building still passes — deliberately, because the pools are a fixed size
and spawn hidden, so a raw entity count reads identically whether the layer works
or never found a road. Zero over a city with the toggle on means the network never
arrived or the pool was never created, which is exactly the failure that hid the
missing car pool.

What it cannot tell you is whether any of them are in frame. A **full count with
an empty-looking street means the pool is spread too thin, not that it is
broken** — which is precisely how the walker radius was diagnosed.

Adding it cost a lesson too: `drive_capture` sits on Bevy's 16-parameter ceiling,
so a bare new query took it to seventeen and it silently stopped being a system.
The compiler's complaint was that `.after()` did not exist on it, several steps
from the cause. The visibility queries are one `SystemParam` bundle now.

## What this deliberately is not

No car-to-car collision, no obeying the traffic signals that already cycle
(`systems::street_detail`), no give-way, no queueing, no avatar interaction, and
people walk through each other and through the cars.
Every one is a simulation cost landing on every frame, and none is what the layer
is for: the question it answers is "is this street alive", settled at a glance
from a range where two cars overlapping for a moment is invisible. Recorded here
rather than hidden — a car driving through a red light is a known limit, not a
surprise.

## Its own section: **Life** (2026-08-29)

It started as one row, **Map ▸ Traffic & people**, put there because it was not
the street furniture. But Map's other rows are all things that are *surveyed,
streamed and then stand still* — canopies, benches, road textures, ground tiles —
and these move. Motion is a property of its own, and it is the one a viewer either
wants or does not, independently of everything under it: it is what you switch off
to judge a still, and what a slow phone gives up first.

So the moving things are their own top-level group, **Life**, sibling to Buildings
/ Map / Height, with three rows:

- **Traffic** — the cars (`traffic_enabled`).
- **People** — the walkers (`people_enabled`), split out of the same row. The two
  pools were always separate and separately sized; somebody judging a street's
  emptiness wants exactly one of them, and a hidden pool costs a transform write
  it never makes.
- **Live vehicles** — the real OVapi feed (`vehicles_enabled`), moved up from
  under the Public transport *marker* layer, which it also **no longer requires**.
  That conjunct (`layers[TRANSIT_LAYER].enabled && vehicles_enabled`) was a
  politeness gate rather than a data dependency — the feed carries its own
  positions and never reads a stop — and its effect was a checkbox that did
  nothing at all unless another one, in another section, happened to be ticked.
  The layer is opt-in already; one opt-in is enough.

The two headings inside the section say which is which — *Generated from the
streets* over the first two, *Reported by their operators* over the third. Two of
these three are invented, and a panel that lists them together without saying so
is claiming the map knows where the cars are.

**The master toggle is read through `cars_visible()` / `people_visible()` /
`live_vehicles_visible()`, never as `life_enabled` directly.** A group master each
consumer has to remember to `&&` is one that a consumer will eventually forget,
and that layer then keeps moving with the group switched off. Four call sites
use them: the two pools in `drive_traffic`, the vehicle feed's poll and its draw,
and the road hum (`cars_visible`, because the hum is a road's — a pavement of
people makes none of it).

Two smaller things went with it. `drive_traffic` skips `DriveNetwork::near` for a
pool that is off — that scan is the whole per-frame cost of a switched-off layer —
and the panel's **Reset** button, which restored every other checkbox, now also
restores traffic, people and `street_detail` (that last one was simply missed: a
reset that puts back all but one reads as a toggle that has stuck).

Both pools ride `Budget::detail_layers`, so a phone still starts without them;
`life_enabled` starts on, like the other group masters, because what a fresh
install starts *with* is decided by the rows under it.

`LayerPrefs::traffic`, `::people` and `::life` use the same `serde(default)`
helpers as their neighbours, so a `user.json` written before any of them existed
reads as the tier's default rather than as off — an absent key is "not stated",
not "the user turned it off". In particular a file predating the split gives the
walkers the tier default, which is what the single toggle it does carry meant for
them anyway.

## Verified, and what is not

Verified from stills over Groningen: cars appear, sit on the carriageway, face
along it, keep to the right-hand lane, and carry the parked cars' paint palette;
people appear on the pavements either side of a street rather than in it.

**Not verified**: that they drive *smoothly*, that the junction hand-off is
seamless, or that nothing pops when a car is recycled. A still cannot show any of
it, and this container has no way to watch a moving frame. The hand-off logic
excludes the road just left — without that a car at a dead end turns round and
drives back down it forever — but that path has been reasoned about, not watched.

## The city hums, off the same network

`soundscape::update_road_hum` (2026-08-26) drives a looping road hum from
`DriveNetwork::loudest_road` — the same resident roads the cars drive, already
carrying each road's class as a speed. No new data, no new request, one scan of
the loaded ring per frame.

Four decisions:

- **A maximum over nearby roads, not a sum.** Summing makes a dense grid of back
  streets louder than a motorway, which is backwards: what you hear is the
  biggest road near you, not the number of roads near you.
- **It falls off with the square of the remaining distance** over
  `ROAD_AUDIBLE_M` (140 m). That is far shorter than real traffic carries, on
  purpose — the hum has to change over a distance you can walk, or it is a
  constant, and constants are inaudible.
- **Noise alone reads as weather.** Distant traffic has a pitch — the tyre roar
  around 68 Hz that carries furthest — so a faint tone sits under the rush, weak
  enough that no note can be named for it. Its period is forced to divide the
  loop, because a sine that does not close is a click every two seconds.
- **Silent from the air**, and gated on **Life ▸ Traffic** (`cars_visible()`):
  somebody switching the traffic off expects the sound of it to stop too. On the
  cars alone — a hum is a road's, and the people make none of it.

`soundscape::road_loudness` is pure and tested on every lane, which is why
`systems::soundscape` is now re-exported unconditionally — the `cfg(feature =
"audio")` on that re-export existed only because nothing outside reached it by
name, and its own comment anticipated this.

**Unheard.** This container has no audio hardware at all
(`docs/notes/container-audio.md`), so the hum is verified as *arithmetic and a
well-formed clip with headroom* — never as a sound. The headroom assertion is not
ceremony: a synthesised clip that clips arrives as a roar at any playback volume,
which is how the wind was reported from a phone, and no volume slider can undo
distortion baked into an asset.

## Water and playground ambience are not done

Both were proposed alongside the hum and neither is here. The reason is the same
for both: there is no resident, queryable source to hang them on. The roads are a
resource because `systems::traffic` needed one; water lives only as per-cell
tessellated geometry and shore rasters, and a POI's category reaches
`WorldPlaces` but nothing measures a *distance to the nearest park*. Either would
need a proximity index built first, which is the actual work — the synthesis and
the ramp are the easy half, and copying this one would take an afternoon once the
index exists.
