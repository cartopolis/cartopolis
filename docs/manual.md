# Cartopolis — User Manual

A short guide to getting around the world, authoring objects, and using the
developer tools. For build/run instructions and system requirements, see the
[README](../README.md).

- [Starting up](#starting-up)
- [Getting around](#getting-around)
  - [Camera modes](#camera-modes)
  - [From the street to space](#from-the-street-to-space)
  - [Going somewhere specific](#going-somewhere-specific)
  - [Sharing a place](#sharing-a-place)
- [Authoring objects](#authoring-objects)
- [Panels and layers](#panels-and-layers)
  - [Height](#height)
- [Developer panel](#developer-panel)
- [Taking screenshots](#taking-screenshots)
  - [The documentation set (`cargo shots`)](#the-documentation-set-cargo-shots)
  - [One-off captures from the dev panel](#one-off-captures-from-the-dev-panel)
- [Letting an editor drive](#letting-an-editor-drive)
- [Troubleshooting](#troubleshooting)

## Starting up

```bash
cargo run -p cartopolis
```

This launches the client **offline**: the streamed map, a local avatar, and no
simulator — single user, no account, nothing persisted server-side. That is where
everybody starts, and for looking at the world it is all you need.

To share a world with other people, sign in to a simulator — **☰ ▸ Sign in…**
opens the card over the live map at any moment. It asks for a server (a bare
hostname, e.g. `cartopolis.org`; the client adds `wss://` and reaches it over
TLS), a username and a password. **Stay signed in on this device** keeps the
session token that server issues, so it stops asking, and comes back to the same
world on the next launch. Once you are in, **☰ ▸ Log out** is the way back to the
offline map, with the world still under you.

The same thing can be asked for at launch, for that run only:

```bash
cargo run -p cartopolis -- --connect <host>
cargo run -p cartopolis -- --offline     # never enters a session (also what --shot uses)
```

There is also a **solo** mode — an in-process sim on loopback, your own world
persisted to `data/world.db` — which has no button because it is a developer
path; `--login <file>` with no server enters it.

The world opens at street level, standing on a patch of OpenStreetMap-backed
ground with imported 3D buildings around you — or where you left off, since the
last location and camera are restored on startup. To open somewhere specific
instead, name a place at launch (see [Sharing a place](#sharing-a-place)):

```bash
CARTO_PLACE="53.21940, 6.56650, 120m, -12.5°, 45.0°" cargo run -p cartopolis
```

## Getting around

Movement is camera-relative. The full input reference:

| Input | Action |
|-------|--------|
| W / S (or ↑ / ↓) | Move forward / backward (camera-relative) |
| A / D (or ← / →) | Strafe left / right |
| Space (or F) | Take off / land |
| E / Q | Fly up / down (while flying) |
| Tab | Cycle camera: Avatar → Free → Orbit (Earth) |
| Ctrl + L | Open the address bar and focus the search field |
| Scroll | Zoom |
| Left-drag | Orbit camera / look |
| Left-click | Select a prim |
| Shift + Left-click | Add / remove a prim from the selection |
| Ctrl + Left-drag | Marquee (rubber-band) multi-select |
| Right-click | Context menu (create / edit / delete prims) |
| T / R / S | Gizmo mode: translate / rotate / scale |
| Delete / Backspace | Delete selected prims |
| Ctrl + Shift + D | Toggle the developer panel (frame rate, draw counts, profiler) |

### Camera modes

Press **Tab** to cycle modes, or pick one from the camera menu in the address bar
(beside the search field):

- **Follow** — third-person camera tracking your avatar.
- **Fly** — a free camera you steer directly; best for framing shots and
  inspecting builds.
- **Earth** — orbit the whole planet.

### From the street to space

There is a single continuous space from the pavement to orbit. Fly straight up
and the view transitions from the street, up through a physically-based
atmosphere, out to a full planetary globe and the black of space — no loading
screens or separate scenes, thanks to the floating-origin coordinate model.

The app menu also has a **View whole Earth** entry once the globe is ready, which jumps to an orbit that frames the entire planet.

### Going somewhere specific

Open the **🔍 Search** panel from the dock (or press **Ctrl + L**) and search for
a place by name. **Back**, **Forward** and **Home** are the row above the field,
browser-style. Each suggestion says what kind of
place it is and how far away it is — which is how you tell the Amsterdam down the
road from the one in Missouri. The world is anchored in real geography, so moving
the anchor re-streams the ground tiles, buildings, and data layers for wherever
you land. Save spots you return to from the **★ Places** panel.

### Planning a route

The **Route** panel plans a route between two points for a car, a bike or on
foot. Each end can be typed as a place name, taken from your current position, or
picked straight off the map with the crosshair button. The result is a line drawn
on the map plus a turn-by-turn list — click a turn to fly to it.

Planning changes nothing else about the view: the buildings, trees and weather
stay exactly as you had them, and the line stays on the map after you close the
panel. **Clear** removes it.

Routing needs a *vector* tile source; against a raster basemap the planner says so
instead of returning a route.

**Long routes** are planned in two passes. Up to about 22 km the whole thing runs
on the detailed street network. Past that, the middle of the route is planned on a
generalised road network — motorways, trunk and main roads down to residential —
while the first and last kilometre stay on the detailed one, so the route still
starts and ends on your actual street. That reaches about 360 km, past which the
planner says how far it is instead of trying. Two things to expect from the
generalised middle: the drawn line is coarser there than in town, and a walking or
cycling route over that distance is planned on roads, because the coarse network
has no footpaths or cycleways in it.

Ferries are routed for walking and cycling but never for driving. The map data
says a crossing is a ferry and not whether it takes cars, and being sent to a
crossing your car cannot board is worse than the detour.

### Driver mode

With a route solved, **▶ Driver mode** follows it. The map is stripped back to the
basemap and the route (buildings, trees, overlays and weather go, and come back
exactly as they were when you stop), your avatar is replaced by a heading arrow,
and the camera takes up a chase view behind it.

The chrome is a satnav's:

- **Top left** — the next manoeuvre and how far to it, with the street name. The
  distance counts down in the steps you would act on rather than in exact metres,
  says **Now** at the junction itself, and the card's bottom edge fills over the
  last few hundred metres so you can see the turn coming without reading it.
- **Under it, the compass** — click it to cycle the map orientation: **3D**
  (tilted, turns with you), **heading up** (flat, turns with you) and **north up**
  (flat, held to north; the arrow turns instead).
- **Right edge** — the arrival clock, the distance left and the time left, the
  route rail (what you will pass, in proportion), and **Stop** to leave.
- **Bottom centre** — your current speed, with the posted limit beside it where
  the map knows one.

The camera drives itself: it sits further back the faster you are going and closes
in on a junction as you approach it. You can still **scroll, drag or pinch** to
look around — a **Re-centre** button appears at the bottom right, and if you do
nothing the view eases back behind the arrow on its own after a few seconds.

**Escape** leaves driver mode from anywhere, which is the way out when a docked
panel is covering the Stop button.

**Straying off the route asks for a new one.** Beyond about 60 m the card says
*Rerouting…* and the route is re-solved from where you actually are; the
destination never changes. **Arriving ends the trip**: the card turns green, names
where you were going, and offers **Done**. The route line stays on the map until
you clear it in the planner.

Where the position comes from depends on the build. In the browser, driver mode
asks for your device location and the arrow follows your real GPS fix. On the
desktop there is no location source yet, so the arrow is your avatar: driver mode
starts you at the route's beginning and narrates your own movement (W/A/S/D) —
speed, remaining distance and ETA all come off that.

### Sharing a place

**🔍 Search ▸ Copy link**, at the foot of the panel, copies where you are *and*
how you are looking at it, as a link:

```
https://cartopolis.org/go?place=53.21940,6.56650,120m,-12.5,45.0
```

The five numbers are latitude, longitude, altitude, and the camera's pitch and
yaw — so whoever follows it arrives at your exact framing, not merely at the same
town. Send it anywhere a link goes.

**What happens when someone taps it** depends on what they have:

| | |
|---|---|
| An Android phone with the app installed | The app opens at that camera |
| Anything else | The web client opens at that camera |

Nothing has to be installed for the link to work, and nothing has to be chosen —
the phone decides. (Android only makes that choice for the app it downloaded from
[cartopolis.org/apk](https://cartopolis.org/apk/); a build you compiled yourself
is signed differently, and for that one the web page shows an **Open this place in
the Cartopolis app** link instead.)

Following a link while the app is *already running* is the one case that does not
work: Android brings it forward without moving the camera. Close it first, or
paste the link into the search bar.

The link is also just a place, so all the old spellings still work:

- **Search bar:** paste the whole link, or the bare `53.2194, 6.5665, 120m` form
  — the degree signs and the `m` are optional, and a pair with no angles keeps
  your current view.
- **Web:** `?place=53.21940,6.56650,120,-12.5,45` on the page URL.
- **Desktop:** `CARTO_PLACE=…` in the environment, holding either the link or the
  numbers.

A launch place takes precedence over the restored last location, so a shared link
always lands where it says. Everything after the coordinates is optional:
`?place=53.2194,6.5665` picks the spot and leaves the rest alone. A link carries
the view and nothing else — your time of day, weather and layers stay yours.

In the browser, Copy link copies to the system clipboard as it does on the desktop
(over https or on localhost — the browser's Clipboard API refuses a plain-http
page). Pasting *into* the app is desktop-only for now, so on the web the way back
to a place is the URL rather than the search bar.

## Authoring objects

You can create and edit 3D objects (prims) in place:

- **Right-click** for the context menu to create, edit, or delete prims.
- **Left-click** selects a prim; **Shift + Left-click** adds or removes from the
  selection; **Ctrl + Left-drag** marquee-selects a group.
- With a prim selected, use the **transform gizmo** — **T / R / S** switch between
  translate, rotate, and scale.
- The edit dialog exposes shape parameters (path-cut, hollow, twist, taper,
  slice) and texturing.
- **Delete / Backspace** removes the current selection.

In online mode, edits are sent to the sim server and replicated to other
connected clients.

## Panels and layers

The **dock** — the row of round buttons centred along the bottom — opens the four
places you navigate between. One at a time; clicking the lit one closes it.

- **💬 Chat** — nearby chat, its channel tabs and the composer. Only present when
  you are connected to a simulator.
- **🗺 Layers** — toggle OSM-derived data layers and points of interest.
- **⚑ Route** — the [route planner](#planning-a-route).
- **★ Places** — save and revisit locations.

The rest live in the **address bar's ☰ menu**, because they are things you visit
rather than flip while looking at the map:

- **Appearance** — pick your avatar. Only when you are signed in to a simulator:
  an appearance is something other people look at, so offline there is no
  occasion for one. Your saved choice comes back with the session.
- **Developer tools** (Ctrl + Shift + D) — the [developer panel](#developer-panel).

Live transit boards and vehicles appear in the world when the relevant layers are
active.

### Life

Everything on the map that *moves* is grouped under **Life** in the Layers panel,
apart from the rest of the map because motion is its own thing: it is what you
switch off to judge a still picture, or to give a slow phone one less job, and it
has nothing to do with which ground textures or buildings you have on.

- **Traffic** — cars driving the streets, generated from the road centrelines.
- **People** — figures walking the pavements beside them, from the same lines.
- **Live vehicles** — real Dutch buses and trains, off their operators' own
  position feed, so this one is off until you ask for it and answers only over the
  Netherlands.

The first two are invented: nobody surveys where a car is, and these are a
plausible city rather than a real one. The third is reported. The panel keeps them
under separate headings for that reason, and the group's master checkbox stops all
three at once.

### Height

The **Height** toggle at the bottom of the Layers panel is the one layer that is off
by default, and the panel says why under it: *AHN elevation — Netherlands only*. On
it, the ground stops being a flat plane and becomes measured terrain, and bridges get
their real deck clearance instead of fighting the road underneath for the same
surface. Outside the Netherlands it has nothing to answer with, so it stays flat.

Two things follow from where the data comes from. It is fetched on demand from
[PDOK](https://www.pdok.nl/)'s public AHN service, so the relief arrives a second or
two after you move — the world builds flat first and re-tears when the measurements
land. And because that is a third party's service, a headless run has to opt in
explicitly with `--height`; there is no Layers panel to tick.

Over a Dutch city the relief is only a couple of metres and the picture barely
changes. The Veluwe is where you see it.

### Aerial photography

**Map ▸ Aerial photography** swaps the drawn ground for real photographs —
[PDOK](https://www.pdok.nl/)'s Luchtfoto RGB, 25 cm, flown in summer — under
everything else the map draws. Off by default, and *Netherlands only* for the same
reason Height is: outside the coverage the tiles quietly stay the drawn map, and
nothing else changes with it. Routing, street names, water shading and both
building layers read the vector map either way, so they behave identically.

It is worth switching on close to the ground: between zoom 15 and 17 the drawn map
is a magnified version of the deepest tiles it has, while the photographs are made
at those scales. A headless run opts in with `--aerial`, and `CARTO_AERIAL_LAYER`
picks a different layer — `Actueel_orthoHR` for 8 cm winter imagery, or a year from
the archive back to 2016.

## Developer panel

Open it with **Ctrl + Shift + D** (or ☰ ▸ Developer tools). It is a **readout,
not a control panel** — what this run is doing, measured live. The things you
*set* are in Settings: the time of day and the weather under **World**, and what
the machine spends drawing the scene under **Graphics**.

It has four tabs.

**Frame** — is it fast, and what is it drawing?

- Frame rate, mean frame time, the worst single frame of the last second, and
  jitter. The four are separate on purpose: a good mean beside a bad worst is a
  hitch, and a good mean beside high jitter is a pacing problem rather than a
  load one.
- **Drawn** — draw calls, entities, and triangles split by what produced them
  (buildings, 3DBAG, map tiles, globe), plus how many of them cast shadows and so
  get re-drawn by every cascade.
- **GPU** — bytes handed to the driver this frame, the worst frame so far, the
  largest single asset, and how much streamed content is resident right now. The
  rate and the total fail differently — a burst times out a command buffer, a
  total exhausts an allocator — so both are here. Below them, whether the GPU
  pipelines this frame needs actually compiled; a failed one is a silent hole in
  the picture, and it is named here.

**View** — where the camera is (lat/lng, altitude, look angles) and which tier it
is in, including the 0–1 crossfade the space tier rides; the nearest street
address to your avatar; and the screenshot tools described below.

**Profiler** — Bevy's own diagnostics, read live:

- **Engine** — the engine's frame rate, frame time, frame count and entity count.
  Measured differently from the Frame tab (a rolling smoothed history against a
  closed one-second window), so a disagreement between the two means the last
  second was not like the seconds before it.
- **Mesh allocator** — the slabs the engine packed your meshes into, and their
  total size. The gap between this and the Frame tab's *resident* figure is
  fragmentation.
- **Process** — CPU as a percentage of one core, and memory as a percentage of
  system memory. Only in a build made with `--features perf`, because it pulls an
  extra dependency; the tab says so when it is absent.
- **GPU passes** — per-pass GPU time and fragment-shader invocations. Off by
  default: it needs GPU timestamp queries (Vulkan and DX12 only), and some
  drivers crash on the readback. Switch on **Time the GPU passes** under Switches
  and restart. Fragment invocations are the number worth trusting across
  machines, because it is a count rather than a time.

**Switches** — the `CARTO_*` startup switches, each of which removes one layer of
the renderer (no sky, no haze, no reflections, no globe ocean, …) or adds an
instrument. They are for bisecting a render problem: several make the picture
worse on purpose, so a run with one on is a run whose rendering proves nothing.
All are read once at startup — set one and restart. The tab says how many are on,
because the answer to "why does it look like that" has to be findable without
remembering that you set one last week. These used to be at the foot of
Settings ▸ Graphics.

Almost nothing here is a new instrument: the Frame and View tabs are a window
onto measurements already taken for the log, the `--dump-state` metrics and the
shareable diagnostics report, and the Profiler is Bevy's own diagnostics, which
until now only a `cargo perf` run could read.

## Taking screenshots

### The documentation set (`cargo shots`)

The images in the README are **generated from remembered camera positions**, not
captured by hand. [`docs/shots.toml`](shots.toml) lists them — each entry is a
name, a lat/lng/altitude, a look direction, and the camera mode, open panels,
time of day and weather that view should be shot in. One command regenerates the
whole set, headless, in a single launch (no window opens):

```bash
cargo shots                      # every preset → docs/img/<name>.png
cargo shots --only globe,route   # just these two
cargo shots --size 2560x1440     # override the manifest's size
```

`cargo shots` is an alias for `cargo run -p cartopolis --
--shots`, so it accepts the rest of the scripted-shot flags too (`--csv`,
`--dump-state`, `--settle`, … — see the [`shots` skill](../.claude/skills/shots/SKILL.md)). Because it
needs no display, it is a build step: it works over SSH and in CI.

**Adding or moving a view.** Fly there in the app, set the time and weather you
want in **Settings ▸ World**, then open the dev panel (**Ctrl + Shift + D**) and
go to its **View** tab and click **📋 Copy as preset** — the current camera lands
on the clipboard as a `[[shot]]` block (position, look angles, time of day, fog,
cloud) ready to paste into `docs/shots.toml` under a new name. From then on that
viewpoint is reproducible instead of something to re-find every release.

Per-shot manifest keys: `name`, `description`, `at = [lat, lng, alt_m]`,
`look = [pitch, yaw]`, `mode` (`free` / `earth` / `avatar`), `time`, `fog`,
`clouds`, `rain`, `ui` (panels to open, as `--ui`), `hide_ui`, `out`. The
top-level `size`, `out_dir`, `settle`, `wait`, `mode`, `hide_ui` and `tile_url`
apply to every shot that doesn't override them. (`tile_url` pins the basemap:
a scripted run never gets one from a server handshake, and the built-in fallback
is raster — which the route planner can't solve against. `CARTO_TILE_URL` still
wins over it.) Unknown keys are a hard error, so a typo fails the run instead of
quietly doing nothing.

### One-off captures from the dev panel

The dev panel also has a one-click screenshot tool for capturing clean,
chrome-free images from wherever the camera is (for sharing or a quick look):

1. Open the dev panel (**Ctrl + Shift + D**) and go to its **View** tab.
2. Frame the shot — pick a camera mode, position, and time of day.
3. Click **📷 Capture slot N**.

On click, the UI is hidden for a single frame and a screenshot of the world (no
panels, menus, or overlays) is saved to `docs/img/screenshot-N.png`. The button
**cycles through three scratch slots** — `screenshot-1.png` … `screenshot-3.png`
— and then wraps back to slot 1; **Reset to 1** sends the next capture back to
the first. These are scratch files: the images the README links to come from
`cargo shots` above, so a stray click here can't overwrite them.

The `docs/img/` directory is created automatically if it doesn't exist. Each save
is confirmed in the console with a `Screenshot saved to docs/img/screenshot-N.png`
line.

Every capture is normalized to a **fixed 1920×1080 (16:9) resolution**, independent
of the window size — the shot is center-cropped to 16:9 and resampled, so all three
README images have identical dimensions no matter how big the window was.

> **Tip:** because the output is center-cropped to 16:9, keep the important part of
> the shot near the centre if your window isn't already widescreen. Maximising the
> window before capturing gives the sharpest result (the fixed resolution upscales
> from smaller windows).

## Letting an editor drive

The client serves the **Model Context Protocol**, so an agent in your editor can
fly its camera and photograph what it sees without you doing it by hand. It is
**on by default** on Linux and macOS (not Windows, for now), listening on a
socket only your own user account can open — so there is no password or token to
copy.

Everything is in **Settings ▸ General ▸ Editor access**: the switch that turns it
off, the socket it is listening on, and the line to paste into your editor's MCP
configuration, with a Copy button beside each. That line runs the client's own
binary as `cartopolis mcp`, a small relay between the editor and the running
client:

```json
{"command": "/path/to/cartopolis", "args": ["mcp"]}
```

The switch takes effect immediately — turning it off closes the socket, turning
it back on opens one — so you never have to restart to let an editor in.

From the command line, if you want to decide per launch:

```bash
cargo run -p cartopolis -- --no-mcp                      # not this run
cargo run -p cartopolis -- --mcp-listen /tmp/second.sock # somewhere else
cargo run -p cartopolis -- --mcp-headless                # no window, for a
                                                         # machine nobody is at
```

Running two clients at once, the second will find another one already serving
on the socket and say so in that Settings row; give it `--mcp-listen` with
another path (and the editor `--socket` with the same one), or `--no-mcp`.

It offers three tools today: `state` (where the camera is and whether the map has
finished loading — no picture, so it costs nothing), `view` (fly somewhere and
photograph it) and `view_here` (photograph the current view without moving, which
is how you compare before and after a change). Every picture comes back with the
measurements taken of the world in it.

While something is driving the client, a box in the top-right corner says what it
is doing and what it answered, so the camera never moves without an explanation
on screen.

To reach a client on another machine, let ssh run the relay there — make the
editor's MCP command `ssh <host> cartopolis mcp`. ssh does the authentication
and the encryption; nothing listens on the network.

There is a command-line client too, for a smoke test or a shell:

```bash
bun tools/mcp.ts state
bun tools/mcp.ts view --at 53.2194,6.5665,300 --look=-20,180 --out /tmp/a.jpg
```

## Troubleshooting

- **Flicker on Wayland (GLES):** force the Vulkan backend with
  `WGPU_BACKEND=vulkan cargo run -p cartopolis`.
- **Build errors after a toolchain/dependency bump:** `cargo clean` and rebuild.

More runtime knobs and known-issue workarounds are documented in
[`docs/notes/`](notes/) and [`CLAUDE.md`](../CLAUDE.md).
