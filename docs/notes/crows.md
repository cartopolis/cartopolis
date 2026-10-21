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
same reason: a fixed pool of 78 birds, six shared meshes and one material made
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
- **One flock stays high** (added 2026-10-21, asked for as "a flock of birds
  flying high in the sky"): 44 birds, 60–120 m up, waypoints up to 300 m out
  and at least 160 m apart, 5 m spacing and four times the low groups' height
  spread so it reads as a loose spray of dots. It never lands, is recycled
  only past 650 m, and casts no shadows. The first version (36 birds,
  80–170 m, 450 m out) put three specks of 3–4 pixels in a street-level view
  of the sky on a real GPU: there, but not a flock.
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
through), no night roost.

## Calls (2026-10-21)

Asked for as "a subtle crow sound mixed in once in a while, when they are
near". Every 7–20 s, if any bird is within 55 m of the camera (straight-line,
so the high flock is never heard), one series of 1–3 caws plays at
`caw_level(distance)` — full within 6 m, gone by 55 m, on a square — times
0.35 and the effects volume. A flushed group within earshot gives 2–4 quicker
caws at 1.3×. Plain volume, no panning: the client has no spatial listener, and
adding one for one sound was not worth the unknowns on Android and the web.

Twelve caws were cut from two CC0 carrion-crow recordings on Freesound (the
UK alarm and a German single bird; `assets/sounds/LICENSE.md`) with ffmpeg on
the VPS (installed 2026-10-21 for this; the container has none). None was
heard: each was checked as a level against its background and as a
spectrogram, which is how a twelfth take showed up as another species. Carrion
crow only — American crow recordings, the most common on Freesound, sound
different and were not used.

## Checking it

`ShotMetrics::crows_air` / `crows_ground` count placed birds, on the traffic's
"placed, not in frame" terms. Over a square, `crows_ground == 0` once the cells
have landed means no ground was found — check the paving cells before the
behaviour. `CARTO_CROWS_HERE=1` puts one group 7 m in front of the camera and
circles the big flock just past it, so a capture can be aimed at birds.
