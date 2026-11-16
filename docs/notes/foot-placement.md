---
topic: foot-placement
triggers: [systems/player/feet.rs, drawn_ground_at, crates/human/src/feet.rs, crates/human/src/reach.rs, systems::feet, place_feet, HumanLegs, FootPlacement, pelvis_drop, plant_leg, reach_from_pose, CARTO_NO_FOOT_IK, CARTO_FOOT_LOG, foot IK, "feet float", "feet sink"]
updated: 2026-11-16
---

# The feet stand on the ground they are over

Asked for by the owner as "voeten op de grond" (2026-11-16). Every clip is
captured on a flat floor and the avatar stands on the one ground height under
its origin (`avatar::ground_or_water` → `TerrainField::ground_at`), so on a
slope the foot ahead is in the ground and the one behind floats, by the
foot's distance from the origin times the gradient.

## Where it shows (measured 2026-11-16)

Two measurements: AHN's own half-metre DTM over central Groningen
(6 km², four 1.5 km WCS tiles, buildings and water left out as no-data), and
the client itself on mac-1, `CARTO_FOOT_LOG=1` logging, at the frame each
capture was taken, each foot's ankle over the ground under it less what the
clip has on its own floor (zero is the clip's contact at every phase), and
where the body stands over the drawn ground. Planted feet only (stance ≥ 0.8).

- **Open ground by gradient over 1 m** (AHN smoothed over 1.5 m, which leaves
  2 cm of residual noise): 74 % under 5 % (a planted foot ~0.3 m from the
  origin off by < 1.5 cm), 13 % 5–8 %, 7 % 8–15 % (2.5–4.5 cm), 5.9 % over
  15 %. Unsmoothed, AHN holds kerbs as 10–18 cm ramps about a metre wide.

| place | against `ground_at` (half-metre tier) | against the drawn ground |
|---|---|---|
| Noorderplantsoen rampart, 53.22532,6.55544 → 53.22538,6.55529, walking up | max 26.5, mean 6.7 cm | max 14.4, mean 4.1 cm |
| the same, idle across the slope at 53.225348,6.555364 | 2.9 / 4.9 cm | 1.1–1.4 cm |
| street, kerb and bank by the Oosterhaven, 53.21205 → 53.21228 at 6.5700 | mean 2.0, ±3–6 at the kerbs | max 1.6, mean 0.3 cm |
| Berlagebrug's NW approach, 53.22091,6.58969 → 53.22048,6.59009 | mean 2.0, max 6 cm | max 4.4, mean 1.2 cm |

So it shows on the slopes of parks, dykes and banks, and nowhere a street
is drawn: the drawn ground holds neither kerbs nor AHN's noise, and a bridge
ramp is graded to at most 6 % (`MAX_APPROACH_GRADE`), under 2 cm a foot.

**The bigger float is the whole body, and this does not touch it.** The body
stands on `ground_at`, which adds the half-metre tier to the field the
sheets are meshed from (`terrain-relief.md`); over ground atlas graded, that
puts back what the grade took out. Logged as `body_over_drawn_cm`: on the
rampart **−38 to +170 cm** (`ground_at` rose 4.2 m over the 12 m where the
drawn path rises 1.9; a `CARTO_PROBE` run read the same off the sheets
themselves), −51 to +20 cm on the Oosterhaven street and bank, −10 to +40 cm
on the Berlagebrug's approach. The captures did not show a body plainly that
far up, and the drawn-ground stand-in (`land_at` plus the street and landing
corrections) is not what a road is drawn on, so this is **measured in
numbers, not yet confirmed in a picture**. Fixing it is a change to
`ground_at` (every point consumer: collision, cars, pins), not to the feet.

## What it does

`systems::feet::place_feet` runs in `PostUpdate` after the animation and
before propagation, like the held phone. Per body near the camera (the local
avatar, and remote ones within `RANGE_M`, 40 m):

1. Each ankle as the clip has it, the **drawn** ground under it
   (`TerrainField::drawn_ground_at`: `ground_at` without the half-metre tier)
   less the drawn ground under the origin: the **rise** each foot needs
   (±`MAX_STEP_M`). A difference, so the body's own offset stays as it is.
   Against `ground_at` the first cut stood feet on kerbs and slopes nobody
   sees.
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
floor, a wall top, a roof, which the ground field does not hold. Eased over
`BLEND_S` (0.2 s). Below 1 mm nothing is solved, so flat ground is the clip
exactly.

After: every planted foot within 2.0 cm of its drawn ground in all four
places (0.0–0.1 cm except the rampart's steepest frames, where a lifting heel
is partly planted).

## Costs

Eight ground lookups per body per frame (`ground_at` at the origin for the
on/off rule; `drawn_ground_at` at the origin, each ankle and two slope
samples per foot), each a bilinear sample plus the per-cell street and
landing corrections; a joint walk to the holder per bone, no allocation; the
joint names are looked up once when the body is built
(`human_avatar::spawn_human` puts `HumanLegs` on the holder). Nothing is on
the wire: every viewer places everybody's feet on its own map.

## What is not done, and what lost

- **The body's own offset** from the drawn ground (above) is left as it was.
- **The drawn kerbstone** (`LineKind::Kerb`, a 10 cm box) is not stood on:
  it collides with nothing and is not in `ground_at`.
- **Stairs** inside buildings are floors (`interiors::Footing`), flat, and
  the placement is off there (the 5 cm rule).
- **No toe/heel split**: the foot is one bone tilted as a whole; the ball
  joint is left to the clip.
- **Sampling the slope as one plane under the body** (a fit through the
  origin's neighbourhood instead of the ground under each foot) would have
  ignored AHN noise and kerbs, and also every step; per foot won.
