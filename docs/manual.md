# Viberfox — User Manual

A short guide to getting around the world, authoring objects, and using the
developer tools. For build/run instructions and system requirements, see the
[README](../README.md).

- [Starting up](#starting-up)
- [Getting around](#getting-around)
  - [Camera modes](#camera-modes)
  - [From the street to space](#from-the-street-to-space)
  - [Going somewhere specific](#going-somewhere-specific)
- [Authoring objects](#authoring-objects)
- [Panels and layers](#panels-and-layers)
- [Developer panel](#developer-panel)
- [Taking screenshots](#taking-screenshots)
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
ground with imported 3D buildings around you.

## Getting around

Movement is camera-relative. The full input reference lives in the
[README controls table](../README.md#controls); the essentials:

| Input | Action |
|-------|--------|
| W / S / A / D (or arrows) | Move / strafe |
| Space / Shift | Fly up / down (while flying) |
| F | Toggle fly / walk |
| Tab | Cycle the camera mode |
| Left-drag | Orbit / look |
| Scroll | Zoom |

### Camera modes

Press **Tab** to cycle modes, or pick one from the camera menu in the top bar:

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

Use the **search bar** in the top menu to jump to a place by name. The world is
anchored in real geography, so moving the anchor re-streams the ground tiles,
buildings, and data layers for wherever you land. Save spots you return to from
the **Bookmarks** panel.

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

The dev panel has a one-click screenshot tool for capturing clean, chrome-free
images (for the README, docs, or sharing):

1. Open the dev panel (**Ctrl + Shift + D**).
2. Frame the shot — pick a camera mode, position, and time of day.
3. Click **📷 Capture slot N**.

On click, the UI is hidden for a single frame and a screenshot of the world (no
panels, menus, or overlays) is saved to `docs/img/screenshot-N.png`. The button
**cycles through three slots** — `screenshot-1.png`, `screenshot-2.png`,
`screenshot-3.png` — so repeated clicks fill the three README images in turn and
then wrap back to slot 1. **Reset to 1** sends the next capture back to slot 1.

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
[`CLAUDE.md`](../CLAUDE.md) and the architecture notes under [`meta/`](../meta/).
