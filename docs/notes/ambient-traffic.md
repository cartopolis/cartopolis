---
topic: ambient-traffic
triggers: [traffic.rs, "ambient traffic", "cars driving", "moving cars", "traffic",
  DriveNetwork, drive_traffic, oneway_reverse, "streets layer", data_layers.rs,
  "Life layer", life_enabled, "cars move badly", "car trajectory", "traffic spread", Turn, headway, corner_speed, place_agent, cars_visible, people_enabled, "layer toggle",
  agent_models.rs, crowd.rs, CrowdFigure, CarBody, signal_speed, signals_near, "brake lights", "walk cycle", "pedestrians", cars_barred, close_car_free_lines, "car-free", "motor_vehicle",
  cyclists, Cyclist, bicycle, BikePart, BIKE_FIT, bike_lean, cycle_ok, "riding poses", Ride, ride_poses, cyclists_enabled,
  crowd_react.rs, crowd_pose.wesl, make_way, keep_clear, WaveBack, CrowdPeople, CARTO_CROWD_HERE, CARTO_AVATAR_WAVE, "head turn", "wave back",
  give_way.rs, "systems::give_way", yield_to, zebra_speed, zebra_priority, SignalButtons, ButtonPhase, button_operated, "push button", "zebra crossing", "give way", CARTO_TRAFFIC_HERE, CARTO_PRESS_BUTTON,
  Fatbike, fatbike, FATBIKE_WHEEL_R, choose_way, defiant, make_way_ahead, Crossing, crossing_ahead, safe_to_cross, signal_lets_walk, closing_walkers, pass_cyclist, hand_off, STRAIGHT_ON_WEIGHT, ride_the_tracks, kind_cycles, foot_down, ride_stop, traffic_hours.rs, "systems::traffic_hours", OFF_DUTY_M, "rush hour", "time of day",
  traffic_physics.rs, "systems::traffic_physics", TrafficPhysics, look_ahead, gives_way, collide_traffic, "collision", CRASH_CLOSING, CROSSING_PATIENCE_S, rapier, deviation, MAX_CORRECTION, CARTO_TRAFFIC_OVERLAPS, CARTO_KNOCK_RIDER, fallen, "falls", path_conflict, model_size, "jerky", "stuck"]
updated: 2026-11-18
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

## Streets closed to cars come from Overpass, not the tile (2026-11-02)

Shortbread's `streets` layer has **no access tags** (its keys: `kind`, `link`,
`rail`, `bridge`, `tunnel`, `oneway`, `oneway_reverse`, `surface`, `service`,
`bicycle`, `horse`). Groningen's Vismarkt is `highway=residential` +
`motor_vehicle=no`, as are Folkingestraat, Stoeldraaierstraat and most of the
centre, so the tile served them as ordinary streets and cars drove between the
market stalls. The BGT paving was checked as a stand-in and is not one: it calls
most of the Vismarkt `voetgangersgebied` but one strip `rijbaan lokale weg`,
where OSM closes all of it.

The street-detail query (`systems::street_detail`) now also asks for every
drivable-kind highway carrying `access`, `vehicle`, `motor_vehicle` or
`motorcar`; `cartopolis_geo::traffic::cars_barred` reads the most specific of
them, and `close_car_free_lines` turns `drive` off on any line whose samples lie
mostly within 4 m of a barred way (`runs_along_closed`). The two arrive on their
own schedules, so it re-matches on either revision, and recomputes from
`kind_drives` so a closure that streams out reopens its street. Walkers keep the
line and, with `drive` off, walk the middle of it. **The closures ride the street
detail toggle**: with that layer off, closed streets carry cars again.

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

**Walkers are MakeHuman people since 2026-11-04** ("The crowd" in
`makehuman-avatars.md`). Until then the head was a deliberate grey, on the
argument that a crowd in one complexion is a claim about a city's people that a
road centreline cannot support. Asked for real people instead, the owner chose
**every MakeHuman skin family dealt out evenly** (African, Asian and Caucasian
in turn over the bodies, the tone spread ±30) over keeping the people grey:
an even mix makes no claim about *this* street, and a grey person among
realistic avatars read as a placeholder.

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

No car-to-car collision and no give-way between vehicles at a junction.
(Queueing came with headway on 2026-09-10, stopping at signals on 2026-10-19,
walkers that notice avatars and cars and cyclists that give way to them on
2026-11-15, and walkers that cross at zebras and lights, traffic that stops
for them, walkers that step round each other and cars that stay behind a
cyclist on 2026-11-17 — see below.)
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
most vans white. The parked cars use the same bodies at a cheaper level of
detail (`parked-vehicles.md`), so a moving car and a parked one are the same
object again.

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

**People walk.** The phase advances by **distance** — one cycle per stride of
the body's captured walk (about 1.5 m), scaled by height — so the feet keep
pace at any speed. (From 2026-10-19 to 2026-11-04 the figure was a box torso
with swinging box limbs, `agent_models::gait`; now the walk is the motion
capture the avatars use, baked per body — `systems::crowd`.) Walkers use
**both pavements in both directions** with up to a metre of spread — before, all
walkers kept to the pavement on their own right, so each side of a street was a
one-way stream.

**People keep off the grass.** Two causes put them there. On a path, a walker
used the street rule — half the width plus 1.6 m out — so everybody in a park
walked on the lawn beside the path; a path is now walked *on*, within half its
half-width of the middle. And a street was assumed to have a pavement both
sides; `traffic::keep_walkers_off` now samples where each pavement would be
every 6 m against the grass layer's own `GrassArea::plantable` (the tile's
lawns and the survey's verges, minus anything a road or path is drawn over), and
a side more than 30 % grass has no pavement. A street with none loses its
walkers; one with a single side puts everybody on it, and a walker reaching it
from the other side crosses at the junction (`Car::keep_to_pavement`). The
grass area is built whenever the tier runs the people layer, not only with the
grass toggle on — a tier constant, so it never retears the cells.

The black and dark-blue paints were added to `parking::CAR_PAINTS` too: they are
two of the four commonest Dutch car colours and the palette had no dark car.

**Not verified by watching**, as before: the pure parts are unit-tested
(`a_car_stops_at_red_and_drives_on_green`, `a_car_through_a_junction_ignores_the_far_stop_line`,
`the_body_dips_braking_and_leans_out_of_a_turn`, `walkers_keep_to_either_pavement`,
`agent_models`' winding and proportion tests); the models were checked in stills.

## Cyclists (2026-11-15)

The third pool, asked for by the owner ("kunnen we ook fietsers toevoegen?"),
on the same terms as the other two: invented riders on surveyed ways, under
**Life ▸ Cyclists** (`cyclists_enabled`, `LayerPrefs::cyclists`), off in the
navigation display's minimal mode with the rest. Until then a walker on a
cycleway stood in for a cyclist, on the argument that inventing one would claim
the tile knows which it is — but every person in this layer is invented
already, and a Dutch street without bikes read as emptier than one with them.

**Where they ride** is `cartopolis_geo::traffic::cycle_ok`: a `cycleway`, and
the streets bikes share with cars from a living street up to a secondary road.
**Not a primary road**: in the Netherlands it has a cycle track of its own,
mapped as its own `cycleway`, and riders on the carriageway too would ride
beside the ones already on the track. A footway, path or pedestrian street only
with `bicycle=designated|yes`; `bicycle=no|dismount` closes any way, as the
router reads it. A cycle track takes 1.8× a street's share of the pool
(`CYCLE_TRACK_SHARE`). On a street a rider keeps 0.9 m in from the edge of the
carriageway — the cycle lane — and on a track to its right-hand half. One-way
streets are obeyed: the tile does not carry `oneway:bicycle=no`, so the
*tegenrichting* is not ridden.

**A rider is a crowd person posed on a bike, not a capture.** The pack holds no
cycling take and needs none (`cartopolis_human::crowd::ride_poses`): the
standing person is put on the saddle, the trunk leaned until the wrists reach
the grips at 90 % of the arm (a search over 0–45°), the head turned most of the
way back up, and each limb solved onto what it holds with the same two-bone
reach the phone in the hand uses (`reach`) — the ankles onto the pedals through
a turn of the cranks, with the foot pitched by an ankling curve, the wrists onto
the grips. Eight poses a turn (`RIDE_POSES`) are baked after the standing pose
into the same pose texture. **The bike is scaled to each body**
(`CrowdBody::bike_scale`), solved so the leg is 95 % straight at the bottom of
the stroke, as a saddle set right leaves it; the shortest starters ride a bike
about 0.8× the reference (`BIKE_FIT`, a 28-inch city bike for ~1.85 m).

**The rider wears its own material, not a tag bit.** The walker's `MeshTag`
bits were being taken for a wave and a head turn as this was built, so rather
than compete for them each body and level has a second `CrowdMaterial` whose
`cycle` uniform starts its cycle at
the riding poses (`cycle.x`) and names the standing pose (`cycle.y`); a
walker's starts at 0. `CrowdBodyGpu::wear` picks one by `CrowdFigure::riding`.

**Pedalling is by distance**, one turn per gear's development (4.6 m, scaled),
so the feet keep pace with the road; slowing (accel below −0.3 m/s²) or held at
a light the rider stops pedalling and coasts. The cranks and both wheels are
child meshes turned about their axles by a transform write; the frame
(diamond or step-through, paint from `BIKE_PAINTS`) is scaled once the body is
dressed. A rider **leans into a turn** by `atan(v·ω / g)`, capped at 0.35 rad
(`bike_lean`) — the opposite sign to a car body's roll.

Cyclists `drives()`: they slow for bends, stop at the drawn signals in their
phase group, and queue behind one another. **Occupancy is one table per kind**
since this change: before it, a car on a street queued behind the walkers on its
own pavement going the same way, at walking pace, and with bikes in the lane it
would have queued behind every one. Now a car follows cars and a rider riders,
and a car passes through a rider in the cycle lane beside it by the lane offset,
not by overtaking.

Not done: a rider waiting at a red light stays balanced on the bike rather than
putting a foot down; nobody parks a bike or gets on one; a cyclist on a street
with a mapped track beside it still rides the street if it is a secondary road
or smaller. **Not verified by watching**, as before; the pure parts are tested
(`a_shipped_person_rides_with_her_feet_on_the_pedals`, `cyclists_ride_their_own_network`,
`a_rider_leans_into_the_turn`, `cyclists_keep_to_where_bikes_ride`,
`a_bike_is_bike_sized_and_meets_its_rider_where_the_fit_says`).

## The crowd notices you (2026-11-15)

Asked for by the owner as the first step of "the city reacts to the player":
walkers that walked straight through the avatar and never acknowledged it.
`systems::crowd_react` holds the decisions as pure functions; `drive_traffic`
calls them while it moves each walker, so there is no second pass over the
pool. **Per viewer, nothing on the wire** — the crowd is invented on each
client. What they react to is every avatar drawn: the local one and the other
people's (`CrowdPeople`, filled by `gather_people` once a frame, one short
list as the crows' threats are).

- **Making way.** A walker whose line would pass within a 1 m berth of an
  avatar 6 m ahead (to 2.5 m past) steps to whichever side of it is nearer
  where the walker already is — so it keeps going the way it started when the
  person moves — at most 1.4 m off its line, eased in with distance and out
  behind (`make_way`, `ease_shift`, 0.8 m/s sideways at most). It faces along
  its sideways step too, so the move reads as a step. If it is still on a
  collision course close in, it slows, to a stop at touching distance.
  `keep_clear` is the last word: a walker's centre is never within 0.65 m of
  an avatar's (both radii), however the avatar moves — that is for somebody
  walking *into* a walker; the plan alone never needs it for a standing one
  (`a_walker_steps_round_a_standing_avatar_and_back` checks the shove is under
  1 cm for every offset from −1 to +1 m).
- **Looking.** On each new meeting within 4.5 m, 40 % of passers-by (seeded by
  walker and meeting, so a capture repeats) look at you for 1–2.5 s, if they
  saw you coming; the head turns at 2.6 rad/s and lets go past 65°.
- **Waving back.** When an avatar starts the `wave` emote, the walker within
  10 m that can see it (within 100° of its facing) and is nearest and most in
  front answers — and a third of the time the next one too — after a 0.45–1.15 s
  pause (the second a little later). It slows to a stop, turns to face the
  waver (3 rad/s), blends into the captured wave over 0.35 s, plays it (1.6 s),
  blends out over 0.45 s, and walks on. A walker that cannot stop and face the
  waver within 3.5 s, or ends up 16 m away, gives up.

### How the body does it

The crowd's poses are baked (`makehuman-avatars.md`, "The crowd"), so:

- **The head turn is a rotation in the vertex shader**, not a pose: each
  vertex's share of it (1 on the head and everything skinned to it — hair,
  hat; ½ on `neck_01`; summed over its four skin weights, so the throat
  blends) rides in the `w` of its position texel, which was unused, and it
  turns about the head joint as it is in that pose (`Crowd::pivots`, one per
  pose, so a walking head bobs and still turns about itself). The angle is
  five signed bits of the `MeshTag` (27–31, steps of 4.5°). Cost: a branch and
  a few multiplies per vertex for a walker whose head is turned; nothing for
  one whose is not; 640 B of uniforms per material.
- **The wave is ten more baked poses**, on the finer level only (a walker
  near enough to wave back wears it): first frame to last of the captured
  `wave`, which is one-shot, after the riders' turn of the cranks (walk 16,
  standing, ride 8, wave 10: `CrowdExtension::cycle.z` is where it starts,
  and only the walking material plays it). Tag bit 26 says "waving"; bits 0–11 are then
  the progress through it and 12–17 the blend from standing. Ten poses keep
  the waving hand within 4.7 cm of the capture at every frame (eight: 6.3 cm,
  twelve: 3.6 cm, sixteen: 2.1 cm — `the_wave_is_baked_finely_enough`).
- **Memory**, measured on the shipped pack: the finer level is ~2 080
  vertices, so its pose texture went from 25 to 35 poses (with the ride), +10
  poses × 2 080 × 16 B = +320 kB a body and ~+4.5 MB over the fourteen
  (measured before the riders landed: 16.8 MB uploaded at startup, from ~12 MB). The
  coarser level is unchanged. Nothing is uploaded after.
- **Per frame** it is a few dozen flops per walker per avatar near, and no
  allocation: `CrowdPeople` keeps its lists, the walkers answering a wave are
  a fixed array, and choosing them (a read-only pass over the pool) runs only
  on a frame somebody starts waving.

### What lost

- **Turning the whole body toward you instead of the head.** Cheaper (no
  shader change), but a walker walking on while its body points at you reads
  as crabbing, and it was not needed: the head joint's skin weights were
  already in the baked mesh.
- **A head turn baked as poses** (look-left, look-right): doubles the walk
  for one degree of freedom, and could not follow a moving avatar.
- **Baking the wave on both levels**: +~80 kB a body for walkers at 30 m and
  more, which never wave back.
- **Steering round the avatar on the walker's path data** (a detour line):
  the lateral offset already existed (`Car::offset`) and a shift on top of it
  needs no new geometry and returns to the pavement by itself.

### Seeing it

Two capture hooks, both on the moving world's clock (`anim_clock`):
`CARTO_CROWD_HERE=1` sends eight walkers at the avatar every 30 s (the
nearest walking line, four from each side 5–14 m off, aimed through it) and
logs once a second the nearest any walker came, the widest step aside, and how
many are looking and waving; `CARTO_AVATAR_WAVE=<s>` makes the avatar wave
every that many seconds. Verified 2026-11-15 on mac-1 (`--shot-mode avatar
--video-fps 5`, Herestraat 53.2157, 6.5682): walkers held exactly the 1.00 m
berth, stepped aside up to 1.00 m, one or two looked, and one stopped, faced
the avatar and waved back for ~2.4 s; again after the cyclists landed (same
shader, riders pedalling on the Gedempte Zuiderdiep, no shader error in the
log). Making those movies is also how
`--video-fps` stopped re-teleporting a repeated pose every frame
(`shot_harness::hold_repeated_poses`): the avatar was drawn metres from where
it stood in a third of the frames.

**Limits.** Walkers still pass through each other (two stepping aside the
same way can meet). No line of sight through buildings for a wave (10 m on a
street rarely crosses one). No nod. Cars give way to avatars, not to
walkers (next section). Frame time on
the phone not measured; the CPU and GPU costs above are counts. Cyclists do
not look or wave back (they give way, next section), and walkers do not make
way for cyclists: none of this reads them.

## Traffic gives way to you (2026-11-15)

The owner's second step of "the city reacts to the player", chosen as
*Oversteken en verkeer*: a button at the traffic light, cars that stop at a
zebra. `systems::give_way` holds the decisions as pure functions;
`drive_traffic` calls them for every car and cyclist (`drives()`), reading the
same `CrowdPeople` list the walkers do (now with each avatar's facing). **Per
viewer, nothing on the wire**: the traffic is invented on each client, so a
pressed button holds the cars this client draws and nobody else's.

**What yields, and when.**

- **Somebody on the road ahead.** A car or cyclist whose path passes within
  its half width + 0.35 m (the avatar) + 0.45 m of an avatar up to 40 m ahead
  brakes on a 3 m/s² curve that ends with its nose 1.6 m short, waits, and
  pulls away at its usual rate once nobody is in its path (`yield_to`). The
  path is the agent's own line: the avatar is projected onto it
  (`traffic::on_path`), so somebody on the pavement of a bend is not in the
  way; straight ahead inside a junction turn and past the line's end. If the
  curve cannot be met — somebody stepped out close in front — it brakes at
  8 m/s², and the step is clamped so the nose never passes the avatar
  (`Yield::room`): `a_car_never_drives_through_somebody_who_steps_out` holds
  it from 4 m to 14 m at 11 m/s. Bodies: a car 0.95 m half wide with its nose
  2.3 m ahead of its centre, a bike 0.35 and 0.95.
- **A zebra somebody waits at or is on.** A car or cyclist with a zebra across
  its road up to 45 m ahead stops with its nose 1 m before the stripes when an
  avatar is on them, or within 2 m of either kerb end (±2.85 m along the road)
  facing within 60° of straight across (`waiting_at`). A driver who would need
  more than 7 m/s² goes, as at an amber light; the avatar on the stripes is
  still given way to by the rule above, which has no such limit.
- **A pressed button.** At a signal tagged `button_operated=yes` the prompt
  offers **Press the button** (within 2.5 m of the drawn pole or the mapped
  node; E or a tap; the avatar plays `interact`). The cycle (`ButtonPhase`):
  3 s nothing, 3 s amber for the cars, 1.5 s all red, 8 s the walking man
  green, 3.5 s red again while the crossing clears — cars held 16 s. Every
  signal of the pressed one's phase group within 30 m is held (both
  approaches' stop lines and the poles at either kerb), through the existing
  `signal_speed` — a held light is red to it whatever the global cycle says. A
  second press mid-cycle does nothing. The prompt reads `(wait for green)`,
  then `(walk)`.

**Where the crossings and signals come from.** All from the street-detail
layer's Overpass/atlas cells — no query change, the tags were already in them;
with street detail off nothing stops, as for the signals. Measured over the
nine z14 cells round Groningen's centre on 2026-11-15: 250 marked crossings,
of which **152 are zebras** by `street_detail::zebra_priority` — 97
`crossing=zebra`, 55 `crossing=marked` + `crossing:markings=zebra`; the rest
are `marked` with `yes`, `dashes`, `dots` or `surface` markings, which give the
walker no priority in Dutch law and are only painted; 11 zebras under signals
(`crossing:signals=yes`) are the signals' business. 266 more are `uncontrolled`
without a zebra ref and stay undrawn. Of 300 `highway=traffic_signals` nodes,
**15 carry `button_operated=yes`**, nearly all on footways at the kerb
(Westersingel × Verlengde Visserstraat, Pleiadenlaan, Eikenlaan,
Boumaboulevard). Only those get a button: most Dutch pedestrian lights have
one, but the data says so for these, and a button invented on the others
would be a placement the data does not have.

**How a pressed crossing is drawn.** A button pole now carries a pedestrian
head below the car head: the red walking man lit (`PropClass::PedRed`, one
more shared material), the green dark, and a yellow button box at 1.1 m. The
car heads are six shared materials cycled by one clock, so one crossing cannot
change colour through them; while a press holds the cars, `draw_buttons`
spawns, per held signal, a dark face over the car head with its own red and
amber over it, and over each button pole's walking man its own red and green.
One cube mesh and four materials made on the first press, a few entities per
press despawned when it ends: nothing uploaded after the first.

**What lost.**

- **Taking the button from every signal.** Most Dutch pedestrian lights have
  one, but 285 of the 300 do not say so; see above.
- **A button-operated signal resting green for the cars.** That is what a
  mid-block push-button crossing does, but the heads are drawn in the shared
  cycled materials; resting them on green needs a second set of six and a
  rule for which signals rest. Not done: after a press the cars go back to the
  global cycle, which may well be red.
- **Walkers of the crowd using the zebras.** They do not cross mid-block at
  all — a walker changes side only at a junction (`keep_to_pavement`) — so
  there is nothing for the cars to stop for until they do; sending them
  across at zebras is a path change, not a give-way.
- **Replicating presses.** A button on the wire would hold no other viewer's
  cars, which are their own invention.
- **A separate yield pass.** The decisions run inside `drive_traffic`'s one
  pass over the pool, with the people as one short list; the zebras and
  signals are each one `Vec` a frame from `StreetDetail`.

**Seeing it.** `CARTO_TRAFFIC_HERE=1` sends three cars at the avatar every
25 s (the drivable line nearest it, 45/60/75 m before it, a direction the line
may be driven) and logs once a second how near the nearest came and its
speed; `CARTO_PRESS_BUTTON=<s>` presses the nearest button within 12 m every
that many seconds. Filmed 2026-11-15 on mac-2 (`--shot-mode avatar
--video-fps 5`): on the zebra on the Oostersingel (53.220992, 6.574746 — the
stripes are in PDOK's photograph) the oncoming queue stops before the stripes
and waits, the near lane and a cyclist too, and pulls away once the avatar is
on the pavement; at Westersingel × Verlengde Visserstraat (53.21828, 6.55610)
the prompt goes from "Press the button" to "(wait for green)" to "(walk)", the
walking man turns green on the pressed pole only (the other pole is the cross
street's), and a car waits at the stop line. **Not shown**: the car head's
red overlay at a readable size (it faces the traffic, away from that camera).
The avatar's own clips are not on the movie clock, so its reach plays at its
real rate. Limits: the avatar's walking speed is not read, so a car does not
anticipate somebody about to step out; walkers are not given way to; a
cyclist on a cycle track crossing a zebra's road is a different line and is
not stopped by that zebra unless the zebra is mapped across the track too.

## The street sees itself (2026-11-17)

Asked "can the cyclists, walkers and cars move through the city more
realistically?", the owner took all three proposals — traffic that notices
itself, the small things, the hour — and added **fatbikes that do not keep
to the rules and ride the pavements**. Five commits; what is not in them:

**Walkers cross** (`Car::crossing`, `crossing_ahead`, `safe_to_cross`).
Before, a walker changed sides only at a junction, by teleporting its
offset through the `Turn`. Now one passing a zebra or a light across its own
street, with a pavement on the far side, stops at the kerb 45 % of the time,
faces the road, and looks (0.8 s at least). At a zebra it steps out once no
vehicle is on the stripes and every one coming could stop at 2.5 m/s² — a
braking a walker would trust, gentler than the 3 m/s² cars plan with, so a
car that has to stop for it can. At a light it walks from a second into its
road's red to three seconds before green (`street_detail::signal_lets_walk`;
the pedestrian heads are not drawn per phase, so this is the walkers' rule,
not a lamp), or while a button holds it. It gives up after 30 s. Across, its
offset runs from its pavement to the other at 1.15× its pace, then it walks on
the way it was going; `last_cross` keeps it from crossing the same stripes
straight back, which a deterministic draw would otherwise do.

**Vehicles stop for them** through the paths the avatars already had: a
walker waiting at a zebra is in `zebra_speed`'s list facing across, and one on
the road is in `yield_to`'s. Both are gathered in the occupancy pass, from
last frame's states — so are all the vehicles' positions and speeds for
`safe_to_cross`. That pass now reads a pose for every walker and vehicle (it
read none for cars and walkers before): about 480 line lookups a frame with
every pool on.

**Walkers step round each other** — `make_way` with the walkers it is
closing on added to the avatars, found through a 6 m grid. *Closing* is the
point: a walker steps round somebody coming the other way or dawdling ahead,
never the one beside it at its own pace, or a pavement of people walking
together jitters sideways for ever. A standing walker makes way for nobody.
They still overlap when two step the same way, rarely.

**Cars pass cyclists or stay behind** (`pass_cyclist`). The lane offsets put
a car and a bike in the cycle lane 0.6 m apart, centre to centre, on a
5.5 m residential street — a car drove through every rider it caught. Now a
car moves over by what it needs for 0.6 m of air between the bodies if that
takes no more than a quarter of the oncoming lane (or, one-way, all but
0.3 m); otherwise it follows, settling ~8 m behind at the bike's pace. It
does not look for oncoming traffic before it moves over. The two tables stay
separate (`AgentKind::index`); the car reads the cyclists' one.

**Traffic turns** (`hand_off`, `STRAIGHT_ON_WEIGHT`). The 2026-09-10 fix for
cars turning off main roads at random — take the way nearest straight on —
overshot: nothing ever turned. It is a weighted draw now, 2.5 for carrying
on within ~25°, 1 for each turn, times `importance`, so a main road keeps
four in five of its cars past a residential side street.

**Cyclists take the track** (`ride_the_tracks`). Shortbread maps a main
road's cycle track as its own `cycleway`, so riders went down both. A street
with a cycle-only line running its way within its half width + 8 m along
60 % of it is off the cyclists' network, recomputed from `kind_cycles`
whenever the lines change. Fatbikes ignore it, as they would.

**Fatbikes** (`AgentKind::Fatbike`, `agent_models::fatbike`). Twelve, under
the Cyclists toggle. The model is built on `BIKE_FIT`'s saddle, bottom
bracket and grips so the riders' baked poses sit on it unchanged; the rest
is a fatbike's (20-inch fat tyres, battery, bench seat, suspension fork,
tall flat bar), in matte paints. They may use any cycle or walking line,
ride one-way streets either way, at 26 km/h (18 on a footway or pavement,
each ±15 %); on a street with a pavement 55 % ride the pavement; 70 % run
red lights and pass zebras (`defiant`). None waits for anybody: it weaves
(`make_way_ahead`, 11 m ahead, 1.8 m/s sideways) round walkers, riders and
avatars, slows only when it cannot get by, and `keep_clear` keeps it out of
them. Walkers make way for and glance at fatbikes as at avatars — too late
to do much. The rider's feet rest on level cranks: the motor pedals.

**A foot down** (`human::crowd::foot_down`, `CrowdBody::ride_stop`). A
held rider used to balance on its pedals however long the light. Each body
now bakes one more riding pose: slid 0.15 m forward and 0.08 m down off the
saddle, hands on the grips, right pedal at 45° to push off from, left foot on
the road beside the bike — solved in the bike's frame, where the road rises
toward the lean, with the bike leaning just enough for 97 % of the leg to
reach it (13.1° for the shipped woman). Seated, by a reckoning on paper
rather than a solve, it took past the 0.4 rad allowed: riders slide off the
saddle to stop. The riders' material names it as their standing pose,
so `standing` blends the foot down with no shader change; 36 of the 40 poses.

**The hour** (`systems::traffic_hours`). The pools are a fixed size, the
busiest a street gets; a share of each is out at the scene's hour
(`DevPanelState::time_of_day`, the sun's clock — a `--time` capture sets it)
on a weekday or a weekend day (from `WorldClock`). The curves are a shape
drawn by hand to how Dutch city traffic is generally known to run — two
commuter peaks for cars and bikes, a midday-afternoon one on foot, the
fatbikes after school and into the evening, a late slow weekend — **not
fitted to any count**. An agent past the quota leaves the road at the end
of a line more than 80 m from the camera (`OFF_DUTY_M`), so a thinning
street never pops in view, and is not placed again until the hour fills.

### What is not done

Vehicles still do not give way to each other at a junction or look for
oncoming traffic before passing; nobody crosses mid-block; walkers do not
wait at a light at a junction without a mapped crossing; nobody parks or
gets on a bike; fatbikes have no passenger, though the seat has room.

## Bodies and collisions: Rapier in the client (2026-11-18)

Shown the street that sees itself, the owner asked that nothing pass
through anything — "that should be a collision; we have a physics engine
for that". Offered steering-only avoidance (which is what a physics engine
would only *detect* for us) and a physics engine in the app, the owner
chose the engine. `systems::traffic_physics` is it.

**Rapier, not Avian, and not the simulator's world.** Rapier was already in
the tree (`physical-prims.md`), pure Rust, and is linked with the
simulator's features so the two share an artifact. Avian is Bevy's ECS and
would fight `big_space`: its bodies live in `Transform`s, which here are
cell-local and jump a whole cell. The traffic is invented per client, so the
server's prim world cannot see it, and this one sees no prims.

**One world near the camera.** Metres from an origin set by the first body,
moved (bodies and all) once the camera is 4 km off. No gravity: bodies stand
at the heights the traffic draws them at — a car on a bridge and a walker
under it do not meet — and a knocked one is locked to its height and
upright. No map geometry, as for the prims.

**Kinematic while driving; Rapier only answers.** `drive_traffic` places each
agent's body every frame (`place`); `collide_traffic`, after it, steps the
world. Before that, each vehicle sweeps its own box ahead (`look_ahead`) by
its stopping distance + 3 m, at most 20 m: a straight sweep leaves the road
at a bend, and longer found pavements. What it hits decides `gives_way`:

- somebody on foot or an avatar: stop 1 m short — a fatbike weaves instead;
- a vehicle going its way: follow; one oncoming within 0.5 m of its lane:
  stop; one oncoming further over: carry on and brush past (cars are 1.64 m
  wide here, narrower than drawn, so two on a narrow street rarely touch);
- crossing paths: **right before left**, with 6 s of patience
  (`CROSSING_PATIENCE_S`) — two that each have the other on their right
  would otherwise wait for ever. A defiant fatbike gives way to nobody.

**Dynamic when they collide.** A pair beginning to touch at more than
1.2 m/s along the contact normal (`CRASH_CLOSING`) is a collision; slower is
a brush, and two people on foot never collide. Both become dynamic with the
velocity they had, Rapier shoves them apart by mass (car 1200 kg, bike
95–110, person 75), friction stops them, and after 3 s they are kinematic
again and ease back to their lines over 1.5 s. An avatar's body is kinematic
always: where an avatar is, the server says. Each collision is logged
(`traffic collision: …`), which is how a capture counts them.

**Not done.** No damage, no hazard lights, nobody gets out; a person hit is
shoved, not knocked over; vehicles do not swerve, only brake.

### Dynamic, not kinematic (2026-11-18, the same day)

The owner still saw cyclists ride through cars. With kinematic bodies Rapier
only *reported* touches; nothing pushed two apart, and every give-way rule
added to keep them apart made a deadlock elsewhere (two oncoming in each
other waited nose to nose for ever). Measured with `CARTO_TRAFFIC_OVERLAPS=1`
(pairs deeper than 0.2 m, once a second, over the whole ring) on 20 s at the
Oostersingel zebra: 20–45 a second.

Every agent body is **dynamic** now. `place` sets it moving to arrive where
its way puts it by the next step, at most `MAX_CORRECTION` (1.5 m/s) off its
own velocity; Rapier's solver decides where it gets, and the agent is drawn
there (`deviation`). Held back more than 0.5 m, its way waits for it. Three
traps, each one of the measured causes:

- **A jump is the way moving, not the body being far from it.** A bike
  shoved 3 m off its spot was "teleported" back — inside the car.
- **Rapier's shape cast drops a hit it starts inside** unless
  `stop_at_penetration` is set; a car already touching the one ahead saw
  nothing and drove on. With it set, only a body whose centre is ahead counts.
- **Put down blind.** Placement checks the bodies Rapier has stepped, so a
  fresh ring put hundreds down in one frame on top of each other: at most 24
  a frame now, and two still found 0.6 m inside each other without a
  collision have the later put down again elsewhere (a pop, rare).

After: 16 in the first second while the ring fills, then 0–2.

**Riders fall** (asked for: "a cyclist that hits a car falls"). A rider in a
collision with a car, or with anything above 5 m/s, goes over onto its side
away from what hit it (0.45 s, about the wheels' contact line), lies 5 s,
and gets up over 1.2 s (`fallen`). Walkers are shoved, not knocked over: the
crowd has no fall pose. `CARTO_KNOCK_RIDER=<s>` knocks the rider nearest the
avatar off every that many seconds, for a capture.

### Smooth, and no jams (2026-11-18, later)

"Jerky, unnatural; avatars float." Four causes, each measured on 10 s at
15 fps from a free camera over the Oostersingel zebra (53.22099, 6.57448,
12 m) with `CARTO_TRAFFIC_OVERLAPS=1`, which also logs per kind how much the
drawn step changes frame to frame and every vehicle standing 3 s near the
camera with what holds it (`why`):

- **Rapier stepped less than the frame.** At most 1/20 s while the traffic
  moved on by the whole frame; at 5 fps or on a slow phone every body fell
  behind its way and was pulled back. Now the frame in steps of ≤ 1/30 s.
- **Drawn as way + push.** Jumped up to 2 m when the two disagreed; drawn
  where the last step left the body now (`place` says when it was put
  rather than driven, and then the way is drawn).
- **One box for every car.** A cyclist stood inside a drawn van Rapier
  thought clear of it; each car's box is its model's bounds (`model_size`).
- **Collisions made the jams.** Two knocked bodies stood 3–5 s with a queue
  behind; most were a car's nose into somebody *crossing* its path, which
  the sweep ahead cannot see in time. `path_conflict` looks where every two
  straight paths meet within 4 s: the later waits, together right before
  left, always for somebody on foot. 13 collisions in the 10 s became 5–8
  (over the whole ring, ~350 agents); the stuck report near the camera
  shows only a zebra and a person in the way.

The floating figure in the earlier films was the capture's own avatar,
in fly mode, its clips not on the movie clock: film traffic from a free
camera (`--shot-mode free --look-at`), not `--shot-mode avatar`.
