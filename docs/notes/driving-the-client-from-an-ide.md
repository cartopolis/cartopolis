---
topic: tooling
triggers: [mcp, mcp_server, cartopolis_mcp, "--mcp-listen", "--mcp-headless", "systems/dev/mcp", "tools/mcp.ts", "editor access", "MCP token", "mcp.sock", "cartopolis mcp", "mcp/relay.rs", "unix socket", "agent drives the client", "faster screenshots", "AI assistant", "--mcp-allow", "mcp/agent.rs", AgentWalk, sender_is_account, viberbot, viberfox, "deck bot", "prompt injection", look_around, pace_idle, "mcp/surroundings.rs", "bot memory", "midas bot", "tools/bot", inbox, "field check"]
updated: 2026-10-18
---

# Driving the client from an editor

The client serves the Model Context Protocol, so an agent in an editor can fly
its camera, photograph what it sees and read what the map is doing.
`systems::dev::mcp` is the code (its listener and tool surface are the Bevy-free
`cartopolis_mcp` crate, `crates/mcp_server`, split out so rmcp/axum/tower's generic
code — 9.7 % of the client's IR — is not compiled inside the client; it may depend on
neither `bevy` nor `cartopolis`, see `client-build-cost.md`); this is what the shape
costs and what was rejected on the way to it.

## On by default, and switched at runtime

**A launch flag was the wrong shape and lasted about an hour.** What this enables
is an editor attaching to a client that is *already running* — and the moment
somebody wants that is the moment something is on screen and they have a question
about it. A flag asks them to have decided before they knew, and the answer to
"can I attach?" becomes "restart it first", which is the one thing the whole
feature exists to avoid.

So: on by default on Linux and macOS, on a Unix socket only its owner can open,
and `AppPrefs::mcp_enabled` is a switch in Settings that opens and closes the
socket while the app runs (`reconcile_listener`). The flags only decide *where*
(`--mcp-listen PATH`), or take a run out of it (`--no-mcp`, a scripted capture,
any target without the `mcp` cfg).

What that costs is a socket at `$XDG_RUNTIME_DIR/cartopolis/mcp.sock` (else
`$TMPDIR` or `/tmp` + `cartopolis-$UID/mcp.sock`, in a directory created 0700 and
refused if another user owns it). The exposure it adds is another process *of
the same user* driving your client — which could already read and write
everything the client stores, so the socket is not what would let it in.

Two consequences worth knowing. A **second client** on the same machine finds
the first one answering on the socket; it says so in that Settings row rather
than in a log, because the reader's next move depends on knowing which of "you
switched it off" and "somebody else is serving there" happened. A socket that
*refuses* a connection is a client that died without cleaning up, and is
replaced. And the bind is **not retried** — a live server on the path is a state
a person resolves, and a system that kept trying would fill the log and bury the
reason. Switch off and on again to try once more.

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

**Between calls it drops to 2 fps** (`mcp::pace_idle`, 2026-10-01). 30 fps was
still the whole city drawn thirty times a second for nobody: measured on the
Deck bot, idle, **GPU 99 % busy and 0.91 of a core → GPU 6 % and 0.08 of a
core**, every tool unchanged. The full rate comes back for anything in flight —
a capture, a `view` waiting, a walk, a build or a line being confirmed — and
while any streamer is still busy, because every streamer is bounded per frame:
idling through a fill left the Deck bot without shops or benches for minutes
after it started. A call arriving mid-sleep waits at most 0.5 s. A parked `read_chat` or `wait` does
**not** count as in flight: that is how a bot sits in a conversation, for
minutes at a time, and counting it would keep the full rate for exactly the
stretch the idle rate is for. On this container it saves nothing, since an idle
frame on lavapipe already takes 1.3 s.

Not drawing at all between calls was tried and Bevy 0.19 cannot do it: with the
only camera's `is_active` false, `bevy_pbr`'s
`prepare_atmosphere_probe_bind_groups` unwraps an empty transform buffer and the
client panics on the next frame. The types are private to `bevy_pbr`, so there
is nothing on this side to switch off instead. The remaining 6 % is what that
would buy.

## A socket, and no token (2026-10-15)

Until 2026-10-15 this was streamable HTTP on `127.0.0.1:4750` behind a bearer
token, kept in `user.json`, shown in Settings with Copy and Regenerate, and
overridable with `CARTO_MCP_TOKEN`. **The token went because of what it actually
kept out**: other users' processes, and web pages (with a `Host`/`Origin`
allow-list for DNS rebinding beside it). Any process of the same user could read
it out of `user.json` — this note said so. A Unix socket in a directory only the
user can open keeps out exactly those two, with nothing to mint, store, display,
log by accident or regenerate, and a browser cannot reach a Unix socket at all.
The socket file is made 0600 as well, which is what protects a `--mcp-listen`
path in a directory that is not private.

Each connection is **one MCP session served straight over the stream** — rmcp's
async-rw transport, line-delimited JSON-RPC, i.e. the stdio framing. An editor
launches MCP servers as commands over stdio, so the bridge is `cartopolis mcp
[--socket PATH]` (`mcp::relay`): a byte pipe, two threads and two `io::copy`s,
run from the very top of `lib::run` before the logger or anything Bevy exists,
because its stdout is the protocol. Several editors may be attached at once
(a task per connection), and rmcp serves one session's requests concurrently,
so a parked `read_chat` holds up nothing behind it.

**Reaching another machine is ssh running the relay there**: the editor's MCP
command is `ssh HOST cartopolis mcp`. ssh does the authentication and the
encryption and nothing listens on a network. The immediate consumer is a bot on
a remote build machine reached exactly that way, which is also why the relay
had to be a plain stdio program rather than another listener.

Settings ▸ General ▸ Editor access shows the socket and the line to paste into
an editor's configuration — `{"command": "<this binary>", "args": ["mcp"]}`, the
running binary's absolute path (what works for somebody who unpacked a download)
plus `--socket` when the path is not the default. Both are shown **only while it
is actually serving**: copying them off a client that is switched off is a
configuration that silently will not work. An old `user.json` with an
`mcp_token` in it still loads; the field is ignored and not written back.

**No MCP on Windows, for now.** `build.rs` sets the `mcp` cfg on Unix desktops
(Linux, macOS) and `cartopolis_mcp` is a dependency on the same predicate. The
socket's directory is the whole of the access control, and the Windows
equivalent is a named pipe with an ACL — a second arm nobody has needed yet.

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

- **A scripted object speaks under its own name.** `say` arrives as local chat
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
nothing, because it is one open request. **A bot no longer does this
itself** (2026-10-18): a model looping on `read_chat` was the brittle part,
since one forgotten re-arm left the chat unanswered and lapsed the lease. Its
host now holds the wait and hands the model each line as a prompt ("The hosted
bot", below).

**The host decides how long a call may wait, not this server.** Claude Code gives
an MCP tool call 60 s by default, so the first bot's `wait_s: 600` came back as
"The operation timed out" at exactly a minute, every time. The limit is the
per-server `"timeout"` (milliseconds) in `.mcp.json`, which overrides
`MCP_TOOL_TIMEOUT`. Progress notifications do not extend it. Anything driving
the waiting tools from an editor needs a limit like that.
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

## Knowing where it is (2026-10-01)

The first bots could act but could not *perceive* without a picture, and a
picture is the most expensive thing they can ask for. So the second set of tools
(`mcp::surroundings`, `mcp::agent`) is mostly about knowing where it is:

- `look_around` describes the surroundings **in words**: the address or the
  building it is inside, people, objects people built (and which are scripted),
  named places, street furniture by kind, and buildings with the register's
  use, year and monument status, each with a distance and a compass direction.
  It reads only what is already loaded for the person at the keyboard
  (`WorldPlaces`, `BuildingFacts`, the prims), so it costs a lookup. Its
  structured half is capped at ten of each, because a city square has a hundred
  things on it and the data is read in full by the model.
- `find_place` is the address bar's Nominatim lookup, biased (not bounded) to a
  box around the avatar so "the station" means the nearby one.
- `look` photographs **through the avatar's eyes**: the first-person camera,
  captured in place, so it neither teleports nor stands a seated avatar up —
  a `view` in free mode does both. Seated, it turns the head and not the body,
  which faces out of the seat.
- `wait` parks until something happens (chat, somebody arriving, a call, a
  contact request, one of its objects printing, its walk ending), the same long
  poll `read_chat` already was, generalised.
- `follow`, `sit` / `stand`, `interact` (what E does, walking there first),
  `face`, and `edit_object` (an `UpdatePrim` carrying the object's whole current
  state, as the gizmo sends, with only the named fields changed).
- `save_place` / `my_places` and `remember` / `recall`: named places and a
  notebook, per account and server in the `Data` namespace beside the build list
  (`mcp/places/…`, `mcp/notes/…`), because the bot's instructions are a
  read-only file and nothing else outlives a restart. `me` now also says the
  time, the sun and the weather.

- `send_picture` / `see_picture`: pictures in private chats, both ways
  ([pictures-in-private-chats.md](pictures-in-private-chats.md)).
- A **place card** and a **live location share** reach the agent as
  coordinates. A card's text is only its caption, which for an unnamed place is
  "a place", so the first bot told the person who sent one that nothing had
  arrived; `read_chat` now prints the card's latitude and longitude, and `me`,
  `read_chat` and `wait` (`location`) report anybody sharing where they are.

**An outfit is not among them, and cannot be yet.** Appearance is not on the
wire at all — every other client draws a remote avatar in the default — so a
bot changing clothes would change only its own screen.

**An arrival has to allow for the next frame.** The sim keeps moving the avatar
on the last wish until the next intent, so a goal nearer than one frame's step
is passed before the steering runs again. At 30 fps that is 6 cm; on this
container's 3-second frames it walked a bot 5 m past the box it was going to
press and into a building. `steer` now arrives when the goal is within the
coming step and strolls the last four metres.

## The hosted bot (2026-10-18)

`viberfox` is an ordinary account, signed in from a headless client on a build
node and answered by an agent of its own. midas runs both:

```bash
midas bot start --node <node> --name viberfox \
  --agent 'npx -y @agentclientprotocol/claude-agent-acp' \
  --brief tools/bot/viberfox.md --agent-meta @tools/bot/claude-agent-acp.json \
  --agent-mode default \
  --mcp 'cargo run -q -p cartopolis -- mcp --socket /tmp/bot/mcp.sock' \
  -- 'CARTO_DATA_DIR="$PWD/../viberfox-data" cargo run -q -p cartopolis -- --mcp-headless
      --mcp-listen /tmp/bot/mcp.sock --connect cartopolis.org --mcp-allow jeroen'
midas bot log viberfox                  # what it heard, did and thought
midas bot tell --name viberfox "…"      # a line from the operator
midas bot stop --name viberfox          # stops both, gives the node back
```

The client runs on the node; a **host** process runs in the agent container,
where the Claude login is, and outlives the session that started it. midas's
`src/host.rs` and its DESIGN.md (decision 18) have the mechanics. What matters
on this side:

- **The agent is anything that speaks the Agent Client Protocol** (ACP: JSON-RPC
  on stdio, as Zed drives Claude Code, Codex or Gemini CLI). Claude Code comes
  through its official adapter. Swapping the agent changes `--agent` and
  `--agent-meta`, nothing here. Native Claude-only mechanisms (Remote Control
  sessions, `--channels`) were rejected for exactly that; MCP's own "sampling"
  is deprecated in the 2026-07-28 spec and Claude Code never supported it.
- **Its only tools are this client's.** The session has one MCP server, `bot`
  (`midas bot mcp`). The host grants a permission only to that server's tools.
  `tools/bot/claude-agent-acp.json` also turns off Claude Code's built-in tools,
  settings files and every other MCP server, so the agent's own fence stands
  in front of midas's. A chat-driven agent with a shell would be a shell for
  whoever chats; it has none. `--agent-mode default` matters as much: without
  it the adapter took "auto" from the container's own Claude settings, and its
  auto-approval check, not midas, refused the bot's first private `say`.
- **Its input is `inbox`**, a tool made for the host and not the model: it
  waits like `read_chat` and answers with the lines as `read_chat` words them,
  then every picture from a private chat as an image. A field check also
  carries its pose in words, because a host passes content blocks on and drops
  the structured data. Nothing at all comes back when nobody spoke. Each answer
  becomes one prompt, or joins the next one while a turn runs.
- **`CARTO_DATA_DIR` outside the checkout** (one shell word, so `$PWD` is the
  node's checkout). Without it the private-chat keys live in the checkout's
  `data/`, which a node's full sync (`git clean -fdx`) deletes, and every such
  restart made the bot a new device, eight in a week, each one more add that
  can leave a chat in two epochs (`encrypted-chat.md`).
- **The agent answers with `say`** and its text output goes only to the
  transcript; `tools/bot/viberfox.md` tells it so.
- **The subscription login is for the owner's own use.** Anthropic's terms
  allow an unmodified Claude Code signed in with your own subscription. A bot
  that answers other people as a service needs an API key in the agent's
  environment instead. That is a configuration change on the host and no code.

It replaced (both removed 2026-10-18) the 2026-09-27 setup: a `viberfox-bot`
container on the VPS, running a Remote Control Claude session told to loop
`read_chat` itself, reaching a client on the Deck through an ssh tunnel
(`viberbot-mcp-tunnel.service`) to a bearer-token HTTP listener the socket
transport above has since removed.

## What is not built yet

The geometry checker that would answer "does anything float or intersect"
without a render, and a server-side record of an
account's prim ids (export drops them by design, so `my_builds` is this
installation's memory of what it made).
