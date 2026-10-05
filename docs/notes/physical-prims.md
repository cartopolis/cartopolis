---
topic: physical prims
triggers: ["physical prim", "physics", rapier, Rapier, SetPrimPhysical, PrimMotion, PrimBody, physics.rs, PhysicalMotion, smooth_physical_prims, set_physical, apply_impulse, set_vel, football, Euroborg, "kick emote", "Emote::Kick"]
updated: 2026-10-05
---

# Physical prims

A prim can be switched to **physical** — Second Life's checkbox — and from then
on the simulator moves it: it falls, bounces off other prims, rolls, and is
pushed by avatars walking into it. Wire protocol 34 (`PrimDto::physical`,
`SetPrimPhysical`, `PrimMotion`), migration V25 (`prims.physical`),
`simulator::physics`, and the client's `prim_geo::smooth_physical_prims`. The
reason it exists is the `football` content pack: a ball and two goals on FC
Groningen's pitch in the Euroborg.

## Decisions, and what lost

**Rapier, on the server only.** Compared 2026-10-05 against Avian and Jolt.
Avian is the more natural fit inside Bevy and was ruled out because it *is*
Bevy's ECS — the simulator has no Bevy and must not grow one. Jolt (and PhysX)
are C++ behind `cc`, the toolchain cost that already kept Opus out of the
voice codec. Rapier is pure Rust and Bevy-free. `bevy_rapier` is not used: the
client simulates nothing.

**The server decides; no client predicts.** A ball everybody kicks has to be one
ball. Ownership-by-whoever-touched-it (the usual social-VR answer, which would
have let collisions use the client's map) breaks the moment two people reach the
ball at once. The cost is that the kicker sees their own kick a round trip late;
client-side prediction of the kicker's ball, with the same Rapier code in a
shared crate, is the next step if that turns out to matter.

**Prims collide with prims, avatars and the ground — never with the map.** The
simulator holds no map data and is not going to (buildings, terrain and street
furniture stream only to clients). A pitch, goals and ramps built out of prims
are the whole collision world, and the ground is the flat `motion::GROUND_Y`
plane the sim already stands avatars on. Terrain relief is therefore invisible
to physics: a ball on the Hondsrug rolls on a flat plane under the drawn slope.

**`simd8` is on; `parallel` and `enhanced-determinism` are off.** SIMD costs
only cross-platform bit-identical results, which a single authority does not
need. Multithreading is off because the VPS's eight cores are shared with
nominatim, overpass and the build slots, and a pitch is a few dozen contacts.
There is no GPU on the host; Rapier's GPU work is not a production rigid-body
solver anyway.

## Frames

Prims are placed by geo anchor + altitude (ADR-029), so there is no world-wide
metre frame, and Rapier is `f32`. The world is cut into **zones** of 0.01°
(~1.1 × 0.67 km at Groningen), each its own Rapier world in metres from the
zone's centre, axes as the client's (x east, y up, z south). Zones exist only
while a physical prim is in one; a body that rolls out is re-inserted into the
neighbour with its velocity. Static prims and avatars are gathered from the zone
plus a 0.002° margin.

Avatars push as **kinematic capsules** placed at their `ClientIntent::geo` fix
(the client's own, sent up to 30 Hz), interpolated across the three physics
steps in a tick. An avatar that has sent no fix pushes nothing.

## How changes are noticed

No edit path tells physics anything. Each tick `Physics::step` compares every
physical prim with the pose it last wrote into it (bit for bit), and a
difference is an edit or a script move: the body is teleported there and held
kinematic for `HOLD_AFTER_EDIT_S` (1 s), so a builder can lift a ball and let
go. Static prims are hashed per zone; a changed hash rebuilds that zone's
static colliders. This is why `SimWorld::replace_prim` keeps the in-memory
`physical` flag — the row it is handed may be older than a script's
`set_physical`.

## Numbers

- Step 60 Hz (three per 20 Hz tick, at most six when catching up). At 20 Hz
  alone a shot at 15 m/s moves 0.75 m a tick — wider than a post. CCD is on
  for every dynamic body; `a_shot_does_not_tunnel_through_a_post` fires a ball
  at 40 m/s at a 12 cm post.
- Density 100 kg/m³: a 22 cm ball is 0.56 kg (a real one is 0.43), a 1 m crate
  100 kg.
- A kick (`Emote::Kick`, key 5) reaches 1.5 m in front of the feet and adds
  14 m/s along the facing plus 3 m/s up, scaled down for anything over 2 kg.
- `MAX_PHYSICAL_PRIMS` = 64 per world, refused at the switch.
- `PrimMotion` costs ~40 bytes per moving body; a body at rest is sent once
  with `resting` and then not at all.

## On the client

`PrimMotion` updates `PhysicalMotion` in place (not a `PrimUpsert`, which would
rebuild the mesh twenty times a second). `smooth_physical_prims` carries the
body along its velocity for up to 0.2 s past the last pose and eases what is
drawn toward that; a jump over 4 m is drawn as a teleport. `place_geo_prims`
skips prims that have a `PhysicalMotion`, because two systems inserting one
`GridCell` in a frame race on whose command lands last. A full `PrimUpsert`
removes the component, so an edit is placed from the upsert.

## Not done

- Collision events for scripts (`on_collide`) — the referee polls instead.
- A torus or a model prim collides as its bounding box; hollow, path cut and
  taper are ignored.
- The client draws avatars passing through prims (it has no prim colliders);
  only the server's capsules touch the ball.
- Nothing here has been seen on a screen yet: verified by the server's tests
  and an end-to-end goal over the wire, not by a capture.
