---
topic: prim-scripting
triggers: [scripting.rs, script_editor.rs, mlua, luau, "script", "sandbox", "fuel", "on_touch", set_geo, sensor, particles, send, on_message, play_sound, loop_sound, prim_sound, SoundName]
updated: 2026-10-04
---

# What the scripting engine is, and what mlua hands you that Luau does not

Prims run **Luau**, through `mlua`, one VM per scripted prim, on a dedicated OS
thread (`cartopolis_simulator::scripting`).

## Why Luau, and why the choice is not really about the language

Three capabilities decided it, and standard Lua has only two:

- `Lua::sandbox()` — frozen stdlib and metatables, `safeenv`.
- `Lua::set_memory_limit()` — a byte ceiling enforced by the allocator.
- `set_interrupt()` returning `VmState::Yield` — **the one that mattered**.
  Standard Lua's `set_hook` can raise an error but cannot *suspend*. With only
  an error, the sole answer to a slice that ran long is to kill the script; with
  a yield, an over-budget script is descheduled and resumes next tick. That is
  the difference between a fuel bank that is a scheduler and one that is a
  killer, and it is why `while true do end` costs its author a slow prim and
  costs its neighbours nothing — **provided every entry point actually runs in a
  coroutine**. See below; for the top-level chunk it did not, for months.

So the comparison that decided this was Luau against *standard Lua*, not against
the wider field. Anything proposed to replace it has to answer the same question
first: can an over-budget script be suspended and resumed, or only killed?

## A fuel yield is silently a no-op outside a coroutine

The single most important thing to know before adding an entry point, because it
looks like nothing is wrong and there is no diagnostic anywhere. Found and fixed
2026-08-10.

Luau's `lua_isyieldable` is `nCcalls <= baseCcalls`, and **only `lua_resume`
raises the base** — so a body running under a plain `pcall` is not yieldable.
When the interrupt hands back `VmState::Yield` there, mlua **discards it without
error**: no log line, no `Err`, no panic. Fuel accounting therefore appears to be
installed and enforces nothing.

`Slot::compile` ran the author's top-level chunk exactly that way, so
`while true do end` at the top level hung the host thread forever — while the
note above, the code comment beside it and the test suite all said it could not.
The test survived because it only covered a loop inside `on_start`, which *is*
resumed and *was* correctly metered. The consequences compounded in a way worth
knowing, because they are what the symptom looks like from outside: the host
thread never returns to `rx.recv()`, `HostMsg` is unbounded and fed by every
scripted prim 20×/s, so the process grows until OOM — and since source is
persisted *before* it runs, a restart replays it and hangs again before any other
prim compiles, still logging "scripts loaded" with the full count.

**The rule: every path that executes author code must go through a fuel-bounded
thread, never `pcall`.** A new entry point is exactly where this comes back, and
the sandbox has a second door onto the same problem — `Slot::handler` uses
`raw_get` because a top-level `setmetatable(getfenv(), …)` is not something
`luaL_sandbox` prevents.

## Two things `sandbox(true)` does not do

Measured against mlua 0.12 / Luau, 2026-08-10.

**`require` is a filesystem read primitive, and it is on by default.** mlua's
`Lua::new()` wires Luau's require-by-string to an `FsRequirer` (`src/luau/mod.rs`
— `self.globals().raw_set("require", require)`), and `sandbox(true)` does not
remove it. "Luau is sandboxed" is a statement about the *language*; the binding
adds capabilities back. `Slot::compile` strips `require` (and `loadstring`)
before sandboxing, and `sandbox_denies_escape_routes` is the test that holds it
shut across version bumps. Anything else mlua adds to globals in a future release
arrives switched **on** — re-read that list when bumping.

**Luau allocates in ~32 KiB pages, so a small memory ceiling is meaningless.**
Loading a one-line handler costs 32,720 bytes; a two-line one costs 65,440. Second
Life's familiar 64 KiB per-script figure is therefore one or two allocations wide
here, and scripts failed to compile with "not enough memory" depending on which
side of a page boundary the source landed — a limit whose behaviour changes with
whitespace. `SCRIPT_MEMORY_BYTES` is 256 KiB (eight pages) and is measured *from
the interpreter's baseline*, not from zero: a fresh sandboxed state is ~386 KiB
before a line of script exists, and that is the host's cost, not the author's.

The corollary nobody costed up front: **one VM per prim is ~386 KiB of floor**.
A world at its default 200 scripts is ~75 MiB of interpreter. That is affordable
and it is also the number that decides whether a world can ever have thousands of
scripted prims — it would need a shared VM with per-script environments, which
gives up the per-script memory ceiling (the allocator limit is per *state*).

## The API is Second Life's shape, and the catalogue is shared

Calls are flat, plain snake_case globals — `set_pos`, `say`, `set_timer_event`,
`play_sound` — which is Second Life's `ll*` shape (one global per verb) without a
prefix. They were `vf`-prefixed (`vfSetPos`) until 2026-10-04; that prefix was
the old brand name in front of every call, and it went without aliases, so a
script written against it fails on its first call and has to be rewritten.
snake_case matches Luau's own library, so a script reads as one language.

Two consequences worth knowing:

- **A script can shadow a host function, and that is not a hole.** The functions
  live in the script's own environment table, which is also where
  `function on_start()` lands — a read-only table could not hold a handler. Since
  every script is its own Luau state, `say = nil` breaks exactly one script.
  (The earlier design had a read-only `vf` *table*; the flat namespace gave that
  up, and the thing it was protecting turns out not to need protecting.)
- **The catalogue lives in `cartopolis_core::SCRIPT_API`**, not in the simulator,
  because the editor renders it and the server implements it. A simulator test
  compiles an empty script and asserts the installed names are exactly the
  catalogued ones — a reference that drifts is worse than none, since an author
  trusts it.

What is *not* in the API is equally deliberate. Luau ships `math`, `string`,
`table` and `os`, so there is no `frand` (`math.random`) and no `get_unix_time`
(`os.time`). A host function that only forwards to the standard library is a
second name for one thing.

`sleep` is written in Lua, not Rust, and that is the interesting one: a host
function cannot yield the coroutine that called it, but Lua code can, and the
scheduler already resumes a yielded handler next tick. So sleeping is "yield
until the clock says otherwise" — no scheduler machinery at all, and a sleeping
script is descheduled by exactly the mechanism that deschedules one which
overran. Its clock (`get_time`) is *simulated* time for the same reason fuel is
counted in instructions: two hosts running the same script should agree.

## `set_pos` is durable in y and not in x/z — `set_geo` is the horizontal one

A prim's horizontal placement is re-derived from its geo anchor
(`prim_geo::place_geo_prims`), so a script's write to `position.x/z` leaves `geo`
stale: the motion is visible and persists until the map re-anchors or the client
reconnects, at which point the prim returns to its anchored spot. Height has no
second owner and stays where a script puts it.

**The fix, since protocol 20, is not to make `set_pos` durable but to give the
anchor its own call.** `set_geo(lat, lng)` writes the anchor directly — which is
the authoritative value, so it survives a re-anchor, a reconnect and a restart —
and `geo_scale(lat)` hands back metres per degree so a script can turn a radius
into an offset. The firebird in the `firebird` content pack flies a 180 m circle
with the two of them.

That is a smaller change than the one this note used to propose (move the
Mercator projection into `cartopolis_core` and have the sim rewrite `geo` from
`position`), and it is better rather than merely cheaper. The projection the
client uses is anchored at the *map* anchor, which is a different frame for every
client and moves when any of them teleports; the sim cannot reproduce it and
should not try. Naming the geographic coordinate outright means no frame has to
be reconciled at all — the same reasoning ADR-029 applied to prim placement in
the first place.

The old advice — that the editor's template bobs a prim up and down rather than
orbiting it — is why that template is what it is, not a statement that orbiting
is impossible.

## `sensor` is the only thing that can see an avatar, and it needed a wire change

An avatar's *sim* position is metres in its own client's anchored frame. Every
client anchors somewhere else and re-anchors when it teleports, so two avatars in
two cities both sit near the origin and "is this avatar near that prim?" has no
answer in sim space at all. A prim, meanwhile, has carried an absolute
`GeoAnchor` since ADR-029.

So protocol 20 added the other half: `ClientIntent.geo`, the avatar's own client
saying where on Earth it is. The sim keeps it, `SimWorld::script_views` measures
it against each scripted prim's anchor once per tick, and `sensor(range)`
returns the nearest one.

Three consequences worth knowing. The neighbours are **pushed with the view**,
not queried — the host thread holds no reference to `SimWorld` (that is what
keeps a script off the tick's write lock), so a sensor reads a snapshot at most
one tick old, exactly as `get_pos` always has. An avatar whose client has not
sent a fix is **not seen**, which is the honest answer rather than a guessed
position. And the range is capped at `SENSOR_MAX_RANGE_M` however wide a script
asks, because the gathering is what costs, not the question.

## One prim reaches another only over a channel

Every host call acts on the script's own prim — `prim_id` is captured by the host
and is not an argument — so a button cannot reach the thing it controls. That is
the *authorisation surface* working as designed (there is no argument an author
can supply that touches somebody else's object) and it also meant a button was
impossible to build.

`send(channel, text)` and `on_message(channel, text, sender)` are the way
through, with `listen(channel)` to opt in. Four properties, each chosen against
an alternative:

- **A message never leaves the host thread.** It is not a `ScriptCommand`: the
  simulation has nothing to do with it, and routing a button press out to the
  tick loop and back would put it behind the world's write lock for no reason.
- **Delivery is after the whole round-robin pass, so it lands next tick.**
  Delivering inline would make "does this listener see it this tick or next?"
  depend on where the scheduler's cursor happened to be — a script's behaviour
  would change with the number of other scripts on the server.
- **Listening is opt-in.** Without it every send would queue an event on every
  script in the world, so one chatty prim would cost every other one work.
- **A script does not hear itself**, which is the one deviation from Second Life
  here. Listening on the channel you send on is the ordinary shape — the beacon
  and the firebird both do it — and hearing yourself makes that an immediate
  feedback loop the author did not write.

A channel reaches **every listening script on the server**. It used to be scoped
to the sender's region, so that an estate's button could not work its
neighbour's door — regions are retired
([retired/regions.md](retired/regions.md)) and nothing that survived them divides
the world into estates. Bringing that property back means a **radius** around the
sender's `GeoAnchor`, measured the way `sensor` already measures, not a new
container. `MAX_MESSAGES_PER_TICK` bounds a sender for a different reason than
`MAX_COMMANDS_PER_TICK` does: a command costs one wire frame, a message costs an
event queued on every listener.

## Sound is a closed library, and the client measures the distance

`play_sound(name, volume?, range_m?)`, `loop_sound(...)` and `stop_sound()`
(protocol 33). Four decisions, each against an alternative:

- **Names, not URLs or uploads** (`cartopolis_core::SoundName`). A URL makes
  every client fetch whatever a stranger's script points at; an upload needs a
  store, a ceiling and a decoder per platform. The client synthesises the seven
  clips at startup (`systems::prim_sound::clips`), as it does its footsteps, so
  every sound is loaded before it is played and there is no `preload_sound`.
  The enum is append-only: postcard encodes the variant index.
- **An unknown name is a script error that lists the library**, never silence.
  A typo that plays nothing is indistinguishable from a speaker that is off.
- **The server sends every one-shot to every connection and the client drops
  it by distance.** The server cannot measure the distance: an avatar's sim
  position is metres in its own client's frame. The listener is the camera, the
  same rule `prim_video` uses, with a squared fade to zero at `range_m` (default
  50 m = `say`, at most 300 m = `shout`). `play_sound` is capped at two per
  script per tick (over it: dropped and counted, not faulted), and a client plays
  at most sixteen one-shots at once.
- **A loop is a level on the prim (`PrimDto::sound_loop`) and is not
  persisted.** Somebody walking up to a fountain must hear it, so it cannot be an
  event; but it lives in `SimWorld` only, carried across an edit by
  `replace_prim`, because a restart runs `on_start` again and that is where a
  standing loop belongs. A client keeps a player only for loops in range.

The names were `vf`-prefixed until the same day this landed; the rename had no
aliases, so every stored script written against `vfSay` and friends fails on its
first call until it is rewritten.

## The unit of accounting is the world

Capacity (`max_prims`, `max_scripts`) is one budget for the server, set in
`cartopolis.toml`. It used to be a column on the region row, on the reasoning
that a region is what an operator hands out — no simulator ever ran more than
one. Fuel is a per-tick bank spent round-robin with a per-script slice cap, so
the cost of a busy world is bounded by the tick, not by the number of scripts in
it.

Sourced from Second Life's twenty years of the same problem, and deliberately
so: per-object script limits, a memory ceiling per script, and "your script got
slower" as the failure mode instead of "your script was killed" are all its
answers, and they are answers to a social problem more than a technical one.
The one place this deviates is that fuel is measured in **interrupt callbacks**
(a deterministic instruction cadence), not wall-clock — a world's capacity is
then reproducible across hosts, and a slow disk cannot make a script look
expensive.

## Source is not snapshot data

A `PrimDto` is re-sent to every observer at the tick rate. Script
source is an order of magnitude larger than the rest of a prim and is of interest
to exactly one person — so it lives in its own table, travels only on
`GetPrimScript`/`SetPrimScript`, and the editor is the only thing that ever asks.
Script *output* does go to everyone (rate-capped at the host), because the
alternative is a per-editor subscription registry that has to be kept in step
with disconnects.
