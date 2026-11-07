---
topic: zoom-ladder
triggers: [zoom_rail.rs, zoom_rail_control, zoom_scale, edge_push, globe_return_alt, Scale, cosmos.rs, galaxy_points, flight_step, galaxies.rs, GalaxyCatalogue, "cosmos/galaxies.bin", planets.rs, near_stars.rs, gen-near-stars.ts, globe_grab, globe_fling, globe_aim, rail_goal, cosmos_alt, hold_for_flight, FlightPhase, globe_light_direction, "--light", SunMode, "Milky Way", "solar system", ladder.rs]
updated: 2026-11-07
---

# The zoom ladder: one zoom from the street to the edge of the universe

Built 2026-11-01 from `docs/mockups/zoom-ladder/` (v1, v2). What the code
does not say:

## Past the ceiling the camera waits, and only a number climbs

The globe's camera stops at 45,000 km from the Earth's centre
(`ORBIT_MAX_DISTANCE_SPACE`); the far plane, f32 precision and the atmosphere
are all tuned to it. Rather than raise it, the zoom past it moves
`CameraState::cosmos_alt` (up to 10²¹ m) while the 3D camera waits at the
ceiling over the place it left, and `systems::cosmos` paints the rungs above
in egui. Coming back under the ceiling hands the view back to the globe where
it was — which is why a drag out there never turns the globe: turning it
would move where the way down lands.

**The globe stays the Earth out there** (2026-11-01, asked for: the painted
disc that replaced it read as a different ball). The camera does not move;
its `Camera::sub_camera_view` is set larger than the whole image, by the
factor the Earth has shrunk on the map, and offset to where the map puts the
Earth, so the real globe — tiles, atmosphere, night side — shrinks and moves
with it. Space is painted round it with a round hole (`fill_around`); below
2 px across the painted disc takes over. The globe keeps the camera's own
side and its own light while the map turns to above the ecliptic: at the
sizes where the two differ it reads as the same planet.

**A drag pans** (`CameraState::cosmos_pan`): the point under the pointer
follows it, read back through the squeeze (`View::under`), so a drag from the
rim brings what was there to the middle, and the zoom then goes into it. The
pan is clamped so the Earth stays within 0.4 of the screen's short side of the
middle (or where it already was, past that), and that room shrinks to none
between 2× and 1.05× the ceiling — the way down always lands home.

The cosmos is painted on egui's **background layer, before the panel chain**.
A separate `Order::Background` layer paints over panels, as every floating
`Area` does (`egui-shared-root-ui.md`).

## Above the World rung, one map at one scale

Until 2026-11-01 (evening) each rung was a board of its own at its own scale,
crossfaded on height: the globe and a smaller flat Earth showed at once past
the ceiling, the Sun jumped from an arrow at the edge to 0.7 of the screen
away, the solar system stood still from 18 AU to 670 AU (its log scale was
clamped), and the Stars board was a lone "Sun · you" for two decades of zoom
before Alpha Centauri came in. The person steering asked for it gradual, with
the proportions kept clear.

Now every body is placed in one frame (ecliptic metres from the Sun; stars
turned from equatorial, the galaxy from galactic, the camera's frame turned
from render space by matching the pole and the Sun) and projected flat onto
the view, then the distance from the middle goes through `c·ln(1 + r/c)`:
to scale inside `c`, each further ten-fold the same width outside it. So the
next thing out is already at the rim and moves in continuously; rings at round
light-travel distances are the ruler. `c` is effectively infinite at the
ceiling (the Earth here is the size of the globe it replaces, tested), 0.14 of
half the screen height by the Moon's orbit, and lets go again near the top so
the Milky Way is a spiral, not wound into rings round its centre.

What looked wrong on the way and why it is the way it is:

- **Orbits are drawn only when seen from above** (`face_on`): just past the
  ceiling the view is still the camera's, edge-on to the ecliptic, and an orbit
  is a straight line across the screen.
- **The planets' orbits wait until the Sun is the middle**: the squeeze is
  round the middle, and the Earth's own orbit runs through it while the Earth
  is the middle — it came out a teardrop.
- **Star lanes only between stars both on screen**: a lane to a star past the
  rim crossed the whole picture.
- The projection is orthographic, not perspective: a perspective camera put
  the Sun behind it whenever it stood over the viewer's shoulder.

**The Milky Way is built from measurements** (2026-11-02; until then four
invented arms of random dots, asked "is it realistic?"). The arms are Reid et
al. 2019's log-spiral fits to the parallaxes of ~200 young massive stars
(ApJ 885, 131, table 2, `cosmos::ARMS`; arXiv 1910.03357), carried on 150°
past the azimuths measured at falling brightness, because the far side behind
the centre has no parallaxes and drawn empty it read as half a galaxy. The
long bar is Wegg, Gerhard & Portail 2015 (5 kpc half length, 28° to the
Sun–centre line, near end at positive longitude), the disc exponential with
a 2.6 kpc scale length. The Sun is 8.15 kpc (26,600 ly) out, the distance
the arms were fitted with, and lands on the Local arm's inner edge as in the
paper's figure 1 (tested). Check the handedness against that figure turned
so the Sun is left: the bar's near end then points up-left. A tap on "Milky
Way" goes to 6·10²⁰ m, not its band's middle (`zoom_rail::rung_altitude`):
at 3·10¹⁹ m the Sun is still the middle and the disc is squeezed into a
crescent round it.

The planets come from JPL's approximate Keplerian
elements (Standish, valid 1800–2050), checked against the calendar (the Earth
at ~39° of heliocentric longitude on 1 November) and the Mars opposition of
16 January 2025. The 46 star systems within 16 ly are HYG v3.8 (CC BY-SA
4.0), generated by `tools/gen-near-stars.ts` from
`raw.githubusercontent.com/astronexus/HYG-Database/main/hyg/v3/hyg_v38.csv.gz`
(the v4.1 paths on GitHub and Codeberg answered 404 on 2026-11-01).

The Moon is wherever `utils::sun::moon_direction` puts it, which is the sky
dome's approximation (a fixed 18° declination), not an ephemeris — its
longitude only, laid on the ecliptic (the real orbit is within 5° of it).

## Past the Milky Way: the galaxies, to the edge of the universe

Added 2026-11-01 (asked: "is it realistic to zoom out further than the Milky
Way?" — answer: all the way to the edge). Four rungs above the Galaxy one:
Local Group (10²¹–3·10²² m), Nearby galaxies (to 5·10²³), Cosmic web (to
2·10²⁵) and Universe (to `zoom_rail::TOP_M`, 3·10²⁷ — the edge, 46.5 billion
light years comoving, fits a phone held upright there, tested).

The galaxies are **one served file**, `cosmos/galaxies.bin`
(`cartopolis_geo::formats::galaxies`), written by atlas's `galaxies` dataset
from three catalogues — McConnachie 2012 (CADC, the January 2021 public FITS:
144 dwarfs, no row for the Milky Way or Andromeda), Karachentsev's UNGC 2013
(VizieR `J/AJ/145/101`, 869 to ~11 Mpc) and 2MRS (VizieR `J/ApJS/199/26`,
44,599) — and fetched by the client once `cosmos_alt` passes 10¹⁹ m. It is not
embedded like the near stars because it is ~800 KB nobody below the galaxy
needs. Run on the host with `atlas run --datasets galaxies`: a deploy ships
only the binary.

What the code does not say:

- **VizieR's own server in Strasbourg did not answer the VPS on 2026-11-01**
  (connect timeout, three tries) while the Harvard mirror answered in half a
  second; the dataset tries the mirror first. **The mirror flakes too**: on
  2026-11-02 it answered 200 with no body after 60 s for a few minutes, the
  run failed, and a re-run three minutes later took 2 s. The file was first
  published then — until that day everything past the Milky Way was empty
  rings with "the galaxies could not be fetched", which is what the person
  steering saw as "zooming past the Milky Way is broken". After a run, check
  `curl -I https://3dbag.cartopolis.org/cosmos/galaxies.bin`.
- **2MRS is taken only beyond 11 Mpc.** Nearer, a galaxy's own motion is a
  large part of its redshift (Andromeda's cz is −300 km/s); the UNGC has real
  distances for those. Distances are `cz / 70`.
- **The file's order is the thinning**: catalogue by catalogue, and the survey
  brightest first as seen from here, so a phone (and the web) draws the first
  12,000 and keeps every named galaxy; a desktop draws all. Drawn as one mesh
  of squares with one dot per 2-pixel bin, so the squeezed far shell at the
  rim is not overdrawn.
- **The squeeze breathes.** It eases (knee 0.6) over the Milky Way so the
  spiral reads, closes again as the Local Group comes in at the rim, and lets
  go at the top (knee 8, effectively to scale): there, everything mapped is a
  small patch in the middle of the edge's ring. That is the honest picture.
- The view turns once more, to above the **supergalactic plane** (pole at
  l 47.37°, b 6.32°) between 3·10²² and 3·10²³ m, where the Local Sheet and
  the Virgo direction lie face on.
- The empty band across the survey is the Zone of Avoidance (our own disc),
  and the caption says so.
- **Capturing it**: `--shot-mode earth --at 53.2,6.57,3e27` (without the
  mode the camera is not put in the cosmos at all, and the picture is black).
  Before 2026-11-01 any height past ~10²¹ m overflowed the floating origin's
  grid in the harness; the teleport now stops at the ceiling. A capture
  fetches the live file; to try a new one, point `CARTO_3DBAG_URL` at a local
  tree holding `cosmos/galaxies.bin` (`atlas run --datasets galaxies --out
  /tmp/site`).
- Labels go to the clusters first, then by how bright a galaxy looks from
  here (luminosity over distance squared): by luminosity alone Andromeda lost
  its place to galaxies five times farther.
- Clusters are named on their brightest galaxy, matched in 2MRS by position
  within 0.02° (`galaxies::LANDMARKS`): Coma's NGC 4889 is in the survey as a
  bare 2MASX id, not as NGC.

## A jump along the rail flies through every rung

Until 2026-11-01 (late) a tap on a rung's name glided the height at a rate
proportional to what was left (`ZOOM_GLIDE_RATE`, 6/s in log height): fastest
at the start, so a tap from World to Milky Way climbed seven and a half powers
of ten in its first four frames past the ceiling and ten and a half in 0.4 s,
and the view's turn onto the ecliptic, the middle moving to the Sun and the
stars fading in all went by in a handful of frames. The view itself was
continuous; the speed was the fault.

Now the camera follows a height that flies to the goal
(`zoom_rail::flight_step`, `CameraState::rail_way`): it speeds up at 10 powers
of ten a second², holds to at most 5 rungs and at most 7 powers of ten a
second, and slows to a stop on the goal. Rungs past the Earth span up to four
powers of ten each, which is why both caps: by rungs alone the Stars rung went
by in 0.2 s. The street to the edge of the universe takes about five seconds
(tested in `zoom_rail.rs`), one rung under one. A new goal mid-way bends the
movement instead of restarting it, so a finger sliding along the track is
followed smoothly. The height waits at the ceiling until the camera is on the
same side of it, since the globe has to reach the ceiling before the cosmos can
take over. While it flies the rail names the rung it passes and rings the goal, so
the thumb is seen passing each rung. The wheel, a pinch and the − / + discs
still glide as before: they move a step at a time already.

## The rail names one rung, and stands aside on a phone

Reported 2026-11-02 as "the rail is in the way; its names appear suddenly when
I pinch near it", then "the texts beside the rail are not really needed, and
the rail is a bit long". Two causes, both from reading egui's one pointer as
if it were a mouse:

- **The list of all sixteen names opened on egui's hover within 150 pt of the
  track.** On a touchscreen that hover is the first finger down
  (`touch-input.md`, "A hand has several fingers"), so a pinch anywhere near
  the right edge flashed the list up.
- **Any claimed finger on the track set the height**, from the frame it
  landed. A pinch's first finger lands a few hundredths of a second before
  the second, so a pinch begun on the rail jumped the camera before it zoomed.

Now (`map_osd::zoom_rail_control`) the list is gone. The rail names only the
rung the camera is on, beside the thumb, and only while something moves: it
fades out 2 s after the rung last changed or the rail was last used (a mouse
over it counts; a finger never hovers), and the whole rail dims to 55 %. A tap on
the track goes to the middle of the step it landed on (`step_altitude`, the
whole galaxy's exception included), where the list's names used to go; a drag
goes to the height under it. The track takes only a finger that **landed**
on it and stayed alone for 0.12 s or moved 4 pt, and lets go for good when
any second finger comes down. Tracks are 140 pt (desktop) and 120 pt (phone)
since the scales (below); 170 and 140 before, 250 and 200 at first.

On a phone the rail stands at the foot of the right edge, over the dock,
where the + / − pair was — the middle of the edge is where the other hand
pinches, and the bottom is where the thumb is. A desktop keeps it centred
as the mockup has it. The tests replay the finger sequences frame by frame.

## Four scales, a rail for each

Asked 2026-11-07: one rail from the street to the edge of the universe gave
each of sixteen rungs 11 pt, and a slip of the thumb threw the view out of
the Earth. Built from `docs/mockups/zoom-ladder/` v4: the rail spans one
`zoom_rail::Scale` — Globe (Street to World, 8 steps), Solar system, Milky
Way, Universe (4 each) — and a disc on top wearing the scale's icon opens the
four. The heights are the ladder's; the solar system's and the Milky Way's
steps are parts of today's rungs (`Step::span`), and a test holds every step
inside its rung so a layer's board and the rail agree.

- **Every zoom but a jump stops at the scale's ends** (`free_camera`, in the
  cosmos block: every end but the street's lies past the globe's ceiling,
  so only `cosmos_target` is clamped). Half a second of pushing on against
  an end (`EDGE_PUSH_SECS`; a wheel's notches may pause 0.25 s within it)
  crosses, and the zoom goes on from there — the climb is still seamless
  when meant.
- **At an end the − (or +) becomes the next scale's button**, a ring round it
  filling while the push lasts. Held, it is still the zoom, so a held −
  reaches the end and crosses by pushing; a tap crosses at once, but only a
  press that began after the end turned: a − held into the end must not
  jump when it is let go (tested).
- **`zoom_scale` follows the camera, not the other way round**: a jump sets it
  to its goal's scale, and any move that leaves it (a teleport, a shot's
  `--at`, a jump taken over mid-way) to the scale the camera is in. Without
  that a capture at 3·10²⁷ m was clamped back to the World.
- **A jump to the globe returns to the height a jump left it at**
  (`globe_return_alt`); leaving by the edge clears it, and the globe's
  overview, the World step, is the landing then. A tap on the current
  scale goes to its overview. PgUp / PgDn change scale.
- The edge button's ring is the accent, not the mockup's orange: the brand
  orange is the logo's alone (`monochrome-chrome.md`).

## Light and Dark are the sun modes, not a globe setting

The light disc's Auto / Light / Dark are `SunMode::{Auto, Day, Night}`, and a
set time is `Manual`. In the street they are what they always were (the real
clock, noon, midnight). From above, `sky::globe_light_direction` moves the
sun light with the camera — over the shoulder for Light, behind the planet
for Dark — crossfaded on `space_factor`; the globe tiles get no per-frame
material writes for it, because Light turns the night side off and Dark turns
it on everywhere (`GlobeOcean::terminator.w`), so only the directional light
follows the camera. Light's dashed line is where the sun sets **now**, read
from the real clock, not from the noon Light holds the street at.

## A flight lands just under the space tier

A jump flown across the globe (`geo_nav::hold_for_flight`) comes down to
56 km, then lands as an ordinary jump at 47 km looking straight down and
glides to its height. Landing it in the space tier put the camera over the
re-anchored tile's centre instead of the place: 19 km off over Tokyo
(measured 2026-11-01). The rail's goal (`rail_goal`) does the glide.

## Testing a flight through the MCP server

`--mcp-headless` starts in Fly mode; `bun tools/mcp.ts view … --mode earth`
puts it in Earth mode, and `call teleport '{"lat":…,"lng":…}'` then flies.
**Kill the client by name afterwards** (`pkill -f "cartopolis.*mcp-headless"`):
`kill` on the PID of a `cargo run` leaves the client serving the socket, and
the next run's calls reach the old binary — which cost an hour of debugging a
fix that was already in.
