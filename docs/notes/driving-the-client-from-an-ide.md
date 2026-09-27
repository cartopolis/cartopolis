---
topic: tooling
triggers: [mcp, "--mcp-listen", "--mcp-headless", "systems/dev/mcp", "tools/mcp.ts", "editor access", "MCP token", "agent drives the client", "faster screenshots", "AI assistant", "--mcp-allow", "mcp/agent.rs", AgentWalk, sender_is_account, viberbot, viberfox, "deck bot", "prompt injection"]
updated: 2026-09-27
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

## Being somebody: the agent half (2026-09-27)

Signed in to a server, the client is also an account an agent can *be*:
`me`, `read_chat`, `say`, `walk_to`, `teleport`, `emote`, `build`, `set_script`,
`script_output`, `touch`, `delete_objects`, `my_builds`, `scripting_guide`
(`systems::dev::mcp::agent`). Every action goes out through the channel a
person's does — a walk is a travel wish (`avatar::AgentWalk`, the third input
source beside the keys and the stick), a line is a `ChatSend` or a sealed private
message, a build is `/api/content/import` under the account's own session — so an
agent can do nothing the account could not do by hand, and the server's
ownership and rate checks are the ones that apply. `--mcp-headless --connect`
is the shape for a machine nobody sits at; before this, a headless run was always
offline.

**The risk is what the agent reads, not what it does.** Anything it reads can
try to instruct it, so chat reaches it only from the accounts on the allow-list
(`--mcp-allow`, or Settings ▸ General ▸ Editor access). The list is empty by
default. Everybody else's lines are counted and withheld, names included,
because a username is text somebody chose. The filter is in the client, not the
server: a person and their agent can share one account, the person still sees
everything on screen, and only the agent's view is narrowed. The threat is to
the owner's own model, and the owner's own client is on their side.

Two holes had to be closed before a name was worth filtering on, and both were
live for every user, not only for agents:

- **A scripted object speaks under its own name.** `vfSay` arrives as local chat
  with `sender_id == 0` and the prim's name as the sender. Anybody who builds can
  name a prim after the one account on the list. `ChatLine::sender_is_account`
  is the test, and public lines only count with a real avatar behind them.
- **A private line's sender was its own claim.** The name inside an MLS entry
  comes from the credential the sending device minted, and the server registers
  devices without looking inside their key packages. So `mallory` could mint a
  credential saying `jeroen`. `private_chat::Engine::on_deliver` now drops an
  entry whose credential disagrees with the account the server took it from.
  This fixes the messenger for everyone, not just the agent.

**A picture is the residual.** `view` hides the chrome whenever the client is
online and the list is narrower than everyone, because the chat overlay draws
every line from everybody. What is left in a capture, such as a sign somebody
built or a prim's name, cannot be filtered. That is why the session driving a
bot should hold no other keys. Walking, building and chat need an avatar, so
they refuse on an offline client rather than half-working.

**Staying in a conversation is a long poll.** Nothing wakes a model when a
chat line arrives: a session acts only inside a turn. Checking on a timer
(`/loop 1m`) costs a full turn a minute whether anybody spoke or not. So
`read_chat` takes `wait_s` (at most 600). With nobody on the list saying
anything new, the call is parked (`Pending::Chat`) and answered the frame a
line lands, or at the timeout. An idle bot then costs about six short turns an
hour and answers within a second. While the call waits, the model does
nothing, because it is one open HTTP request.

**The host decides how long a call may wait, not this server.** Claude Code gives
an MCP tool call 60 s by default, so the first bot's `wait_s: 600` came back as
"The operation timed out" at exactly a minute, every time. The limit is the
per-server `"timeout"` (milliseconds) in `.mcp.json`, which overrides
`MCP_TOOL_TIMEOUT`. Progress notifications do not extend it. The bot's is
660 000. Anything driving these tools from another host needs the equivalent.
A waiter whose caller has already given up is **dropped, not answered**
(`oneshot::Sender::is_canceled`). Answering would still advance the chat cursor
and spend the line it woke for on nobody.

**What the first two bot sessions taught (2026-09-27)**, each now a change:

- It polled `me` about once a second while walking: 30-odd calls in one walk.
  `walk_to` takes `wait_s` and answers when the walk ends.
- Two walks jammed a few metres into a route near the Prinsentuin, on a hedge or
  fence corner beside the footpath. A wedged walk now sidesteps (left, right,
  back-and-aside, skipping a corner on the second try) before it gives up.
- It could not sign off when asked. `sign_out` / `sign_in` end and restart the
  session while the client keeps serving. `sign_in` reuses what the launch
  signed in with.
- Every `view` returned about a hundred measurements it never read. The
  default is now a seven-field summary, with `details: true` for the rest.
- Every `read_chat` handed its own lines back, doubling each exchange. They are
  left out now.
- It could not find jeroen, who was 48 m away as "someone". The fact behind
  that stands (avatars carry no names, and only public chat ties one to a
  person), and `me` now says so when it lists an unnamed avatar.

**A walk follows a route, not a line.** The first version walked straight at the
goal and, on its first real test, got 14 m across the Grote Markt's neighbourhood
before a building stopped it. `walk_to` a place now solves a foot route with the
Route panel's own pipeline (`navigation::solve_detached`: its own graph cache,
so it never disturbs a route somebody has on screen) and steers corner to
corner. It falls back to a straight line only when no route comes back. The stuck
test is *movement* (5 s without moving a metre), not closeness, because a route
bends away from its goal as often as towards it. A walk is capped at 2 km. A person can
only be walked up to once they have spoken in public, because avatars carry no
names on the wire. And nothing can be shown to one player only, so a card game
with hidden hands cannot be scripted. `scripting_guide` says so, and builds the
rest of itself from `cartopolis_core::script_api`, so it cannot drift.

## The bot on the Steam Deck

`viberfox` is an ordinary account driven this way, running headless on the
Deck's GPU. The Deck's ssh endpoint is a **rootless podman container** (`arch`),
so its root cannot touch the Deck's netfilter. The first plan, a firewall rule
admitting only the VPS to the MCP port, is not available there. What replaced
it needs no root anywhere:

- The client runs as its own user (`viberbot`) inside that container, from its
  own copy of the binary and `assets/` (`CARTO_ASSET_ROOT`; the compiled-in path
  points into a build slot another agent will resync). It serves on
  **`127.0.0.1:4760`**: loopback, and not 4750, which other agents' clients on
  the Deck use.
- The VPS holds an ssh key that the Deck's `viberbot` accepts **only for a
  tunnel to that one port** (`restrict,port-forwarding,permitopen=…`, no shell).
  A unit on the VPS holds the tunnel open and publishes it on the agent Docker
  network's gateway, so a container on that network reaches the bot at
  `http://172.19.0.1:4750/mcp` with the bearer token, and nothing else. The
  client accepts that `Host` because of `--mcp-host`.

So an agent environment that should drive the bot gets the URL and the token and
**no ssh key**. The setup, the unit names and how to undo each piece are in
`docs/guides/deck-bot.md`.

## What is not built yet

The geometry checker that would answer "does anything float or intersect"
without a render, and a server-side record of an
account's prim ids (export drops them by design, so `my_builds` is this
installation's memory of what it made).
