---
topic: physical prims
triggers: ["physical prim", "physics", rapier, Rapier, SetPrimPhysical, PrimMotion, PrimBody, physics.rs, PhysicalMotion, smooth_physical_prims, set_physical, apply_impulse, set_vel, football, Euroborg, "kick emote", "Emote::Kick", GrabPrim, ReleasePrim, "pick up", carry, carrying, throw, core/src/carry.rs, net/carry.rs, can_carry, grab_prim, release_prim, holding, WorldAct, "protocol 48"]
updated: 2026-11-15
---

# Physical prims

A prim can be switched to **physical** — Second Life's checkbox — and from then
on the simulator moves it: it falls, bounces off other prims, rolls, and is
pushed by avatars walking into it. Wire protocol 34 (`PrimDto::physical`,
`SetPrimPhysical`, `PrimMotion`), migration V26 (`prims.physical`; V25 is `building_owners` on the building-designs branch, which must land and deploy first),
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

## Picking up, carrying and throwing (protocol 48, 2026-11-15)

Asked for by the owner ("oppakken en gooien"): walk up to the ball, pick it up,
carry it, throw it or put it down. `GrabPrim` / `ReleasePrim { throw }` on the
wire, `AvatarStateDto::holding` back; `SimWorld::grab_prim` / `release_prim`,
`physics::Carry` / `Release`, `net/carry.rs`, and the client's prompt in
`systems::interact`.

- **What can be carried** is `cartopolis_core::carry::can_carry`, one rule both
  ends call: physical, longest side ≤ 1.2 m, mass ≤ 15 kg at the physics step's
  own density (which moved to `carry` with `prim_mass_kg` so the client weighs
  a prim the way the sim does). A football (0.56 kg) yes; a 0.5 m crate (12.5
  kg) yes; a 1 m crate (100 kg) and a 2.4 m goal post no. **Size and mass both**,
  because density is uniform: a mass bound alone lets a 3 m plank through, a
  size bound alone a solid 1.2 m ball of 90 kg.
- **The sim checks everything.** Anyone may pick a thing up, as anyone may
  kick it — it is not an edit, so no owner is asked. Refused with a
  `ServerError`: not physical or too big (422), more than `GRAB_REACH_M`
  (2.5 m flat, −0.5…+2.3 m in height) from where *the sim* holds the avatar
  (422), seated (422), already held by someone else or hands full (409),
  no such prim (404). A grant is not answered: it shows as `holding` in the
  next snapshot, to everyone at once. Grabs share a 5/s bucket; a release is
  never throttled, since a dropped one would leave the thing stuck in the hands.
- **Carried is kinematic.** Each tick `SimWorld::step` hands the physics step
  every `(prim, carrier geo, feet, yaw)`; the body is switched to
  kinematic-position-based and stepped along a path to 1.1 m above the feet,
  just clear of the capsule's front, turning with the carrier. Kinematic bodies
  do not react to the carrier's (kinematic) capsule, so the ball is neither
  dropped nor pushed off by the body holding it; it does push other balls, and
  it passes through static prims. It moves through `PrimMotion` like any body,
  so every viewer sees it carried with no client change.
- **Throw** is the sim's: 8 m/s along the facing, 3.5 m/s up, slowed by
  √(2 kg / mass) above 2 kg, plus the thrower's own velocity, clamped to
  15 m/s. The client sends only `throw: true` — there is no speed on the wire
  to validate. **Put down** lowers it to the feet in front and lets go at rest.
- **What ends a hold** without asking: the carrier leaving (no carry that tick,
  so it drops where it was), a teleport, sitting (an intent with a seat), the
  prim deleted, made non-physical or rescaled past carrying. Swimming is the one
  the client asks for (a put-down when `AvatarState::swimming` starts), because
  the sim knows nothing of water.
- **Controls.** E on a loose physical prim: "Pick up …" (the `interact` clip).
  Holding, the prompt is two buttons, "Throw … [E]" (the `punch` clip, the only
  capture with a forward arm thrust) and "Put down [G]" (`interact`). Two
  buttons rather than a long press, because on touch a long press is already the
  context menu, and G because nothing else binds it. A pickable prim beats a
  scripted one in the prompt: the ball against a scripted goal is still the ball.

**What lost.** A client-sent throw velocity (aimed by the camera) — a speed the
sim would have to clamp and could not check against anything, for an aim the
avatar's facing already gives. A dynamic body held by a spring or a joint to the
hands — it would swing, collide with the carrier's own capsule and need tuning
per mass, for a sway nobody asked for (not built; reasoned, not measured). Only physical prims: carrying a non-physical prim
would be an edit by a non-owner.

**Not done here.** No carry pose (the arms play idle under a floating ball;
there is no carry take in the pack), and remote avatars' `holding` is not used
by the client at all — so the prompt offers "Pick up" for a ball in someone
else's hands and the sim refuses it (409). No MCP tool for it.

## Not done

- Collision events for scripts (`on_collide`) — the referee polls instead.
- A torus or a model prim collides as its bounding box; hollow, path cut and
  taper are ignored.
- The client draws avatars passing through prims (it has no prim colliders);
  only the server's capsules touch the ball.
- Nothing here has been seen on a screen yet: verified by the server's tests
  and an end-to-end goal over the wire, not by a capture.
