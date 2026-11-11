---
topic: interest-by-reach
triggers: [reach.rs, "net/reach.rs", INTEREST_RADIUS_M, reach_m, ReachIndex, TickTable, Outbound, PrimOutOfReach, "within reach", "out of reach", "absolute position", "avatar position", AvatarStateDto, placed_avatars, embodied_place, "area of interest", AoI, "interest management", snapshot_frame, encode_avatar_state, "crates/simulator/src/state.rs", "crates/simulator/src/net/mod.rs", "systems/net/network.rs", apply_network_snapshot, "federation stage 1"]
updated: 2026-11-11
---

# An avatar stands on Earth, and a connection is sent what is within its reach

Protocol 47, 2026-11-11: stage 1 of the network-of-servers design
(`docs/mockups/federation/README.md`, *One world, more people*). Two facts
about the simulator changed at once, and everything that measures a distance
or decides what a client is sent rests on them.

## The authoritative position is a latitude, a longitude and an altitude

`AvatarSim::place` is where an avatar stands on Earth; `step` integrates the
intent's velocity there, metres turned into degrees at the avatar's latitude
(`cartopolis_core::degree_scale_m`). `AvatarStateDto` replicates `geo` and
`alt_m`, and every client converts them into the frame its own map is anchored
in (`network.rs`'s `Seen`, through `lat_lng_to_world`), as it has always done
for a prim's `PrimDto::geo`.

Until protocol 46 the position was metres in the avatar's *own client's* map
frame, and the client's `ClientIntent::geo` was kept beside it as a second,
untrusted place for sensors and physics. Two clients agreed about where an
avatar stood only while they happened to share an anchor; Here chat, voice and
faces compared frames that differed; prim speech measured a prim's frame echo
against avatar frames (`research.md` §4 in the federation folder has the
survey). All of that now goes through one `reach::Place` and one
`Place::distance_m` — the ground distance and the height difference as one
line, so sixty metres up is as far as sixty metres across.

**An avatar is nowhere until its client says where it stands**: the first
`ClientIntent` carrying a `geo`, or a `Teleport`. That is what `embodied` used
to mean, with one change of consequence: an intent without a `geo` no longer
puts an avatar into the world. The web UI, which signs in to chat and never
says where it is, stays out of every snapshot and out of earshot, as before.
Once placed, **the intent's `geo` is ignored**: the sim is the authority, and a
client that wants to be elsewhere teleports. The client's prediction and the
sim's integration agree to a part in ten thousand over a reach — Mercator
metres at the anchor against the ellipsoid's own scale — which is less than a
millimetre a tick. The one client path that used to *walk* an avatar by
sending a changed `geo` was a test (`physical_prims.rs`); it teleports now.

The client places its avatar on its first online frame, whatever the camera is
doing (`send_network_intent`'s `placed`): in the orbit or free camera no intents
are sent, and without that one message the session would have had nothing
within reach — no things, nobody — until the person switched to the avatar
camera.

## Each connection receives what is within its reach

The tick leaves a `reach::TickTable` on the one broadcast channel: every
placed avatar's `AvatarStateDto` encoded once (`encode_avatar_state`), and
avatars and things in two latitude-sorted indexes. Every connection reads it
(`net/reach.rs::on_tick`) and assembles its own `WorldSnapshot` from the
states within `reach_m` of its avatar — `snapshot_frame` splices the encoded
states into one frame, pinned byte for byte to `encode_app_frame` by a test —
and keeps the set of things it has sent: a `PrimUpsert` for each that came
within reach, a `PrimOutOfReach` for each that left. The handshake's snapshot
is empty, and so is every tick's for an avatar that is nowhere — the empty
snapshot still goes, because it is also the heartbeat a resumed link is judged
by (`session::check_link_after_resume`, `RESUME_SILENCE_S`). **The world no
longer travels in the snapshot, and no client is sent anyone it could not see
or hear.**

`reach_m` is the shout radius plus a 200 m margin — 500 m on the ground — so
somebody walking towards you is drawn before you can hear them, widened by
twice the altitude for a flier and capped at 5 km. The camera's height does
not widen it: the orbit camera sends no intents and the wire carries no view
height, so the macro view shows the avatars within the avatar's reach, not
everyone on the server as it used to. Widening by the camera is the next
piece, when the stage that sends it (a view height on the intent, thinned past
the base radius) is wanted.

The channel carries `reach::Outbound`, not bare frames, so a connection can
route an item without decoding it: `Frame` for everybody (capacity, motion,
tones, sounds, console lines, looks), `Thing` with its place, `Removed` with
its id, `Tick` with the table. A thing's change is forwarded to the connections
whose last-measured reach holds it **and to the account that asked for it**,
wherever it stands: a create has no other answer, and an edit's answer must
reach the editor. `refuse_and_correct` still answers on the connection itself.

Two things are still sent to everyone and could be filtered later, if a
measurement says so: `PrimMotion` (at most 64 physical prims a world) and
`PrimSound`, both of which the client range-tests or ignores for a thing it
does not have.

## The index is a sorted list, and there are no cells

`ReachIndex` sorts by latitude and answers a query from the band of latitudes
a radius spans, measuring the exact ground distance to each entry in the band.
At the populations one server holds that beats rebuilding any tree twenty
times a second, and it has nothing to get wrong at a pole or across the
antimeridian (`reach.rs` tests both). The representation is private to
`reach.rs`; swap it when a measurement asks.

**Nothing cuts the Earth into cells, on the wire or in the database**, and
nothing may: zones as the unit of interest were drawn and rejected in the
design ("this still looks like a region-based design"), as was visiting
another server to see more. A reach is a circle about a person; reaches
overlap freely; no reach owns anything. The physics zones in `physics.rs` are
a Rapier detail — a world needs an `f32` origin — and are neither on the wire
nor an authority.

## What was and was not measured

Not measured yet: the tick's cost with the table against the old broadcast
(`tick timing` in the journal is the instrument, as
`simulator-tick-budget.md` says), nor the index against an R-tree. The old
numbers to beat are in that note: 0.063 ms a tick with no client connected.
The per-connection work is one `snapshot_frame` splice and two set
differences a tick, plus a read of the world only when a thing enters reach.

## Tests that pin it

- `reach.rs`: the circle, height as distance, invalid places, the antimeridian
  and the poles, the table.
- `state.rs`: nowhere until placed, the sim moves a placed avatar and ignores
  later reports, non-finite intents and teleports, the seat, the teleport, the
  replicated velocity.
- `tests/integration/presence.rs::things_come_within_reach_and_leave_it`: the
  empty handshake, a bench arriving on placement, leaving by `PrimOutOfReach`
  on a teleport to another city, and coming back.
- `cartopolis_core`: `snapshot_frame_matches_encode_app_frame`.
