---
name: shots
description: Photograph the Cartopolis client headlessly or drive a running one — `--shot` captures, sweeps and metrics, `--expect` gates, `--flicker` z-fight checks, `--aerial-reference`, the `cargo shots` README presets and field checks, `--shot-mode avatar`, and the client's MCP server (`view`, `state`, `bun tools/mcp.ts`). Use whenever a change has to be seen rather than tested — "what does it look like from there?", a render bug, a visual before/after, regenerating docs/img — and to pick which build node a capture should run on.
---

# shots

Answer "what does it actually look like from there?" without flying a camera by
hand. Everything here is one harness, `crates/cartopolis/src/systems/dev/shot_harness.rs`:
`--shot`, `cargo shots`, `cargo perf` and the MCP server's `view` all push a
`ShotSpec` onto it, and it owns the only cycle that places a camera, waits for
every streamer, quiesces the worker pool and then fires.

## Where to run it — decide what the capture has to prove

A machine with a real GPU (a workstation, or a build node with one) draws a
settled viewpoint in seconds and is evidence for everything. A build node
without one renders through Mesa lavapipe on the CPU: measured over Groningen at
1600×900 on 2026-09-15, **4.1 s a frame and about six minutes a settled
viewpoint**, most of it the harness rendering hundreds of frames while the map
streams. That is trustworthy for **geometry, placement, culling and visibility**,
and no evidence at all for **colour, exposure, bloom, lighting or frame time**
(`docs/notes/headless-shots-software-renderer.md`). GPU nodes are few, so take
one when the question needs it, not by default. Check `midas status` for which
nodes have one; the session hook lists them too.

```bash
midas take gpu
ssh <node> 'WGPU_BACKEND=vulkan cargo run -p cartopolis -- --shot /tmp/x.png --at …'
scp <node>:/tmp/x.png /tmp/x.png     # a node's output stays on its machine
```

## One capture, a sweep, a flythrough

```bash
# one frame, straight down from 180 km
cargo run -p cartopolis -- --shot /tmp/shot.png --at 53.2194,6.5665,180000 --look=-90,0

# an altitude sweep in ONE launch → /tmp/sweep_0_60000m.png, /tmp/sweep_1_1000000m.png, …
cargo run -p cartopolis -- --shot /tmp/sweep.png \
    --at 53.2194,6.5665,60000 --at 53.2194,6.5665,1000000 --look=-90,0

# a 24-frame flythrough between two --at, with the ramp scalars logged to CSV
cargo run -p cartopolis -- --shot /tmp/fly.png \
    --at 53.2194,6.5665,2000 --at 53.2194,6.5665,4000000 --frames 24 \
    --look=-90,0 --csv /tmp/ramps.csv
```

- `--look pitch,yaw` in degrees pairs with `--at` by position. **Write the `=`**
  (`--look=-90,0`): clap reads a leading `-90` as a flag otherwise.
- **The yaw turns counter-clockwise** (90 looks west), so a compass heading is its
  negation. The comment on `ShotSpec` said otherwise until 2026-10-15.
- `--look-at lat,lng` aims at a place instead, recomputed each frame from
  where the camera actually ended up — use it for shallow angles (a sunken road, a
  viaduct), where three metres of altitude moves a fixed aim thirty.
- Runs are **headless**: no window at all (`WinitPlugin` off, `ScheduleRunnerPlugin`
  drives frames), the camera renders offscreen, nothing can occlude and blacken a
  capture, and it works over ssh or in CI with no `DISPLAY`. Scripted runs are
  always **offline** and never read or write `data/user.json`.
- Each capture waits for the streams to go quiet (`--settle`, capped by `--wait`)
  rather than sleeping. Both count *virtual* seconds clamped at 0.25 s a frame, so
  on a slow renderer they run ~6× longer than they read.
- The egui overlay is in the picture (the HUD documents the shot); `--hide-ui`
  drops it. `--ui a,b` opens panels and states — the vocabulary is
  `shot_harness::UI_ELEMENTS` (`dev`, `layers`, `route` seeds a demo route,
  `driver`, `sit`, `fire`, `building`, `inside`, `chat`, `settings`, `all`, …);
  an unknown name is logged as ignored, not refused.
- Environment: `--size WxH`, `--time HOURS`, `--fog D`, `--clouds C`, `--rain R`,
  `--weather-live`. A/B the lighting curves with `--time 6` (dawn) and tell a
  washed-out sky from a washed-out fog with `--time 18 --fog 0`.
- Layers that a headless run has no panel to tick: `--aerial`, `--sewers`,
  `--underground geotop,gasfield|all`, `--satellites`.

## Avatars: `--shot-mode avatar`

`--shot-mode free|earth|avatar`. In free and earth mode the camera sits at `--at`
while the avatar stands at the map anchor — they rarely coincide, so the avatar
is simply off screen, which has caused a lot of "the avatar isn't rendering"
confusion when it was fine and elsewhere. In **avatar** mode the follow camera is
put on the avatar: `--at`'s altitude is the follow distance and `--look` is
(elevation°, azimuth°). Two things make it work although every scripted run is
offline (where there is no avatar and no Follow camera): `ShotHarness::wants_avatar`
spawns one when the run says it is photographing one (`--shot-mode avatar`,
`--ui sit`, `--ui driver`), and `hold_scripted_camera_mode` re-holds the mode every
frame for the duration of the capture. Use it at scale 1.0 rather than
giant-scaling an avatar to find it in a free-camera shot.

### Watching an animation: `--clip`

A still cannot show a foot sliding, a knee bending backwards or a pop where a
loop restarts — those are differences between frames. `--clip NAME` freezes the
avatar at `--clip-frames` (default 12) evenly spaced points of one cycle, captures
each, and tiles them into **`<shot>_sheet.jpg`**: one block per `--at`, the cycle
left to right wrapping at six columns, an orange bar under each tile filled to its
phase. It implies `--shot-mode avatar`; only the first frame of each viewpoint is
flown to, the rest are taken in place. Read the sheet, not the frames.

```bash
# a walk cycle from the side and from the front, the whole body, no HUD
cargo run -p cartopolis -- --shot /tmp/walk.png --clip walk --hide-ui --size 400x600 \
    --time 12 --at 53.21855,6.56730,5 --look=0,90 --at 53.21855,6.56730,5 --look=0,0
```

**Frame it from 5 m with a level camera.** The follow camera aims at the
avatar's head, so from 4 m or a few degrees up the feet leave a portrait frame —
and the feet are most of what a walk cycle is about. Stand it on solid ground:
the Grote Markt above. By a canal (53.2194,6.5665) the avatar drops toward the
water once the run starts, and every frame after the first shows the quay wall.

Names: `idle walk run sit wave interact roll punch kick faint`. The clip is held
still (`avatar::PinnedAnimation`), so the frames are the clip's own poses at those
phases however slowly the renderer goes — but that also means a playback *rate*
fault (feet sliding because the clip plays too fast for the ground speed) is not
visible here: the avatar stands on a treadmill.

## Metrics: one source, three sinks

Every capture measures a `ShotMetrics` — altitude, tier, the altitude-driven
crossfade scalars (`space_factor`, exposure, bloom, `globe_albedo`), near/far,
mesh and render counts, and many layer-specific tallies — into the `shot state`
log line (always), `--csv PATH` (a row per capture) and `--dump-state PATH`
(JSON: an object for one shot, an array for a sweep). They report the *actual*
camera state (the altitude after the teleport snapped it), not what was asked.
**A new inspectable fact is one field on `ShotMetrics`** and appears in all three
at once — that is how to inspect the settled world from a shell without a
recompile per question. `shot state` is also what separates "black image" from
"black scene".

`--frames N` (needs ≥ 2 `--at`) interpolates N viewpoints along the waypoints,
altitude log-interpolated; files are index- and altitude-tagged
(`fly_00_2000m.png`) so the filmstrip sorts in flight order. With `--csv`, a jump
between adjacent rows of `space_factor` / `exposure` / `bloom` / `globe_albedo`
is a **hard cut in a ramp** that no single still can show.

## Gates: `--expect`, `--flicker`, `--aerial-reference`

**`--expect` turns a capture into a test.** `--expect 'settled==true' --expect
'globe_rendered>0'` (quote it — `>` is a redirect) asserts any `--dump-state`
field on every capture and exits non-zero if one fails, logging `shot expect
failed` with the measured value. Operators `>= <= == != > <`; booleans are
`true`/`false`; `==` is exact, so assert floats with `>=`/`<=`. **An unknown key
is a startup error** (a typo that quietly holds is worse than no assertion), as is
an unknown manifest key. A failure does not abort the run, so the rest is still
captured and a red job hands back the pictures that explain it. The `scene-smoke`
CI job is built on this — and on a software rasteriser it may only gate geometry,
placement and visibility.

**`--flicker` measures depth stability.** A z-fight between coplanar surfaces is
deterministic per view matrix, so no single still shows it — which is how every
coplanar-flicker bug here (roads over roads, water over land, the ghost pavements
of 2026-08-29) shipped past a green screenshot job. `--flicker` captures each `--at`
twice ~5 cm apart and reports the flipped pixels as `flicker_px` (3×3-eroded, so
silhouette crawl does not count). Gate it with `--expect 'flicker_px<=N'` in a
pinned environment (`--time 12 --clouds 0 --rain 0`); traffic and pedestrians
still move between the pair, so N sits above that floor, never at 0. Asserting
`flicker_px` without `--flicker` is a startup error.

**`--aerial-reference` is what a render of a real place is checked against.**
Beside each capture it writes `<shot>.aerial.jpg`: PDOK's aerial photograph of the
ground from the camera to its aim point, camera a red dot, line of sight red, aim a
yellow cross (`systems::aerial_reference`; manifest key `aerial_reference = true`;
the Netherlands only). Use it whenever a capture is evidence that a structure, road
or the ground is *right*, not merely drawn: the unified tunnels' first renders
walled a cycle path across a park by Groningen's ring (2026-09-28) while every
metric was self-consistent, and only the photograph showed the path at grade.

## The README images and field checks: `cargo shots`

`docs/shots.toml` remembers named camera positions (`at = [lat, lng, alt]`,
`look = [pitch, yaw]`, and per shot `mode` / `time` / `fog` / `clouds` / `rain` /
`ui` / `hide_ui`; for a view that must match a photograph, `roll` / `fov` / `date`
/ `on_ground` / `[shot.measured]`).

```bash
cargo shots                      # the ungrouped set → docs/img/<name>.png
cargo shots --only globe,route   # a subset
cargo shots --only views         # a group → docs/img/views/<name>.png
```

`cargo shots` is `cargo run -p cartopolis -- --shots`, so every flag above still
applies, and the CSV gains a `name` column. The manifest pins `tile_url`, because a
scripted run never sees the server handshake that supplies one and the built-in
fallback is the *raster* basemap, on which `route` renders "Routing needs a vector
tile source" (`CARTO_TILE_URL` still overrides). To add a view, fly there and press
the dev panel's **📋 Copy as preset**, which puts a `[[shot]]` block on the
clipboard. Other manifests in `docs/` (`*-shots.toml`, `perf.toml`) use the same
format; `cargo perf` flies `perf.toml` (`docs/notes/performance-suite.md`).

**The README shows one contact sheet, not the presets themselves.** The six
ungrouped shots are real screens with the UI open; `docs/img/readme.html` frames
them mockup-style and is rendered to `docs/img/readme.jpg` — `docs/img/README.md`
has the Chromium and the macOS (`tools/render-html.swift`) commands. Shot order in
the manifest matters (a solved route and the demo chats' shared location outlive
their shot). The scenic set the README used to carry is the `views` group.

**A field check is a preset that came from a phone** (`systems::field_check`,
`docs/notes/field-check.md`): Developer tools ▸ View ▸ Field check makes a
photograph, Cartopolis from the same pose and the aerial map of where it
stood, side by side, and sends it to the agent's chat, whose pose `read_chat` hands over as a `[[shot]]` for
`docs/field-checks.toml`.

## When a capture comes back wrong

- **No PNG after a minute of healthy frames, or a black scene with only the
  chrome, exit 0**: the tile workers and Bevy's screenshot readback share
  `AsyncComputeTaskPool`. The harness pauses the pool (`workers::set_paused`) between
  the settle verdict and the shutter; anything new spawned onto that pool can
  starve the readback again (`docs/notes/headless-shots-software-renderer.md`).
- **The run quits silently before the first capture**: something read
  `Query<&Window>` and drew to a scissor rect that does not match the target. With
  no window, `resources::HeadlessSurface` *is* the screen (`--size`); every reader
  of the window must fall back to it.
- **Bisect a render problem by removing one layer at a time**: `CARTO_NO_ATMOSPHERE=1`,
  `CARTO_NO_HAZE=1`, `CARTO_NO_ENVMAP=1`, `CARTO_NO_OCEAN=1`, `CARTO_NO_CLOUDS=1`,
  `CARTO_NO_SHADOWS=1`, `CARTO_NO_SPACE_RAYMARCH=1` (the list is `debug_switches.rs`),
  plus `CARTO_NO_GROUND_DETAIL=1`, `CARTO_NO_LOD_BLEND=1`, and `CARTO_SKY_DOME=1` to
  force the phone's fallback sky.
- **Something looks wrong only once**: it may be a ramp seam (sweep with `--csv`)
  or a z-fight (`--flicker`) rather than a still-image fault.

## Driving a running client: MCP

The client hosts a Model Context Protocol server (`systems::dev::mcp`, server in
`crates/mcp_server`; `docs/notes/driving-the-client-from-an-ide.md`): `state`
(where it looks, whether the map has settled; no picture), `view` (fly and
capture) and `view_here` (re-capture without moving, for a before/after). It is
**not a second screenshot path** — `view` is a `ShotSpec` on the harness above,
and a live harness only differs in parking instead of quitting at the end of its
list. A windowed client therefore captures its *window* (`--mcp-size` applies only
headless), and the camera-mode holds run only during a capture, or an idle
operator would take the camera from whoever is using the client.

**On by default on Linux and macOS**, on a Unix socket only its owner can open
(`$XDG_RUNTIME_DIR/cartopolis/mcp.sock`; the directory *is* the access control,
no token), switched in Settings ▸ General ▸ Editor access while the app runs. A
scripted capture never serves, and `--no-mcp` takes any run out. Unix desktops
only — `build.rs` sets the `mcp` cfg on the same predicate `Cargo.toml` gates the
crate on.

```bash
cargo run -p cartopolis                        # already serving on the default socket
cargo run -p cartopolis -- mcp                 # the stdio relay an editor launches (or `ssh HOST cartopolis mcp`)
cargo run -p cartopolis -- --mcp-headless      # no window; the shape for a VPS
cargo run -p cartopolis -- --mcp-headless --connect HOST --mcp-allow jeroen   # an account an agent drives
bun tools/mcp.ts state                         # the same surface from a shell
```

**Signed in, the client is somebody an agent can be** (`systems::dev::mcp::agent`):
`me`, `read_chat`, `say`, `walk_to`, `teleport`, `build`, `set_script` and the rest
act as the account through the channels a person's actions use, so the server's
ownership and rate checks apply unchanged. **What the agent reads is the risk**:
chat reaches it only from `--mcp-allow` / Settings ▸ Editor access (empty by
default), `view` drops the chat overlay while that list is narrower than everyone,
and a public line counts only with an avatar behind it
(`ChatLine::sender_is_account`), because `say` speaks under a prim name anyone can
choose. A long-running bot on a build node is `midas bot start` (see
`docs/guides/deck-bot.md`).
