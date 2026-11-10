---
topic: light-against-photographs
triggers: [light_compare.rs, shot_presets.rs, sky.rs, free_camera.rs, ground_bounce.wesl, systems::light_compare,
  earth_air, ScatteringMedium, ground_albedo, light-shots.toml,
  "cartopolis compare", "reference photograph", "shot.region", "looks unrealistic",
  "the light is not realistic", "facades look flat"]
updated: 2026-11-10
---

# The light is judged against photographs, not against the render

On 2026-11-08 the person steering said the sun, the light on the buildings, the
sky and the horizon still did not look real, after two months of work on them.
What that work had in common: every number was tuned against another part of the
render. The fog colour against the sky, the deck's fill against the fog, the
exposure table (`METER`, `exposure-and-adaptation.md`) against the city as the
renderer drew it, the haze against the aerial perspective. Counted that day:
60 tuned `f32` constants in `sky.rs` and 74 in `free_camera.rs`, and four
separate sources of the colour of distant air (bevy's aerial perspective,
`DistanceFog` from `HORIZON_SKY`, the horizon-haze shell, the deck's fill).
`cloud-lighting.md`'s history is that pattern: a fix to one, then another went
brown or pink, then a fix to that. A render tuned against itself can be
self-consistent and still not look like Groningen.

So a change to the light is measured against a photograph of the same view:
`cartopolis compare` (`systems::light_compare`).

## What is compared, and why stops from an anchor

A shot in `docs/photos.toml` names a `reference` photograph — a camera picture,
whose pose the shot already is (`camera.md`) — and `[[shot.region]]`s: a sunlit
wall, a wall in shade, the sky overhead, the sky at the horizon, sunlit and
shaded ground. Per region it reports how many stops the region is from the
`anchor` region, in the photograph and in the capture, and how warm it is
(log2 of red over blue of the mean linear colour). And the picture's contrast,
as the standard deviation of gamma-encoded luma, the figure the exposure note
already used.

**Stops from an anchor, never absolute levels.** The phone picked its own
exposure and its own tone curve, and the render has the light meter; neither
says anything about the light. What does is how the light divides between sun,
shade and sky, and an exposure change moves every region by the same stops and
so leaves that alone. The phone's processing (HDR merge, local tone mapping)
does compress the split, which makes a photograph's sun-to-shade ratio a lower
bound on the scene's; the render goes through a tone curve too, so both are
pictures, and pictures are what is being compared.

A region in the photograph and the same place in the capture can need different
rects (`render = [...]`), because a phone's compass is a few degrees out
(`camera.md`: magnetic, no declination) and the render frames slightly
differently. Draw the rects on the side-by-side the command writes
(`<shot>.compare.jpg`), not on the photograph alone.

## How to read one

`stops_error` is the capture minus the photograph: a shaded wall at +1.5 means
the shade is a stop and a half too bright for the sun on the anchor, which is
what "flat" looks like. `warmth_error` the same for colour. `stops_rms` and
`warmth_rms` sum a shot up; a change to the light should lower them across
the set, not on the one shot it was tuned on.

Only a real GPU's capture is evidence here (`mac-1`, `mac-2`): lavapipe is evidence
for geometry and none for light (`headless-shots-software-renderer.md`).


## What the first look found (2026-11-09)

Before any photograph came in, on mac-2 at the Grote Markt in June, clear,
mean linear luma per patch:

- **On the phone, and on every run without the atmosphere's environment map,
  a facade in shade was an eighth of the desktop's** (0.012 against 0.088):
  the ambient that stands in for the map had been sized on paper.
  `AMBIENT_DAY_BRIGHTNESS_NO_IBL` is 12,000 now (`sky-without-an-atmosphere.md`).
- **Nothing bounced** — on the phone. Ground in shade was 10 times darker than
  ground in sun and bluer than red; outdoors it is about 5–6 times and grey.
  `ground_bounce.wesl` adds the sunlit street's light to the buildings (a shaded
  facade +0.4 stop, warmer), **only where there is no environment map**: the
  atmosphere's map already holds it, below its horizon (next section).

## What the second look found (2026-11-10)

On mac-2, `docs/light-shots.toml` (the Poelestraat and the Grote Markt at eye
level, 13:30 on 21 June, clear), with every building and ground surface
painted one grey of albedo 0.2 so that only the light differs:

- **The shaded ground was darker than a shaded wall because the wall saw
  bevy's planet below the horizon.** The sky-view table the environment map
  is baked from ends a downward ray on the planet's ground and lights it with
  the sun at `ground_albedo` 0.3 (`raymarch_atmosphere`, `ground = true`): a
  sunlit plane under every wall, which ground facing up does not see. With it
  set to 0 the shaded wall fell 1.9 stops, onto the shaded ground. So the
  wall's fill was right and `ground_bounce` was counting it a second time on
  the desktop; it is added only without the map now.
- **The shaded ground was dark and blue because bevy's air has no haze.**
  `ScatteringMedium::earth` holds an aerosol column of optical depth 0.005 and
  has its Mie scattering and absorption swapped (3.996e-6 is the absorption in
  bevy 0.20, the scattering in Hillaire 2020 and in bevy 0.16). Putting the two
  right changed nothing measurable; raising the column to a clear Dutch summer
  day's 0.1 (`globe::earth_air`) took ground in shade from 3.4 to 2.7 stops
  under ground in sun, and its log2(red/blue) from −3.4 to −1.6; the sky
  overhead from −2.1 to −1.3, the pale blue it is here. Shaded ground and
  shaded wall now agree (−2.74, −2.68). The phone, without the map, is within
  a third of a stop (−2.80, −2.42) and warmer (−0.3).
- What the haze costs: a sun 5° up is duller and less gold than it was
  (`daylight-shots.toml`'s `gm_16_6`), since the haze dims a low sun as a
  real one does; looking into a low sun (`e_8`) the city behind it goes white.
  A haze that takes more blue than red (Ångström 1.3) gave back a little gold
  and drew a lilac band along the horizon, so it is grey. 0.1 is a guess at a
  clear day, not a measurement: the photographs set it.
- Still open: the desktop's shade is bluer than the phone's by more than a
  stop of red over blue, and nothing yet says which is right — that is what the
  photographs are for. Ground in a street also takes light from the sunlit
  wall across it, which nothing adds (a quarter of a stop, by arithmetic).
