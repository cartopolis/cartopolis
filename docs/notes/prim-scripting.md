---
topic: prim-scripting
triggers: [scripting.rs, script_editor.rs, mlua, luau, "script", "vf.", "sandbox", "fuel", "on_touch"]
updated: 2026-08-10
---

# What the scripting engine is, and what mlua hands you that Luau does not

Prims run **Luau**, through `mlua`, one VM per scripted prim, on a dedicated OS
thread (`viberfox_simulator::scripting`).

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
A region at its default 200 scripts is ~75 MiB of interpreter. That is affordable
and it is also the number that decides whether a region can ever have thousands of
scripted prims — it would need a shared VM with per-script environments, which
gives up the per-script memory ceiling (the allocator limit is per *state*).

## The API is Second Life's shape, and the catalogue is shared

Calls are flat and `vf`-prefixed — `vfSetPos`, `vfSay`, `vfSetTimerEvent` — which
is `ll*` with the serial numbers filed off, deliberately. The people most likely
to write scripts here have twenty years of muscle memory for that namespace, and
a differently-spelled version of the same idea buys nothing.

Two consequences worth knowing:

- **A script can shadow a `vf` function, and that is not a hole.** The functions
  live in the script's own environment table, which is also where
  `function on_start()` lands — a read-only table could not hold a handler. Since
  every script is its own Luau state, `vfSay = nil` breaks exactly one script.
  (The earlier design had a read-only `vf` *table*; the flat namespace gave that
  up, and the thing it was protecting turns out not to need protecting.)
- **The catalogue lives in `viberfox_core::SCRIPT_API`**, not in the simulator,
  because the editor renders it and the server implements it. A simulator test
  compiles an empty script and asserts the installed `vf*` names are exactly the
  catalogued ones — a reference that drifts is worse than none, since an author
  trusts it.

What is *not* in the API is equally deliberate. Luau ships `math`, `string`,
`table` and `os`, so there is no `vfFrand` (`math.random`) and no `vfGetUnixTime`
(`os.time`). A host function that only forwards to the standard library is a
second name for one thing.

`vfSleep` is written in Lua, not Rust, and that is the interesting one: a host
function cannot yield the coroutine that called it, but Lua code can, and the
scheduler already resumes a yielded handler next tick. So sleeping is "yield
until the clock says otherwise" — no scheduler machinery at all, and a sleeping
script is descheduled by exactly the mechanism that deschedules one which
overran. Its clock (`vfGetTime`) is *simulated* time for the same reason fuel is
counted in instructions: two hosts running the same script should agree.

## `vfSetPos` is durable in y and not in x/z

A prim's horizontal placement is re-derived from its geo anchor
(`prim_geo::place_geo_prims`), and the lat/lng projection lives in the **client**
(`geo_nav::world_to_lat_lng`) — the simulator cannot do the conversion, so a
script's write to `position.x/z` leaves `geo` stale. In practice horizontal
motion is visible and persists until the map re-anchors or the client reconnects,
at which point the prim returns to its anchored spot. Height has no second owner
and stays where a script puts it.

This is why the editor's template bobs a prim up and down rather than orbiting
it. Making horizontal motion durable means moving the projection into
`viberfox_core` and having the sim update `geo` alongside `position` — which is a
change to how *every* prim edit works, not just scripted ones, and belongs in its
own diff.

## The unit of accounting is the region

Capacity (`max_prims`, `max_scripts`, schema V7) hangs off the region row because
a region is what an operator hands out. Fuel is a per-tick bank spent round-robin
with a per-script slice cap, so the cost of a busy region is bounded by the tick,
not by the number of scripts in it.

Sourced from Second Life's twenty years of the same problem, and deliberately
so: per-object script limits, a memory ceiling per script, and "your script got
slower" as the failure mode instead of "your script was killed" are all its
answers, and they are answers to a social problem more than a technical one.
The one place this deviates is that fuel is measured in **interrupt callbacks**
(a deterministic instruction cadence), not wall-clock — a region's capacity is
then reproducible across hosts, and a slow disk cannot make a script look
expensive.

## Source is not snapshot data

A `PrimDto` is re-sent to every observer in the AoI at the tick rate. Script
source is an order of magnitude larger than the rest of a prim and is of interest
to exactly one person — so it lives in its own table, travels only on
`GetPrimScript`/`SetPrimScript`, and the editor is the only thing that ever asks.
Script *output* does go to everyone (rate-capped at the host), because the
alternative is a per-editor subscription registry that has to be kept in step
with disconnects.
