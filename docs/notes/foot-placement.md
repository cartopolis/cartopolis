---
topic: foot-placement
triggers: [systems/player/feet.rs, systems/map/drawn_ground.rs, systems::drawn_ground, crates/human/src/feet.rs, crates/human/src/reach.rs, systems::feet, place_feet, HumanLegs, FootPlacement, pelvis_drop, plant_leg, reach_from_pose, DrawnGround, ground_at, CARTO_NO_FOOT_IK, CARTO_FOOT_LOG, foot IK, "feet float", "feet sink", "avatar floats"]
updated: 2026-11-16
---

# The body and its feet stand on the ground as drawn

Asked for by the owner as "voeten op de grond" (2026-11-16). Two things were
wrong, and the bigger one was not the feet:

1. **The body stood on ground nobody sees.** `TerrainField::ground_at` — what
   the avatar, the collision, cars, crows, pins and prims stand on — added
   AHN's half-metre tier to the field the ground sheets are meshed from. Nothing
   is drawn on that tier (it only shades the ground, `ground_detail`), and over
   a path atlas graded it put back what the grade took out.
2. **The clips are flat-floor captures** and the body stands on the one ground
   height under its origin, so on a slope the foot ahead is in the ground and
   the one behind floats, by the foot's distance from the origin times the
   gradient.

## How far off it was (measured 2026-11-16, mac-1)

`CARTO_PROBE` points every 0.5 m along three walks, `ground_at` (`walk`) set
against the height the ground sheet's own triangles have there (`ground probe:
sheet`, the mesh itself). In a single-place run on the
**Noorderplantsoen rampart** path (53.22532,6.55544 → 53.22538,6.55529, a
34 % slope, 4 m of rise in AHN), `ground_at` rose 4.2 m where the drawn path
rises 1.9: the body stood **up to 1.7 m over the path** at its top and 0.7 m
under it at its foot. Seen side by side in the same pose
(`/tmp/s690-feet/final2/rampart_*`): the avatar stood on the grass of the
rampart beside a path it was supposed to be on. By the Oosterhaven (a street,
its kerb and the grass bank to the water, 53.21205 → 53.21228 at 6.5700) it
was ±15–45 cm off; on the Berlagebrug's NW approach up to 12 cm.

AHN itself, over 6 km² of central Groningen (half-metre DTM, buildings and
water left out): 74 % of open ground slopes under 5 % over a metre, 5.9 % over
15 %. Unsmoothed it holds kerbs as 10–18 cm ramps a metre wide — the kerbs a
body on the half-metre tier stepped up and down that are not drawn.

## The ground is the drawn sheet (`systems::drawn_ground`)

No formula over the field reproduces the sheets: between vertices up to
12.5 m apart a sheet is a chord, refined where the field creases; streets
stand on their solved levels, landings and lower quays on their own. On the
rampart the chord stood 21 cm off the field it was cut from, and the field
function the walker had (`land_at` and the street and landing corrections)
was 39 cm off the sheet at one point. So each cell's worker, once it has
stood the sheets, samples their triangles onto a **one-metre raster**
(`DrawnGround::rasterize`, the higher sheet where two overlap; the ground
groups only, not water, decks, tunnels or anything with volume), registered
per cell like the walkable decks and landings and dropped with the cell.
`ground_at` reads it first, the field function only where no sheet is built,
and a flat deck over either.

Measured with the same probes after (`final2/numbers.txt`), `ground_at` less
the sheet:

| walk | before: max / mean / within 2 cm | after |
|---|---|---|
| rampart path | 23 / 7.6 cm / 32 % (170 cm in the single-place run) | 13.7 / 2.0 cm / 68 % |
| Oosterhaven street and bank | 46 / 7.3 cm / 25 % | 11.0 / 0.8 cm / 88 % |
| Berlagebrug approach | 12 / 3.1 cm / 40 % | 5.9 / 1.1 cm / 81 % |

What is left is the raster cutting corners: within a metre of a crease it is
off by the change of slope over a quarter metre (8.5 cm at a 34 % rampart's
foot), and at a step (a terrace's edge, the bank's wall) it spans the step.
A half-metre raster would halve that at four times the memory.

**Cost:** 4.2 MB a cell (every 64 m tile of a city cell has a sheet in it;
i16 centimetres), so about 38 MB for the nine cells at street level, and
280–470 ms per cell on the cell's worker, against 2–24 s of refinement.
Storing it as an i8 residual over the field, or only for cells near the
avatar, would halve or better that; not done.

**Who stands on it:** everything that called `ground_at` — the avatar and
remote avatars, their feet, the collision and the climb, the follow camera's
ground, traffic and transit vehicles, crows, pins, prims, splashes, sewers
(depth under the drawn street), GPS placement and the MCP readouts. **Not**
what is built on a worker from `LidSnapshot::ground_at`: the grass mask,
trees, street furniture. Those stand on the sheets' own field function (the
values at the sheets' vertices), so they differ from the sheet only by the
chord; they are built before the raster of their cell exists and would have
to be rebuilt when it arrives.

## The feet (`systems::feet`)

`place_feet` runs in `PostUpdate` after the animation and before
propagation, like the held phone. Per body near the camera (the local
avatar, and remote ones within `RANGE_M`, 40 m):

1. Each ankle as the clip has it, `ground_at` under it less `ground_at`
   under the origin: the **rise** each foot needs (±`MAX_STEP_M`).
2. **The pelvis comes down only for planted feet** (`feet::pelvis_drop`): a
   foot's *stance* fades from planted to lifted as its ankle rises
   4.5–10 cm over rest, and a planted leg keeps the bend the clip gave it.
   Taking the lower foot's whole difference — the first cut — dropped the body
   20 cm at every stride up the rampart, because the swinging foot behind was
   over lower ground: a lunge per step. A foot in the air asks for nothing and
   stops short of lower ground. Eased at `DROP_RATE`, bounded at `MAX_DROP_M`.
3. **Two bones per leg** (`feet::plant_leg` over `reach::reach_from_pose`):
   the thigh and the calf turned so the ankle lands on its rise; the knee keeps
   the side the clip bent it to and is sent forward where the clip holds the
   leg straight or bent back. `reach_from_pose` lets a leg be as straight as
   the clip already has it — the arm's 0.995 limit pulled a standing leg in
   by 4 mm when solving it for where it already was.
4. A planted foot is **tilted to the ground's slope** (one-sided samples
   0.3 m along x and z), at most `MAX_TILT` (25°).

On: standing, walking or running on the ground. Off (`feet::Footing`):
swimming, seated, climbing, an emote (and any pinned clip but idle, walk and
run), in the air, and **more than 5 cm off `ground_at` at the origin** — a
floor, a wall top, a roof. Eased over `BLEND_S` (0.2 s). Below 1 mm nothing
is solved, so flat ground is the clip exactly.

Planted feet against the drawn ground (`CARTO_FOOT_LOG=1`, the stance foot
at each captured frame): walking up the rampart up to 14 cm in or over it
(mean 4), 1–1.5 cm standing across it, up to 4 cm on the Berlagebrug's
approach, under 2 cm on the Oosterhaven street; after, within 2 cm
everywhere. Eight ground lookups per body per frame, no allocation; the
joint names are looked up once when the body is built (`HumanLegs`). Nothing
is on the wire: every viewer places everybody's feet on its own map.

`CARTO_NO_FOOT_IK=1` leaves the clips' feet alone for a "before" capture.

## What is not done, and what lost

- **The drawn kerbstone** (`LineKind::Kerb`, a 10 cm box) is not stood on:
  it is not a ground sheet.
- **Stairs** inside buildings are floors (`interiors::Footing`), flat, and
  the placement is off there (the 5 cm rule).
- **No toe/heel split**: the foot is one bone tilted as a whole.
- **Keeping the half-metre tier for the feet only**, the body on the drawn
  ground: feet on kerbs and slopes nobody sees. Tried first; the first cut
  stood every foot on it.
- **Making the field function match the sheets** (the tunnels' lids, the
  roads' levels, the quays' terraces added to `ground_at`): still a chord
  away, 21 cm on the rampart.
- **Keeping each cell's triangles** to query exactly: ~3 million vertices a
  street-level cell, 36 MB or more each.
