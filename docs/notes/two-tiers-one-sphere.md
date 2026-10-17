---
triggers:
  - working on globe.rs, map_stream.rs, or anything that decides what the horizon looks like
  - "why is there a globe visible at 10 km" / "why does the flat map curve"
  - changing GLOBE_SHOW_ALT, GLOBE_DRAW_ALT, the camera far plane, or a sky shell radius
  - adding a layer that has to reach further than a few kilometres from the anchor
  - the near and far planes in `free_camera.rs` (`camera_controls`)
updated: 2026-10-17
---

# Two tiers, one sphere: what the world looks like between 2 km and 50 km

The client has a flat tier (the streamed clipmap around the map anchor, in a
tangent frame measured in metres) and a globe tier (a quadtree of ellipsoid tile
patches). Until 2026-08-24 they were **alternatives**, switched at
`GLOBE_SHOW_ALT` = 50 km. They are now **layers**: the flat tier owns the near
ground at every altitude, the globe fills in past it from `GLOBE_DRAW_ALT` = 2 km,
and the flat tier's tiles are bent onto the globe's own sphere so the two meet
without a step.

## What was wrong, and the measurement that says so

Three constants had to agree and did not:

| | reach |
|---|---|
| flat clipmap (7×7 z10 ring at lat 53) | ±82 km |
| ground skirt (`GROUND_DISC_RADIUS`) | 150 km |
| true horizon from 12 km altitude | **391 km** |
| camera far plane below the space tier | 200 km |

So between roughly 5 km and 50 km the view was: map out to 82 km, then blank
paper to 150 km, then nothing. `GLOBE_SHOW_ALT`'s own doc comment claimed "below
it the detailed flat-tile city view fully covers the sky-facing hemisphere",
which is true at 2 km and false by 10.

Two things made it worse than the geometry alone. The skirt renders *brown*, not
paper — see the aerial-perspective section below. And the horizon-haze shell that
exists to cover exactly that band **turns itself off above 5 km**:
`fog_altitude_factor` smoothsteps to zero by `FOG_FADE_HIGH` = 5 km, so `peak`
is 0 and `sky.rs` hides the shell. The cover disappears precisely where the
defect appears.

Reproduced headlessly at Groningen, 12 km, looking north (`--shot`, 2026-08-24):
hard straight map edge partway up the frame, a brown band above it, flat rim,
sky. Identical to the phone screenshots that reported it.

## Why the flat tier is curved rather than the globe being brought closer

`setup_globe` builds the sphere in the anchor's tangent frame with the anchor
exactly on its surface at the local origin. So the flat tier's `y = 0` plane *is*
that sphere's tangent plane, and `y = -GlobeState::drop_at(d)` is not an
approximation of the globe — it is the globe, evaluated in the frame the basemap
is already placed in. That is the whole reason a handoff is possible at all: had
the two been built in unrelated frames there would be no way to make them meet.

Every ground tile therefore takes the drop at its own centre in its transform and
carries the remainder across itself in vertices. Consequences worth knowing:

- **The whole pyramid drops together**, so the clipmap's 0.4 m level spacing is
  preserved exactly and there is no "curve only past N km" threshold — a boundary
  between curved and uncurved levels would be the step this removes.
- **Subdivision is a function of tile size alone**, not distance. Within a tile
  the drop's growth is a *tilt*, which the corners represent exactly; only the
  tile's own sag needs vertices. A z10 ring 80 km out costs the same 7×7 grid as
  one overhead, and levels under ~3.5 km need no subdivision at all.
- **Neighbours cannot crack**, because both evaluate the same global `drop_at` on
  their shared edge.

Numbers: 0.08 m of drop at 1 km, 7.8 m at 10 km, 528 m at the clipmap's 82 km
edge, 1,764 m at the skirt's 150 km rim.

## What this costs, and the errors that are being accepted

- **A step at the seam.** The globe's quadtree lifts finer tiles outward
  (`QT_LIFT_PER_Z`, up to 48 m at z12) so its own levels do not z-fight. With
  both tiers drawn that would put the planet *over* the city, so the whole
  quadtree is sunk by `QT_SURFACE_SINK` = 68 m. The planet is uniformly 68 m
  smaller, which is invisible from orbit, and the clipmap's edge stands 68 m
  proud of the globe — 1.5 px of a 1080-tall frame at the altitude the seam is
  visible from.
- **A horizontal mismatch at the seam.** The flat frame uses the *anchor's*
  Mercator scale for every tile, so a tile 82 km north is off by ~0.9 % ≈ 700 m
  against the ellipsoid the globe uses. Nobody has reported seeing it; if it ever
  shows as a kinked coastline, the fix is to shrink the clipmap's coarse rings
  and let the globe take over closer in, not to re-project the flat tier.
- **The sphere, not the ellipsoid.** `drop_at` uses the geocentric radius at the
  anchor. The meridional and prime-vertical radii differ by ~0.4 % at lat 53, so
  the flat tier and the globe part by ~2 m at 82 km. Pinned by a test.
- **Draw calls.** At 12 km over Groningen the frame went from 213 draws to 594.
  The guard that keeps it from being far worse is in the quadtree's split rule:
  below the space tier, any globe tile lying wholly inside
  `map_stream::clipmap_reach_m` is kept but never split, because the clipmap is
  opaque and paints it at a zoom the globe cannot reach.

## Aerial perspective past 32 km needs the raymarched atmosphere

`render_sky.wgsl` composites a geometry pixel as
`scene x transmittance(t) + inscattering(t)`. In `LookupTexture` mode the
transmittance uses the true distance while the in-scattering is clamped at
`aerial_view_lut_max_distance` (32 km), so ground past that is extincted without
the airlight that should replace it — and since blue scatters out first, what
remains is red. That is what made the old ground skirt brown, and once the globe
carries the view to a 391 km horizon it is 300 km of real map with a magenta
cast.

`Raymarched` takes both terms from one integral over the true distance with no
clamp, so the switch moved from `GLOBE_SHOW_ALT` to `GLOBE_DRAW_ALT`. This is a
correctness fix, not a quality setting: the far ground now washes toward the sky
the way aerial perspective does.

Raising `aerial_view_lut_max_distance` instead was rejected. It stretches the
same 32 slices over a longer range, so buying the horizon costs the near field
its accuracy — an earlier session recorded it as "does not work", which is the
same finding seen from the near end.

Two things this does *not* reach. A handset that fails `space_raymarch` keeps
`LookupTexture` and the magenta with it, which is what it renders today anyway;
and the other handset path (no `FLOAT32_FILTERABLE`, so `SkyMode::Dome`) has no
Bevy atmosphere at all and never had the artefact — which is how the cause was
isolated, since the same viewpoint captured on the dome path was already clean.

Below `GLOBE_DRAW_ALT` nothing changed, and the step at the threshold is masked
because `DistanceFog` is still at ~46 % strength at 3 km. That fog is the
dominant haze at low altitude and it was tuned against a *clamped* atmosphere;
if it is ever revisited, it is now compounding with a correct one.

## Everything that stands between the ground and the far plane had to move

The far plane, the sky-dome shell and the star/moon shells were all sized against
"the world ends at the 150 km skirt". They are now sized against
`free_camera::horizon_distance_m`, and the ordering they must keep is:

    horizon  <  sky shells  <  far plane

Getting the middle one wrong is invisible on a desktop, because the sky-dome
shell is the *phone's* sky (`SkyMode::Dome`, an adapter with no
`FLOAT32_FILTERABLE`). A shell left at 160 km while the ground runs to 391 km
paints over the far half of the view. `sky_dome`'s test asserts the ordering at
four altitudes; `CARTO_SKY_DOME=1` forces the path anywhere.

The far plane's margin is 1.5×, not a hair over 1.0, because the Earth's
*silhouette* is drawn by the far edges of the tiles that straddle the horizon —
z9 tiles ~70 km across at 12 km altitude. At 1.2× the skyline came out as a
sawtooth: the tile grid, clipped one tile at a time.

Both overrides (this band's, and the space tier's far-limb plane) live in
`free_camera::camera_controls` **after** the camera-mode match, so they hold in
every mode; the system resets near/far to the ground defaults (0.1 m, 200 km) at
the top of every frame. They used to sit in the Orbit arm alone, so climbing in
Fly or Follow left the far plane at 200 km and clipped the whole planet to a
black screen.

## Still open

- The near field — buildings, roads, trees, grass, water, street labels, the
  avatar — is still flat. Everything it covers is within ~3 km of the anchor,
  where the drop is under a metre, so this reads as correct; it is an
  approximation all the same, and a layer that ever reaches further out will need
  the same treatment the tiles got.
- None of this has been seen on a phone. The container renders it on lavapipe,
  which is trustworthy for geometry, placement and visibility and not for colour
  or lighting (`headless-shots-software-renderer.md`).
