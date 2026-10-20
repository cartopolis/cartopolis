---
topic: crows
triggers: [crows.rs, "systems::crows", "crows", "birds", "flock", birds_enabled, birds_visible, CrowModel, wingbeat, WingPose]
updated: 2026-10-20
---

# Crows over the camera and on its open ground

`systems::crows` (2026-10-20), the model in `cartopolis_geo::agent_models::crow`.
Asked for as "flocks of low-poly crows flying around and crows on the ground
pecking — natural and lively". Under **Layers ▸ Life ▸ Birds**, on the detail
tier's default like Traffic and People.

It is built on the ambient traffic's terms (`ambient-traffic.md`) and for the
same reason: a fixed pool of 34 birds, six shared meshes and one material made
at startup, and after that only transforms are written. A group that drifts
past 330 m from the camera is placed again near it, never despawned.

## The birds are invented; where they come down is not

Crows land only on open ground the paving survey (BGT) names — squares
(`Paved`), footways, car parks, living streets, yards — or on its grass,
planting, verges and gardens **where the grass layer has not grown blades**, and
only where the last 30 m of the glide in clears the walls
(`BuildingColliders::segment_fraction`). Outside BGT coverage, or before the
cells arrive, **no group can land and every group flies** — the intended
fallback, not a fault. A ground group gives the map 12 s to find ground before
it takes to the air instead.

**Not on the grass layer's lawns, and that was measured.** Its blades are wild
grass, 0.38 m jittered to 23–53 cm, and a feeding crow is about 25 cm tall. The
first captures (2026-10-20, Noorderplantsoen, `CARTO_CROWS_HERE`, 4 m up) put a
group of seven 7 m in front of the camera on a drawn lawn and showed one dark
speck. Flattening the blades round a feeding group would need crow positions in
the grass material, which is shared by every patch; not done. City crows forage
on squares and paths as much as on grass, so the open ground is no compromise
in a street — but in a park the birds are on the paths, not the lawns.

## What makes them read as birds

Each of these was a deliberate choice, and each is the first thing to look at
if the layer starts to look mechanical:

- **Groups decide, members steer.** Group sizes 11, 7, 5, 4, 3, 2, 2: crows are
  mostly pairs and handfuls. A group roams between waypoints within 170 m of the
  camera, lands together, feeds 25–90 s, and leaves together.
- **Loose flocking with a turn-rate limit** (1.3 rad/s), so flocks arc round a
  waypoint and bank into the turn. Spacing ~4 m: crows are not starlings.
- **Flap–glide bouts per bird**, glide on descent, beat on the climb. The wing
  is two segments, the hand lagging the arm and shortening on the upstroke
  (`wingbeat`) — a single panel flapped like a hinged board.
- **On the ground**: pecks (body tipped forward, tail up, beak to the grass),
  strutting walk with the head bobbing, hop with a wing flick, look round, turn.
  Each bird picks its own, so a group is never in step.
- **They spook, in a cascade.** The avatar (6 m), an ambient walker (3.5 m) or
  the camera itself (2.5 m) flushes the nearest bird at once and the rest of its
  group over the next 0.1–0.6 s, away from the threat. A landing site is never
  chosen beside a threat.

## Not done

No perching on roofs or lamp posts, no avoidance of tall buildings (flocks cruise
18–45 m over the ground, above most Dutch roofs; the Martinitoren is flown
through), no calls, no night roost.

## Checking it

`ShotMetrics::crows_air` / `crows_ground` count placed birds, on the traffic's
"placed, not in frame" terms. Over a square, `crows_ground == 0` once the cells
have landed means no ground was found — check the paving cells before the
behaviour. `CARTO_CROWS_HERE=1` puts one group 7 m in front of the camera and
circles the big flock just past it, so a capture can be aimed at birds.
