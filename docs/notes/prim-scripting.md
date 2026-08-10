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
  costs its neighbours nothing.

So the comparison that decided this was Luau against *standard Lua*, not against
the wider field. Anything proposed to replace it has to answer the same question
first: can an over-budget script be suspended and resumed, or only killed?

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
