---
topic: tooling
triggers: [mcp, "--mcp-listen", "--mcp-headless", "systems/dev/mcp", "tools/mcp.ts", "editor access", "MCP token", "agent drives the client", "faster screenshots", "AI assistant"]
updated: 2026-09-16
---

# Driving the client from an editor

The client serves the Model Context Protocol, so an agent in an editor can fly
its camera, photograph what it sees and read what the map is doing.
`systems::dev::mcp` is the code; this is what the shape costs and what was
rejected on the way to it.

## On by default, and switched at runtime

**A launch flag was the wrong shape and lasted about an hour.** What this enables
is an editor attaching to a client that is *already running* — and the moment
somebody wants that is the moment something is on screen and they have a question
about it. A flag asks them to have decided before they knew, and the answer to
"can I attach?" becomes "restart it first", which is the one thing the whole
feature exists to avoid.

So: on by default on the desktop, loopback only, and `AppPrefs::mcp_enabled` is a
switch in Settings that opens and closes the socket while the app runs
(`reconcile_listener`). The flags only decide *where* (`--mcp-listen`), or take a
run out of it (`--no-mcp`, a scripted capture, any non-desktop target).

What that costs is a socket on 127.0.0.1:4750 that refuses everything without the
bearer token. The exposure it adds is another process on the same machine driving
your client — and that process could read the token out of `user.json` anyway, so
the socket is not what would let it in.

Two consequences worth knowing. A **second client** on the same machine finds the
port taken; it says so in that Settings row rather than in a log, because the
reader's next move depends on knowing which of "you switched it off" and "somebody
else has the port" happened. And the bind is **not retried** — a port in use is a
state a person resolves, and a system that kept trying would fill the log and bury
the reason. Switch off and on again to try once more.

## What it replaced, and why the panel could not be fixed

There was an AI panel in the client: a chat box, an `ANTHROPIC_KEY` in `.env`,
and four tools that made one prim per call. Three faults, none of them a
polish problem.

The model was in the wrong process — the editor where the work happens could not
reach it. The app shipped a provider credential. And a prim per call is the
granularity nobody can show working: the agents that build well in Blender write
a *batch* and then render, inspect and revise (Fable 5.1 and GPT-6 Astra both
work this way; the fine-grained tool route is the one with no evidence behind
it). This tree already has the batch format — the content document
`/api/content/export` emits and `/api/content/import` accepts — and the render
half was what was missing.

## The number this exists for

A settled ground-level fill is **27.8 s cold, 25.7 s warm**
([tile-raster-cost.md](tile-raster-cost.md)), and that is *after* the process has
started, loaded its assets and compiled its shaders. On this container's
software rasteriser a single frame over a city measures ~4 s, so a `--shot`
viewpoint is about six minutes. Every one of those costs is paid per **process**,
and `--shot` is one process per batch of viewpoints.

A client that stays up pays them once. What it does not do is make a *first*
visit to a new city cheaper — the fill is the fill. The win is the second look
and everything after it.

**Measured 2026-09-16, in this container** (lavapipe, headless, 640×360, Groningen
from 180 km, `tools/mcp.ts`):

| | wall clock |
|---|---|
| first `view` — fly, settle, capture | **37.4 s** (20.9 s of it the settle) |
| `view_here` — the same frame again, moving nothing | **1.4 s** |

Same client, same viewpoint, back to back. Twenty-seven times, and the second
number is the one the loop runs at: change something, photograph it again, look.
Before this, both of those were a whole process.

The first number is not the comparison to make with `--shot`, which pays process
start, asset load and shader compilation *on top* of the same settle — this run's
client reached "serving" and placed its camera 1.3 s later, but that is after
cargo had already linked a warm binary. Time a genuinely cold `--shot` on the
Deck against a warm `view` there and add the row; the container's software
renderer is spending 462 ms a *frame* here, which drowns everything else.

## There is no second screenshot path

`view` does not take a screenshot. It pushes a `ShotSpec` onto the **shot
harness**, which already owns the only cycle that does this correctly: place the
camera, wait for every streamer to go quiet, quiesce the CPU pool so the GPU
readback is not queued behind a rasterise backlog
([headless-shots-software-renderer.md](headless-shots-software-renderer.md)),
fire the shutter, measure the world it photographed.

A live harness differs from `--shot` in exactly one respect — running off the end
of its list *parks* instead of quitting. Everything else is shared, which is the
point: the perf suite made the same choice for the same reason, coupling to this
harness through three fields rather than growing a second state machine
(`ShotProgress::perf_hold`).

Two consequences. A windowed client captures its **window**, so `--mcp-size` does
nothing there and the returned image is whatever the window is; only
`--mcp-headless` renders offscreen at a size you choose. And the systems that
*hold* something for the duration of a capture — the camera mode, the forced egui
refresh — run only while a viewpoint is in progress (`shot_in_progress`), because
a camera mode held every frame by an idle camera operator is a camera the person
at the keyboard cannot change.

**A windowed capture shares its camera with whoever is sitting there, and input
is not locked out.** Dragging the mouse mid-settle moves the camera the shot is
about to take, and the picture will be of wherever it ended up. That is a
deliberate non-decision: taking the controls away mid-frame is worse than the
occasional spoiled capture, the activity box says a capture is running, and the
metrics report the camera's *actual* pose rather than the one that was asked for
— so a spoiled shot says so in its own numbers.

## Two host shapes

**Windowed** (`--mcp-listen` on an ordinary run) is somebody's session that
happens to be drivable. The user store, the frame limiter, live weather and the
load controller all behave normally — `harness_active` answers "a script owns
this run", and a windowed live harness says no.

**Headless** (`--mcp-headless`) is the shape for a machine nobody is sitting at:
no window, offscreen target, `ScheduleRunnerPlugin`, staying up to answer calls.
This is what a driven remote session wants on the VPS.

Its frame loop is capped at 30 fps, where a scripted `--shot` run takes every
frame it can. The difference is that a scripted run *ends*: taking the whole
machine for a minute is the cost of the run. A server that never ends and spins
at a zero interval is one core burned continuously for a world nobody is looking
at, on a box that is shared. The cap is a minimum frame time, so on a renderer
already slower than 33 ms — this container draws a city in about four seconds —
it changes nothing.

## The token is in `user.json`, and on screen

It is a secret from other processes, not from its owner: the person who needs it
is configuring an editor on the same machine. So it lives in the user store and
is shown in **Settings ▸ General ▸ Editor access**, with Copy and Regenerate
beside it. `CARTO_MCP_TOKEN` overrides it for a headless run with no screen to
read it off, where it is logged once at startup instead.

The address and the token are shown **only while it is actually serving**. They
are what you copy into an editor, and copying them off a client that is switched
off is a configuration that silently will not work.

Binding is loopback by default and reaching another machine is an `ssh -L`, not a
flag — the tunnel is authenticated and encrypted and this is not. A non-loopback
bind is allowed (the `Host` allow-list names whatever was asked for) because a
guard that silently ignores a flag is worse than one that does what it says.

## Rejected: relaying through the simulator

An MCP server in the simulator, relaying camera and capture commands to a named
connected client over the wire, would have solved reachability outright — the
client dials out, so no tunnel and no changing address — and reused accounts for
authentication. It loses on three counts: it needs a protocol revision, it puts
megabyte PNGs on the game wire beside the 20 Hz snapshot stream, and it does not
work **offline**, which is the mode most of this is used in. Worth revisiting if
the thing being driven is ever a phone behind NAT.

## What is not built yet

Authoring (`build` / `edit` / `scene` / `script` over the existing content
document), avatar control, and the geometry checker that would answer "does
anything float or intersect" without a render. The seam for avatar control is
already in the tree and is not a new one: `map_osd::OsdInput` is a second input
source that `avatar::handle_avatar_movement` reads beside the keyboard, and an
agent's wish vector belongs in the same place — which gets it right in both run
modes, since that system owns the ramp online and offline alike.
