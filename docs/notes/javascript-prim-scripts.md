---
topic: javascript-prim-scripts
triggers: [scripting/bun.rs, scripting/rules.rs, bun_tests.rs, js_tests.rs, tools/script-host, script-host.sh, cartopolis-script-host, COMPILED_HOST, script_host, script_host_dir, realm-runtime.js, rewrite.ts, ShadowRealm, "bun:jsc", heapSize, BunEngine]
updated: 2026-11-16
---

# Prim scripts in JavaScript, on Bun

Prim scripts run as JavaScript in a Bun process the simulator supervises
(`scripting::bun`, `tools/script-host`) — the only engine since 2026-11-16,
when Luau (`prim-scripting.md`) was removed. The question that note put to any
replacement — can an over-budget script be *suspended*, or only
killed? — is answered here by rewriting the script, not by the engine.

## How the Luau guarantees are rebuilt

JavaScriptCore can stop a script, never suspend one. So `rewrite.ts` turns every
author function into a generator, every call into `yield*` (inline, see below),
and puts a fuel check at every loop iteration and function entry — the two
places Luau's interrupt fires, so `WORLD_FUEL_PER_TICK` and the slice carry over
as numbers. A callback a built-in calls (`arr.map(f)`) cannot be paused: it runs
to its end and an overrun there faults, the same edge Luau has at C calls.

Memory has no allocator ceiling in one shared heap, so it is four layers:

1. the fuel check reads resident memory every 1024 steps and stops a slice that
   grew it past `sliceHeap`;
2. built-ins whose output size is an argument (`repeat`, `padEnd`, `Array(n)`,
   `fill`, `Array.from`, …) refuse past `maxLength`;
3. a heap-snapshot audit attributes the heap to each script and stops one over
   `scriptRetained` — seconds of pause, so only when the process grows;
4. the supervisor (Rust) kills the process past `script_memory_limit_mib` or a
   tick past `script_tick_timeout_ms`, blames the prim that was running (a file
   both processes map) and reloads the rest — which loses their state.

## One realm, frozen — and why not one realm each

All scripts share one ShadowRealm; every intrinsic is frozen before any script
runs, the legacy `RegExp.$1`… statics and `Symbol.for` are deleted, and each
script's state is a closure over its own instance (the SES compartment model).
One realm per script was built first and measured (mac-1, Bun 1.4.4,
2026-11-15): the same firebird handler cost **89 µs** a run spread over 500
realms and **9 µs** in one — JavaScriptCore optimises per realm, so each copy
stayed cold — and 2000 realms held **1.6 GB**. Shared: 2000 content scripts in
~215 MB of process, an idle script ~3 KB of heap.

The cost of that choice is that isolation between scripts rests on the freeze
being complete. `tools/script-host/test/sandbox.test.ts` ("one realm, scripts
apart") tries each known route; a new intrinsic in a Bun release is the thing to
re-check. Escaping the realm reaches only the script process, which holds
nothing but scripts and starts with an empty environment.

## Traps found, each silent

- **Bun puts `process`, `fetch` and `Bun` in every ShadowRealm**, and on 1.4.4
  `Bun` is non-configurable: `Bun.spawnSync(["id"])` from a script ran a program.
  The runtime deletes what it can, shadows `Bun` and `globalThis` with
  parameters of those names around every script, and refuses to start if a new
  undeletable global appears.
- **`node:vm` is not a sandbox**: `this.constructor.constructor("return process")()`
  reaches `process` from an empty context (Bun 1.3.14).
- **`bun:jsc`'s `heapSize()` walks the heap** — 26 µs at 10 MB, 318 µs at 100 MB.
  Read per slice it made ticks quadratic (1000 scripts: 3.1 s a tick). The fuel
  check reads `process.memoryUsage.rss()` (~9 µs at any size) instead.
- **A generator per call costs ~136 ns** against 7 ns for a plain call (1.4.4),
  so calls are rewritten inline rather than through a helper generator, which
  put one on every call, `Math.sin` included. Calls remain ~3× slower than Luau.
- **JavaScriptCore gives up on a runaway regular expression after ~1 s (Linux)
  or ~0.4 s (mac-1) and answers `false`** — silently wrong. One call is a stall
  for every script; a loop of them is what the supervisor's timeout is for.
- **A heap snapshot labels a function only by the name in its source**, so the
  audit finds an instance by a *property* named `__prim_<id>`, not a function.
- `1 << 40` and `4 << 30` are not big numbers in JavaScript; both bit the
  prototype's own limits once (`2 ** 40`).
- `bun:jsc`'s `Worker` `resourceLimits` is ignored (a worker grew to 320 MiB under
  a 32 MiB limit, 1.3.14).

## Measured against Luau

mac-1 (Apple M5 Pro VM), Bun 1.4.4 against the simulator's own host, each in its
own process, 2026-11-15 (`tools/script-host/bench`):

| | Bun | Luau |
|---|---|---|
| 2000 firebirds, ms per tick | 10.8 | 11.1 |
| 2000 footballs, ms per tick | 3.2 | 3.3 |
| 2000 content mix, ms per tick | 35.8 | 31.4 |
| fuel bank of loops, ms | 0.46 | 2.6 |
| per `fib(15)`, ms | ~0.18 | ~0.055 |
| load 2000 scripts, ms | 20–32 | 165–337 |

The content mix at 2000 is a message storm — the packs assume one firebird, and
firebirds read each other's replies as a toggle — so it measures delivery.

## Where the process comes from

`deploy/build/script-host.sh` compiles `src/child.ts` with `bun build --compile
--target=bun-linux-x64` into one executable, `cartopolis-script-host` (87 MB,
Bun 1.4.4 inside; `realm-runtime.js` is imported as text so it travels in it).
The simulator looks for it beside its own binary (`COMPILED_HOST`,
`SimConfig::script_host`), so a server needs neither Bun nor `node_modules`: the
server download carries it next to `cartopolis-server` and its updater swaps
and rolls it back with the program; the hosted sim's deploy publishes it next to
`simulator`. `script_host` names another; `script_host_dir` (with `script_bun`)
runs a checkout's source; with neither and nothing beside the binary, a
`cargo run` runs this checkout's under the `bun` on the path, installing its
packages on first start. The script tests need that Bun and fail without it,
since a pass by skipping would vouch for nothing; the gate also runs
`bun test` here.

**The same executable is the content pages' renderer**, started as `render
<bundle>` (`src/render.ts`, `pages::render`): a second process, never this
one, because what it writes is the site (`docs/mockups/cms`, v3). A change to
`child.ts`'s arguments is a change to both.

A Luau script stored before the switch does not compile as JavaScript; the
hosted world had none left when Luau went (2026-11-16, 64 scripts, all JS).

## What is not done

- The heap audit (`LIMITS.scriptRetained`, what a script holds between ticks)
  is not run by the Rust supervisor; per script only the slice ceiling
  (`sliceHeap`, 16 MiB, what clients are told as `script_memory_bytes`) holds,
  and the process ceiling above it.
- Classes, getters, async functions and an author's own generators are refused
  by the rewrite; errors raised by the Rust-side rule checks carry no line.
