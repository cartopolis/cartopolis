---
topic: zoom-ladder
triggers: [free_camera.rs, globe.rs, earth_mode, globe_spin, ORBIT_MAX_DISTANCE_SPACE, map_osd.rs, Teleport, ladder.rs, Rung, "solar system", "Milky Way", galaxy, "night side", night_side, SunMode, "fly to", flight, rail]
updated: 2026-11-01
---

# The zoom ladder: one zoom from the street to the Milky Way

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
stands.

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
