---
triggers:
  - sky.rs `eye_adaptation_stops`, `metered_light`, `METER`, `ADAPTATION`, `night_vision_grading`
  - changing the camera's `Exposure`, `ColorGrading` or the tone curve (`quality::TONEMAPPING`)
  - changing the sun's illuminance, the ambient or the atmosphere's lighting
  - "the evening / the night looks too dark / too bright / unnatural"
  - considering auto exposure (`AutoExposure`) or local tone mapping
updated: 2026-11-07
---

# Exposure: a light meter with partial adaptation

The camera's exposure follows how much light the city actually has, not the
clock. `sky::metered_light` reads a table (`METER`) of the city's scene luminance
by sun elevation, and the camera makes up `ADAPTATION` (0.78) of every stop that
light loses: an evening is darker than noon by what the eye does not make up.
On top, `night_vision_grading` takes colour out of the shadows and midtones as
the light falls to the night floor; the lit windows, highlights, keep theirs.

## The table is a measurement, and goes stale

`METER` is the renderer measured, not a model of the sky. Change the sun's
illuminance, the ambient, the atmosphere or the windows and re-measure it,
or the exposure keeps answering for the old light.

How it was taken (2026-11-07, mac-2, twice: again after the atmosphere's sun
stopped being dimmed twice, which brought the city up 0.6–0.75 stop from 9° to
−8° and the sky after sunset a stop): the Grote Markt from 70 m (the
`grote-markt` pose of `docs/shots.toml`), clear, no fog, 46 times over
17 October and 21 June, rendered with `Tonemapping::None`. With no curve the
picture is the scene times the exposure, clamped, so per frame: the city crop
(`960x300+0+240` of a 960×540 frame) to linear (`convert -colorspace RGB`),
its mean, times `1.2 · 2^ev100` from the CSV's `exposure_ev100`, log2. The city
was nowhere near clipping (under 3 % of pixels at the brightest noon). The two
dates agree within half a stop at one elevation; the views do not quite (a
morning and an afternoon at 9° differ by 0.4 stop, the light falling on
different facades), which is the limit of a one-view meter.

**Measure dusk one launch per capture.** In a run of captures the sky's light
on the city (the atmosphere's environment map, on the rungs that have one) lags
the clock: at 18.5 h after a 17.6 h capture the city read 0.179, launched on
its own 0.050, and waiting longer (`settle` 6 s) changed nothing. Read in
sequence the dusk points came out up to 0.8 stop bright, so `METER` takes them
from single launches. By day the lag is small, and a morning and an afternoon
at one elevation lag in opposite directions, so the table takes their mean.
Whether the app shows the same lag when the time is jumped (the time slider,
`set_sky`) is not measured.

What it found: from an October noon to the night floor the city loses 7.6
stops. Twilight in the renderer is far gentler than outdoors, about 0.4 stop
per degree below the horizon against about 1.2 in measured skylight, because
the atmosphere keeps the sun on to −18° (`sun_light_in_atmosphere`). That is
why an incident-light model built from the light terms in `sky.rs` (the sun's
illuminance on a horizontal plane plus the ambient) could not stand in for the
table: on paper it came out about two stops darker than the render at −2°,
the atmosphere's share of the light being the part it has no term for.

`ADAPTATION` was fitted against the exposure that matched the look before the
tone curve changed (`daylight-shots.toml` against `TonyMcMapface`): 0.78 is
within half a stop of it at most elevations, and up to a stop darker with the
sun 2–5° up, where the old lifts had piled up. It leaves the night 1.7 stops
under noon.

## What it replaced

Four lifts summed by sun position: `NIGHT_EXPOSURE_LIFT` on `1 − daylight`,
`TWILIGHT_EXPOSURE_LIFT` on `horizon_proximity`, and two the switch to Khronos
PBR Neutral needed (a toe lift after sunset and a day trim). Each was tuned by
eye against a frame, so every change to the light or the curve broke the set:
the curve swap of 2026-11-06 left the city a quarter stop dark by day and half
as bright at night, and the first fix, tuned on whole frames, darkened the
city into a low sun because the frame's mean was mostly sky. **Measure the city
apart from the sky** (`docs/daylight-shots.toml` has the crops).

## Alternatives, and why they are not built

**Auto exposure (bevy's `AutoExposure`, a luminance histogram per frame).**
Measures the picture itself, so it would follow every light without a table,
and adapts over time like an eye walking from a lit square into a dark street.
Not built because:
- it is a compute pass on the HDR texture every frame. The phone rung has no
  `Hdr` (`quality::Budget`) and the phone is already at 26 fps in a still
  street (OnePlus 15, 2026-11-07), so it would be a desktop-only look;
- a capture's exposure would depend on what was on screen in the frames before
  it, which breaks the comparison shots (`--shot`, `daylight-shots.toml`) this
  table is checked with;
- it meters the frame, sky included, which is the mistake above: a view with
  more sky would darken the city. It needs a metering mask to be right.

Worth revisiting on the desktop rungs if the table proves hard to keep: with a
mask that weights the lower half and `ADAPTATION` as its compensation curve.

**Local tone mapping (exposure fusion, a bilateral or pyramid pass).** Keeps the
shade and the sunlit facade readable at once, the way a photograph's HDR merge
does. Several full-screen passes per frame; not affordable on the phone rung, and
on the desktop Khronos plus the meter already gives the photograph's contrast
(luma standard deviation 0.21 against the photograph's 0.19).

**A different tone curve.** Measured on 2026-11-06 (`cloud-lighting.md`): AgX
was greyer than Tony, which was flatter than the photograph. Khronos stays;
`TONE_CURVE_LIFT` pays for its toe.

**A blue shift for night vision.** Not tried: the night is already blue (the
ambient and the atmosphere are), so the grading only takes colour out.
