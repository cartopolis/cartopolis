---
topic: ambient-traffic
triggers: [traffic.rs, "ambient traffic", "cars driving", "moving cars", "traffic",
  DriveNetwork, drive_traffic, oneway_reverse, "streets layer", data_layers.rs,
  "Life layer", life_enabled, "cars move badly", "car trajectory", "traffic spread", Turn, headway, corner_speed, place_agent, cars_visible, people_enabled, "layer toggle",
  agent_models.rs, CarBody, signal_speed, signals_near, "brake lights", "walk cycle", "pedestrians"]
updated: 2026-10-19
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

- The models are built once at startup and shared by the whole pool: four car
  bodies of three meshes each and one walker of three (`agent_models`, see
  below). Until 2026-10-19 a moving car was the parked cars' seven boxes.
- An agent is an entity with a `Transform` and a few child parts. Moving it
  writes those transforms, and a braking car swaps its tail lamps' material
  handle — nothing else. **After startup this layer uploads nothing, ever.**
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

**Walk animation since 2026-10-19** — see "Looks" below. Before that a walker
was three boxes sliding along a line, on the argument that from tens of metres an
upright silhouette in motion was the whole signal; asked for realism, the
sliding was the first thing to go.

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

No car-to-car collision, no give-way, no avatar interaction, and people walk
through each other and through the cars and ignore the lights. (Queueing came with
headway on 2026-09-10, and stopping at signals on 2026-10-19 — see below.)
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
and that layer then keeps moving with the group switched off. Three call sites
use them: the two pools in `drive_traffic`, and the vehicle feed's poll and its
draw.

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

## There is no road hum (removed 2026-09-29)

`soundscape::update_road_hum` looped a noise clip whose volume followed the
loudest road near the camera. It is gone: it scanned every vertex of every drive
line in the loaded ring on every frame — the largest named main-thread cost in a
steady-state web profile, for a sound nobody could pick out from the wind. If a
city ambience comes back, it needs the proximity index below first and must not
be a per-frame scan of `DriveNetwork`.

## Water and playground ambience are not done

Both were proposed alongside the hum and neither is here. The reason is the same
for both: there is no resident, queryable source to hang them on. The roads are a
resource because `systems::traffic` needed one; water lives only as per-cell
tessellated geometry and shore rasters, and a POI's category reaches
`WorldPlaces` but nothing measures a *distance to the nearest park*. Either would
need a proximity index built first, which is the actual work — the synthesis and
the ramp are the easy half, and copying this one would take an afternoon once the
index exists.

## The motion, and what "rudimentary" meant (2026-09-10)

The first version of this layer answered "is this street alive" and stopped
there: a car read its speed straight off the road, its heading straight off the
polyline, and changed road by having all three of those replaced between two
frames. Every one of those is a **step function**, and a step is exactly what an
eye picks out of a moving picture. Four of them, in the order they are visible:

- **Speed was assigned, not eased.** A car handed from a residential street onto
  a trunk road went from 7.5 m/s to 22 m/s in one frame, and a recycled car
  appeared already at its road's speed. It is a rate now (`CAR_ACCEL` /
  `CAR_BRAKE`, and a walker's own gentler pair), and what it eases *toward* is
  the least of three things — the road, the bend ahead and the car in front.
- **A junction was a cut.** The hand-off replaced the line, the direction and the
  lane offset at once, so the car's nose snapped through the whole turn angle and
  its body jumped across the road by the two lane offsets — plus up to
  `HANDOFF_M` more where the two lines do not share a node. Roads meet at
  junctions constantly, so this was most of what the motion looked like. The
  junction is *driven* now (`Turn`): the exit pose of the old road and the entry
  pose of the new one are the two ends of a cubic Hermite curve whose tangents
  are the two travel directions, and the car follows it at a speed derived from
  how tight it is. The curve is exact at both ends by construction, so there is
  nothing left to pop. It is also the only place the layer models something the
  centre lines do not contain: **the corner itself is not in the data** — two
  lines meet at a point, and how far a vehicle swings getting from one to the
  other is set by how wide the two roads are.
- **The heading was the polyline's tangent.** A simplified line changes direction
  in steps, and a vehicle cannot: at every vertex the yaw jumped by the whole
  bend. The drawn heading is a rate-limited chase of the path's
  (`TURN_RATE_PER_MS`), and the rate is per m/s of the agent's own speed because
  a vehicle turns through a *radius*, not at a fixed rate.
- **Nothing slowed for a bend.** A simplified line has no curvature, only kinks,
  so the radius is the one an arc tangent to both segments would have if it were
  allowed to eat half the shorter of them; `v = √(a·r)` at `CORNER_LAT_ACCEL`
  turns that into a speed. `corner_radius` is shared with the junction turns, or
  a car that slows for a bend in a street and then takes the right angle at its
  end flat out would be worse than one that does neither.

And one that is not motion at all: a car was oriented by **yaw alone**, so over
the measured terrain it kept its wheels in the horizontal plane and sank into
every rise. The height was already read from `TerrainField`; the slope is one
more read of it (`normal_at`, which answers `Vec3::Y` wherever the field is off
or absent, so it costs nothing outside the Netherlands). People stay upright.

## Spread: a distribution, not an argmax

The 2026-08-26 fix for "one car in view" was **three uniform draws, keep the
fastest road**. Both halves of that are wrong, and it is worth writing down
because each is wrong in a different direction:

- A **uniform draw** makes a 30 m service stub as likely as a 400 m avenue, so
  most of the pool still went to back lanes. Length is most of what "how much
  traffic does this road carry" means.
- **Keeping the maximum of three draws** over a speed range of 4.5 to 26 m/s is
  very nearly deterministic. Once a trunk road was in range almost every recycled
  car went onto it and the ordinary streets emptied — the opposite failure, from
  the same fix.

So a line's share is its length (capped at the diameter of the placement disc,
since nothing past that is in range) times how much traffic a road of its class
carries — the speed ratio, *softened* by a 0.7 power, because a city where every
side street is empty is as wrong as one where the main road is. `weigh` builds
the cumulative table once per frame beside the candidate list; `draw_line` is a
binary search. Walkers weight by length alone: a busy pavement is busy because
the street is long, not because it is fast.

Two more things placement now does, both of which were losing agents outright:

- **It lands them in front of the camera.** `near` returns every line with *any*
  point in range, and placement then drew a distance along the whole of it — so a
  900 m avenue clipping the edge of the disc received cars anywhere along its
  length, most of them outside the recycle radius and thrown away on the next
  frame. `nearest_along` projects the camera onto the line and the draw is a
  spread around that.
- **It refuses a spot that is taken.** Two of one pool landing on the same street
  a metre apart draws one car twice, in the same paint, which reads as a
  rendering fault rather than as traffic.

## One thing the note above said this would never do

The "what this deliberately is not" list still holds for signals, give-way,
collision and avatar interaction. **Headway does not**: a car now sits behind a
slower one instead of driving through it (`HEADWAY_M`). The reason it is worth
the exception is that it costs nothing new — placement already needs a map of
where every agent is, keyed by its line, so "what is in front of me" is one
lookup in a table that is built either way. It is the only pair that can visibly
interpenetrate: two cars on one street at different speeds converge and then
overlap for as long as it takes one of them to be recycled. Walkers are exempt;
at 1.25 m/s two of them passing through each other is not what anybody is
looking at, and a pavement nobody can overtake on is worse than one where they
do.

There is no deadlock in it: the block is always by an agent with a strictly
larger distance along the same line in the same direction, so the chain is
finite and its head is never blocked.

**Still unverified, and by the same argument as before.** This container cannot
watch a moving frame, so smoothness, the junction curve and the absence of a pop
on recycle are reasoned about and pinned by unit tests on the pure parts —
`a_junction_turn_is_exact_at_both_ends`, `a_heading_turns_at_a_bounded_rate`,
`only_the_bend_ahead_is_a_reason_to_slow` — not watched. A still cannot show any
of it.

## Looks, lights and a walk (2026-10-19)

Asked to make the cars and people "more realistic, how they look and how they
move". What changed, and the reasoning that is not in the code:

**The moving cars stopped being the parked cars.** The parked form is seven
boxes because a lot merges hundreds of them into one cell mesh and every box is
multiplied by the lot (`parked-vehicles.md`). The moving pool has the opposite
cost shape — one shared mesh per model, uploaded once — so a model costs its
vertices once. `cartopolis_geo::agent_models` lofts each body from a table of
cross-sections (bonnet, raked windscreen, roof, rear window, boot), with round
wheels, bumpers, sills, mirrors, yellow plates, headlamps and tail lamps. Four
bodies — hatchback, saloon, estate, van, about 40/25/23/12 % by pool index, and
most vans white. The parked cars are unchanged; the claim "a moving car and a
parked one are the same object" no longer holds and was traded knowingly.

**A car is three meshes because of what a shared mesh cannot do.** The body
takes the paint material; the trim (glass, tyres, plates, lamps) has its own
neutral material with per-vertex colour, so a red car's windscreen is not red;
the tail lamps are alone so a braking car can swap their material
(`TailLampMaterials`) — a handle write, no upload. They are unlit and the braking
one is pushed past 1.0 for bloom. They light while the car slows harder than
0.9 m/s² and while it waits; the off threshold is lower so easing off does not
flicker them.

**The body leans.** Pitch from the eased acceleration (nose down braking), roll
from speed × yaw rate (out of the turn), eased over 0.25 s and capped at 0.05
rad. Read off the motion, not simulated.

**Cars stop at the drawn signals.** `street_detail` now keeps each signal's
node (world XZ) and phase group per cell (`signals_near`); `signal_speed` puts
a braking curve (3 m/s²) on a car whose own phase group shows amber or red ahead,
ending 4 m short of the node. Three things it has to get right, each with a test:
a cross street's lights at the same junction (other group, or >half-width+1 m to
the side) must not stop it; a car that would need more than 7 m/s² goes through
on the amber rather than stopping in the junction; and having crossed one stop
line, the *opposite approach's* line beyond the junction — same road, same group
— is ignored for 30 m (`signal_grace`). With street detail off there are no
signals and nothing stops: a car only obeys a light you can see. Queues form
behind the stopped car through the existing headway.

**People walk.** A walker is a torso with the head and four child limbs hung
from hip and shoulder; `agent_models::gait` swings legs in opposition and each
arm against its own leg, with a small dip as the legs pass. The phase advances
by **distance** (one cycle per 1.4 m, scaled by height), so the feet keep pace at
any speed. Heights vary ±8 %, trousers have their own palette, and walkers use
**both pavements in both directions** with up to a metre of spread — before, all
walkers kept to the pavement on their own right, so each side of a street was a
one-way stream. The head is still the neutral grey, for the reason above.

The black and dark-blue paints were added to `parking::CAR_PAINTS` too: they are
two of the four commonest Dutch car colours and the palette had no dark car.

**Not verified by watching**, as before: the pure parts are unit-tested
(`a_car_stops_at_red_and_drives_on_green`, `a_car_through_a_junction_ignores_the_far_stop_line`,
`the_body_dips_braking_and_leans_out_of_a_turn`, `walkers_keep_to_either_pavement`,
`agent_models`' winding and proportion tests); the models were checked in stills.
