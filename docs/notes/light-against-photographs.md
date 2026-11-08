---
topic: light-against-photographs
triggers: [light_compare.rs, shot_presets.rs, sky.rs, free_camera.rs, systems::light_compare,
  "cartopolis compare", "reference photograph", "shot.region", "looks unrealistic",
  "the light is not realistic", "facades look flat"]
updated: 2026-11-08
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

