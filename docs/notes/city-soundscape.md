---
topic: city-soundscape
triggers: [ambience, city sound, background sound, footsteps, footstep surface, recorded sound, freesound, assets/sounds, systems::ambience, ambience.rs, soundscape.rs, PavingIndex, paving_at, drivable_near, step echo, --clip walk, walk video]
updated: 2026-10-20
---

# The city's sound: recordings, chosen by the map

Started 2026-10-20 after the user asked for a GTA / Red Dead feel on foot. The
earlier soundscape was all synthesis, and every complaint about it was a
synthesis artefact: walking "sounded like a wind tunnel" (a clipped low-pass
rush), the steps were "cheap" and then a metronome, and the road hum "did not
sound like traffic" and was removed for its per-frame cost
(`avatar-game-feel.md`, `ambient-traffic.md`). **The user chose recordings
from free libraries** over recording Groningen themselves.

## What it is

- **Footsteps by surface**, from the BGT's own paving material where the survey
  says (brick, concrete pavers, tiles, asphalt, gravel, grass…), the lawns where
  it does not, and asphalt off the survey (an OSM carriageway). Indoors is a
  wooden floor; hard ground in the rain uses wet takes. Six takes a surface,
  never the same twice running, on top of the existing rate and level jitter.
- **Nine background loops** — traffic, shopping street, residential, park,
  canal, square, night, room, rain — mixed from what the map already holds:
  road speeds and distance, shops/cafés within 40 m, lawn and water on a 14 m
  ring of probes, walls on eight 30 m rays at head height, the interiors, the
  sun and the live rain. Sampled four times a second, eased over seconds.
- **One echo off the facades** for each step in a street narrower than ~20 m.

The loops are a texture over real data, like a stone pattern on a pavement;
they place nothing. A sound tied to one spot (a bell, a gull) is a placement,
and only for a source the data names (`world-content-from-real-data-only`).

## Constraints found on the way

- **No CC0 recording of footsteps on a brick street exists** on Freesound
  (searched in five languages, 2026-10-20); a stone patio stands in for
  `brick-*`. Recording Dutch klinkers would be the one gap worth filling.
- Freesound's originals need a login; the HQ previews (~192 kbps Ogg) do not
  and are what was cut. Every file and its author is in
  `assets/sounds/LICENSE.md`.
- **Bevy's mixer has no filters and no reverb.** Indoors therefore makes the
  street quieter rather than duller, and the street's echo is one delayed copy
  of the step, not a reverb. Real reverb or a low-pass means another audio
  library (Kira or Firewheel); not checked against Bevy 0.20, Android or wasm.
- Decoding Ogg is `bevy/vorbis` (lewton, pure Rust), which adds `lewton` and
  `ogg` to the lockfile and cross-compiles like the rest.
- The paving polygons were dropped after meshing; `PavingIndex` keeps them per
  cell inside `WorldPlaces` (16 m buckets). The road query rejects a line on its
  bounding box before reading its points — the removed road hum scanned every
  vertex every frame and was the biggest named main-thread cost on the web.

## How the files were cut, and how it is checked without a speaker

This container has no audio device, so nothing here was tuned by ear by the
agent. The steps were found by onset detection on the recordings (a take is
rejected if anything in its tail reaches half its peak or it rises late) and
checked as waveform pictures; the loops are the steadiest 33 s of each take,
loudness-normalised to −20 LUFS and crossfaded end-into-start over 3 s.

To hear a change, render a walk: `--clip walk --frames N` with two or more
`--at` walks the avatar along the path (frames spaced by distance, the clip's
phase from distance over the pack's stride), and
`RUST_LOG=cartopolis::ambience=debug` logs what the game chose at each frame,
from which a soundtrack is built and muxed under the frames. Measured on the
Deck, 2026-10-20: 407 frames at 1280×720 in 17 min (~2.5 s a frame) for a
34 s walk at 12 fps. Pick the path from OSM's way geometry, not address
points (those sit on facades), and check the turns with a still first — the
first try walked into a building, which the sampler duly reported as indoors.
