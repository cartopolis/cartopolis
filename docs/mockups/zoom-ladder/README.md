---
topic: zoom-ladder
triggers: [free_camera.rs, globe.rs, earth_mode, globe_spin, ORBIT_MAX_DISTANCE_SPACE, map_osd.rs, Teleport, ladder.rs, Rung, "solar system", "Milky Way", galaxy, "night side", night_side, SunMode, "fly to", flight, rail]
updated: 2026-11-07
---

# The zoom ladder: one zoom from the street to the Milky Way

## v4 — the switch folded into one disc, 2026-11-07

![v4](v4.png)

After review the same day: the scale switch is one disc on the desktop too
(asked when it shows: folded, like the phone's), crossing at the edge by a tap
or half a second of zooming on, and the rail a little shorter. Built on
`session/s643`; standing facts in `docs/notes/zoom-ladder.md`, "Four scales".

## v3 — four scales, proposed 2026-11-07

![v3](v3.png)

Asked 2026-11-07: the one rail from the street to the universe is impractical; the
range should follow the scale you are in (globe, solar system, Milky Way, universe),
and changing scale should stay logical without being part of the one slider.

The proposal: a scale switch of four buttons above the rail (one disc that opens on
a phone); the rail spans only the current scale (Globe 8 rungs, the others 4 each,
same heights as today; the Milky Way's four rungs are new) and stops at its ends.
At an end the − (or +) becomes the next scale's button: a tap, or half a second of
zooming on, crosses, and the view goes on from where it stopped. A tap on a scale
flies there; Globe returns to the place and height you left. Drawn on captures of
today's client (mac-2) at 8,000 km, 2·10¹² m, 6·10²⁰ m and 5·10²⁴ m.

## Built, 2026-11-01

v2 built the same day, after the person steering said to build after the
concept, in five commits on `session/s540`: grab, throw and double-tap the
globe; the light as Auto / Light / Dark; the zoom rail; Earth & Moon, the
Solar system, the Stars and the Milky Way; flights. Standing facts are in
`docs/notes/zoom-ladder.md`. Built differently from the drawing: the rung's
name shows on the rail's tag, not as a toast at the top; a planet or a star
has its distance and light time on its label instead of a card; no
two-finger tap to zoom out; Light draws no city lights beyond its line; Dark
shows city lights everywhere and the land almost black. Not built: anything
past the labels for the other planets (no visiting Mars).

## v2 — after review, 2026-11-01

![v2](v2.png)

The person steering: the light switch should sit with the other light
settings, as a dark / light / auto toggle like a theme switcher; the zoom rail
was massive. So the light is today's `SunMode` (Automatic / Day / Night /
Manual) renamed Auto / Light / Dark / Set a time, shown as a disc in the
top-left column that opens the three beside it, and in Settings ▸ World.
What each means from above is new: Light lights the globe from over your
shoulder (today's Day still leaves its far side dark), Dark is the Earth at
night all round. The rail is a 4 px track between a small + and −, 30 px wide
(28 on a phone, in place of the + / − pair), naming only the current rung;
hovering or dragging shows the other names as plain text. The rest of v1
stands. − sits at the top of the rail, next to the Milky Way, and + at the
foot next to the street (asked the same day): up stays higher, and each button
sits at the end it moves towards.

## v1 — proposed 2026-11-01

![v1](v1.png)

Asked for 2026-11-01: where are the solar system and Milky Way layers; moving
from one layer to the next should be seamless; turning the globe is laborious;
the real sun and night side are beautiful but impractical for navigating.

What the client did then (`free_camera.rs`, `globe.rs`): the camera stops at
45,000 km from the Earth's centre (~38,600 km up), above which there is only
the sky dome; the globe spins at a rate set from the height and stops dead on
release; the right button does nothing; a jump (search, Back, locate) is an
instant `Teleport`; the globe's light is the real sun or Settings' fixed noon
at the anchor, which still leaves the far side dark.

The proposal:

- **A rail** on the right edge naming the rung you are on (the scenario
  ladder's rungs plus Earth & Moon, Solar system, Stars, Milky Way); tap a rung
  to fly, drag to zoom. On a phone it folds into the + / − pair with the rung's
  name between, and opens on a tap.
- **The globe in the hand**: the grabbed point stays under the pointer; a throw
  keeps turning and slows in about a second; double-tap flies ×4 closer,
  two-finger tap ×4 out; right-drag turns and tilts; arrow keys, + / −, N.
- **Even light from above** by default: the sun over the viewer's shoulder, the
  real sunset line drawn dashed, city lights beyond it; a two-way switch (Even
  light / Real sun) bottom-left whenever the globe is drawn. From Earth & Moon
  up the light is always real. The street's sun settings are untouched.
- **Flights instead of cuts**: every jump is one zoom-out-across-in path
  (van Wijk & Nuij 2003), 1–4 s, stopped by any touch.
- **Three rungs above the Earth**: Earth & Moon to scale, the solar system
  (planets at today's angles, log distances), the 45 star systems within 16 ly
  (scenarios v3), the Milky Way from above with the Sun marked. Above the Moon
  zooming in always heads back to the Earth and lands where you left.

Drawn on today's globe frames from `../macro-view/` and the solar-system and
star boards from `../scenarios/`; the Earth & Moon and Milky Way frames are
canvas drawings in the page itself.
