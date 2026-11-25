---
topic: driving
triggers: [driving.rs, systems::driving, core/src/car.rs, cartopolis_core::car, CarParams, CarState, CarInput, Garage, CarSpot, draw_cars, fold_taken_cars, ClientIntent.driving, AvatarStateDto.driving, Driving, CarKind, MAX_DRIVE_SPEED, wish_for_speed_capped, sim_speed_for_wish_capped, "drive a car", "get in", "get out", "call my car", "parked car", "Fly camera", "fly mode", CameraMode::Free, next_camera_mode, "protocol 52"]
updated: 2026-11-25
---

# The avatar drives a car, and the camera no longer flies

Asked for by the owner on 2026-11-25: take the Fly camera out, and let the
avatar travel fast in a car instead, GTA-style, with handling that feels like a
car. The inspiration was cherwellvalleymotorclub.app (a village driven in from
LIDAR and OSM), whose cars understeer when pushed and roll into a field when
pushed harder. Which cars: **any parked car one walks up to, and one's own car,
called to where one stands** — not the moving traffic. That reverses the
2026-10-06 decision "no drivable in-world car" (car routing still needs a GPS
fix; that part stands).

## The handling is a bicycle model with tyres that slip (`cartopolis_core::car`)

Two axles, a slip angle each, a simplified Pacejka curve per axle (peak near
7°, falling to ~90 % past it), weight that moves with the longitudinal force,
a friction circle shared between driving and turning, an engine with a torque
curve, gears and an automatic box, ABS-capped brakes biased 65/35, drag and
rolling resistance, and a handbrake that locks the rear. Under 1.5 m/s it
blends into the kinematic model, where slip angles are noise. It steps at
240 Hz: the tyre forces are too stiff for a frame-rate step. Four builds:
hatchback (front drive), saloon (rear drive), estate, van.

Two traps the tests now hold:

- **Weight transfer must come from the force, not from `d(forward)/dt`.** The
  latter includes the turning frame's `lateral · yaw_rate`, which in a slide is
  tens of m/s² of "braking" no tyre applied: it moved the weight onto the nose,
  unloaded the rear and spun the car out of every slide it entered.
- **Without load sensitivity a nose-heavy car does not understeer.** With one
  friction coefficient the axles let go together and a bend taken too fast was a
  four-wheel drift. A tyre pressed harder grips less per newton
  (`LOAD_SENSITIVITY`), and that is what makes the front go first.

Measured by the tests (`car::tests`): a hatchback does 0–100 km/h in 7–14 s and
tops out at 171 km/h; the saloon 227; 100–0 in 32–50 m; full lock at 90 km/h
runs wide at about 1 g with the front past its grip and the rear holding.

Steering on keys is on/off, so the lock the driver reaches falls with speed
(`steer_limit`), and the wheel takes 0.3 s to reach it. The lock still leaves
more than the tyres can take: pushed, the car runs wide rather than refusing.
Reverse is the brake held at a standstill for 0.35 s, as in arcade games; one
pedal less to find on a phone.

## The sim integrates the car's velocity, capped at a car's speed (protocol 52)

The sim has no map and so no walls; it cannot run the car. The client runs it,
meets the walls itself, and sends the resulting velocity as an ordinary wish.
`ClientIntent::driving` says it is at the wheel, and the sim then caps the wish
at `motion::MAX_DRIVE_SPEED` (70 m/s) instead of a run's 8, grants no jump and
holds no seat. Everybody else draws the car from `AvatarStateDto::driving`:
kind, paint, front-wheel angle and brake lights; speed and heading are the
avatar's own velocity and yaw. The prediction and the sim share
`sim_speed_for_wish_capped`, as the walk shares `sim_speed_for_wish`.

The online smoothing was built for flight at 640 m/s and is a duration, not a
distance (`avatar-prediction-smoothing.md`), so it needed nothing for a car.

## Getting in and out is a walk, not a teleport

The car stands a metre or two from the avatar. A teleport would re-anchor and
play an arrival for that; instead `driving::drive` walks the avatar to the
car's middle at 2.2 m/s (the sim moves it like any walk), and out to a spot
beside the driver's door (left, a Dutch car), or the other door if a wall is
there. Asked to get out at speed, the car brakes to a stop first.

The body at the wheel is the **sit clip on a seat offset** (`resources::Seated`
set every frame to the driver's seat), the bench's mechanism: no new clip, and
remotes draw it from the same offset (`network::remote_seat`).

## Cars on the map

- **A parked car is a prompt now** ("Drive this car"); `world_places` keeps
  each one's body and paint (`ParkedCar`). The car park is one mesh per cell,
  so taking a car **folds its vertices to a point** in that mesh
  (`fold_taken_cars`) rather than rebuilding the cell; its colliders drop out
  of `WorldPlaces::props`. Taken spots are in the anchored frame and are
  forgotten on a re-anchor, when the cells stream afresh.
- **The car one leaves is kept**, drawn and solid, until another is taken or the
  map re-anchors (`Garage::left`). One at a time.
- **"Call my car"** (C, or the prompt's button when no car of one's own is
  within 40 m) puts one's own car beside the avatar on whichever side has room.
  Its look is `Garage::own`, a red hatchback until there is a way to choose.
- Every drawn car hangs off an avatar entity (the driver's, or the local one for
  a car standing still, placed by its offset), so none needs a grid cell.

Walls: eight points round the outline are moved and tested against
`BuildingColliders::refuses`; a contact takes the speed into the wall (a little
bounce), scrapes the speed along it, and turns the car by the blow's moment. The
quay is a kerb: nobody drives into a canal.

## The camera stopped flying

`CameraMode::Free` stays a mode, because a `--shot`, a copied place and the
camera's Mixed picture all place the camera by pose. It is a **placed view**
now: looked around in, never moved (no WASD, E/Q or stick; the touch cluster
is gone), named "View" on the disc. Tab and the disc cycle Follow ⇄ Earth
(`next_camera_mode`); offline, with no Follow, the app is in Earth. A saved
"free" start comes back in Earth, and signs in to Follow.

## Not done

- No damage, no sound for the engine or the tyres, no headlights, no speedometer.
- Grip is tarmac everywhere (`grip` = 1); grass and paths are not told apart.
- The ambient traffic and the walkers do not see the player's car as a car.
- On a phone the stick is accelerator, brake and wheel; there is no handbrake.
- Terrain tilts the drawn car; the sim's car is flat, as its walk is.
