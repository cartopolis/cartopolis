# Viberfox — User Manual

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
- [Developer panel](#developer-panel)
- [Taking screenshots](#taking-screenshots)
  - [The documentation set (`cargo shots`)](#the-documentation-set-cargo-shots)
  - [One-off captures from the dev panel](#one-off-captures-from-the-dev-panel)
- [AI assistant](#ai-assistant)
- [Troubleshooting](#troubleshooting)

## Starting up

```bash
cargo run -p viberfox
```

This launches the client in **solo mode**: an in-process sim starts on loopback and
the client connects to it. Your world is authoritative in that sim and persisted to
`data/world.db`. To join a live world instead, point it at a running sim server:

```bash
cargo run -p viberfox -- --connect <host:port>
```

Or run with no sim at all — single user, no auth, no persistence (also what `--shot`
uses):

```bash
cargo run -p viberfox -- --offline
```

The world opens at street level, standing on a patch of OpenStreetMap-backed
ground with imported 3D buildings around you — or where you left off, since the
last location and camera are restored on startup. To open somewhere specific
instead, name a place at launch (see [Sharing a place](#sharing-a-place)):

```bash
VIBERFOX_PLACE="53.21940, 6.56650, 120m, -12.5°, 45.0°" cargo run -p viberfox
```

## Getting around

Movement is camera-relative. The full input reference:

| Input | Action |
|-------|--------|
| W / S (or ↑ / ↓) | Move forward / backward (camera-relative) |
| A / D (or ← / →) | Strafe left / right |
| Space / Shift | Fly up / down (while flying) |
| F | Toggle fly / walk |
| Tab | Cycle camera: Avatar → Free → Orbit (Earth) |
| Scroll | Zoom |
| Left-drag | Orbit camera / look |
| Left-click | Select a prim |
| Shift + Left-click | Add / remove a prim from the selection |
| Ctrl + Left-drag | Marquee (rubber-band) multi-select |
| Right-click | Context menu (create / edit / delete prims) |
| T / R / S | Gizmo mode: translate / rotate / scale |
| Delete / Backspace | Delete selected prims |
| Ctrl + Shift + D | Toggle the developer panel (day/night slider, etc.) |
| Ctrl + Shift + A | Toggle the AI assistant panel |

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

The camera menu also has a **🌍 View Whole Earth** shortcut once the globe is
ready, which jumps to an orbit that frames the entire planet.

### Going somewhere specific

Click the fox mark in the top-left corner (or press **Ctrl + L**) to open the
address bar, then search for a place by name. Each suggestion says what kind of
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

### Driver mode

With a route solved, **▶ Driver mode** follows it. The map is stripped back to the
basemap and the route (buildings, trees, overlays and weather go, and come back
exactly as they were when you stop), your avatar is replaced by a heading arrow,
and the camera locks into a chase view behind it — no zooming or dragging while it
holds.

The chrome is a satnav's:

- **Top left** — the next manoeuvre and how far to it, with the street name.
- **Under it, the compass** — click it to cycle the map orientation: **3D**
  (tilted, turns with you), **heading up** (flat, turns with you) and **north up**
  (flat, held to north; the arrow turns instead).
- **Right edge** — the arrival clock, the distance left and the time left, plus
  **✕ Stop** to leave.
- **Bottom centre** — your current speed.

Where the position comes from depends on the build. In the browser, driver mode
asks for your device location and the arrow follows your real GPS fix. On the
desktop there is no location source yet, so the arrow is your avatar: driver mode
starts you at the route's beginning and narrates your own movement (W/A/S/D) —
speed, remaining distance and ETA all come off that. If you stray more than about
60 m from the route the card says so.

### Sharing a place

The **⧉** button on the status overlay copies where you are *and* how you are
looking at it:

```
53.21940, 6.56650, 120m, -12.5°, 45.0°
```

That is latitude, longitude, altitude, and the camera's pitch and yaw. Paste it
into the search bar to fly back to the exact framing (the degree signs and the
`m` are optional on the way back in — a bare `53.2194, 6.5665` still works, and
a pair without angles just keeps your current view).

The same string opens a session somewhere:

- **Web:** `?place=53.21940,6.56650,120,-12.5,45` on the page URL.
- **Desktop:** `VIBERFOX_PLACE="53.21940, 6.56650, 120m, -12.5°, 45.0°"` in the
  environment.

A launch place takes precedence over the restored last location, so a shared
link always lands where it says. Everything after the coordinates is optional:
`?place=53.2194,6.5665` picks the spot and leaves the rest alone.

In the browser, ⧉ copies to the system clipboard as it does on the desktop (over
https or on localhost — the browser's Clipboard API refuses a plain-http page).
Pasting *into* the app is desktop-only for now, so on the web the way back to a
place is the URL rather than the search bar.

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

The top menu bar toggles the main panels (also available via keyboard shortcuts):

- **View ▸ AI Panel** (Ctrl + Shift + A) — the [AI assistant](#ai-assistant).
- **View ▸ Dev Panel** (Ctrl + Shift + D) — the [developer panel](#developer-panel).
- **Layers** — toggle OSM-derived data layers and points of interest.
- **Bookmarks** — save and revisit locations.

Live transit boards and vehicles appear in the world when the relevant layers are
active.

## Developer panel

Open it with **Ctrl + Shift + D** (or View ▸ Dev Panel). It gathers tools for
inspecting and tuning the scene:

- **Day / night cycle** — a slider that drives the sun position, with quick
  Sunrise / Noon / Sunset / Midnight buttons and an optional auto-cycle.
- **Distance fog** — atmospheric haze density.
- **Moon** — lunar age, which sets the moon's sky position, phase, and light.
- **Render features** — shadow, atmosphere, and env-map IBL toggles. (Atmosphere
  and env-map IBL apply at startup only — flipping them takes effect after a
  relaunch.)
- **Stats overlay** — FPS, frame time, and live entity count.
- **README screenshots** — see below.

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

`cargo shots` is an alias for `cargo run -p viberfox --profile dev-bevy --
--shots`, so it accepts the rest of the scripted-shot flags too (`--csv`,
`--dump-state`, `--settle`, … — see [`CLAUDE.md`](../CLAUDE.md)). Because it
needs no display, it is a build step: it works over SSH and in CI.

**Adding or moving a view.** Fly there in the app, open the dev panel
(**Ctrl + Shift + D**) and click **📋 Copy as preset** — the current camera lands
on the clipboard as a `[[shot]]` block (position, look angles, time of day, fog,
cloud) ready to paste into `docs/shots.toml` under a new name. From then on that
viewpoint is reproducible instead of something to re-find every release.

Per-shot manifest keys: `name`, `description`, `at = [lat, lng, alt_m]`,
`look = [pitch, yaw]`, `mode` (`free` / `earth` / `avatar`), `time`, `fog`,
`clouds`, `rain`, `ui` (panels to open, as `--ui`), `hide_ui`, `out`. The
top-level `size`, `out_dir`, `settle`, `wait`, `mode`, `hide_ui` and `tile_url`
apply to every shot that doesn't override them. (`tile_url` pins the basemap:
a scripted run never gets one from a server handshake, and the built-in fallback
is raster — which the route planner can't solve against. `VIBE_TILE_URL` still
wins over it.) Unknown keys are a hard error, so a typo fails the run instead of
quietly doing nothing.

### One-off captures from the dev panel

The dev panel also has a one-click screenshot tool for capturing clean,
chrome-free images from wherever the camera is (for sharing or a quick look):

1. Open the dev panel (**Ctrl + Shift + D**).
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

## AI assistant

Open the AI panel with **Ctrl + Shift + A** (or View ▸ AI Panel) for
LLM-assisted world building. It needs an Anthropic API key in the environment:

```bash
ANTHROPIC_KEY=sk-... cargo run -p viberfox
# Optionally override the model:
ANTHROPIC_MODEL=claude-haiku-4-5-20251001 cargo run -p viberfox
```

## Troubleshooting

- **Flicker on Wayland (GLES):** force the Vulkan backend with
  `WGPU_BACKEND=vulkan cargo run -p viberfox`.
- **Build errors after a toolchain/dependency bump:** `cargo clean` and rebuild.

More runtime knobs and known-issue workarounds are documented in
[`CLAUDE.md`](../CLAUDE.md) and the architecture notes under [`docs/`](./).
