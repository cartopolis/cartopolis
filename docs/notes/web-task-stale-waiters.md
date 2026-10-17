---
topic: web-task-stale-waiters
triggers: [web-task, web_task, "closure invoked recursively or after being dropped", waitAsync, "Atomics.waitAsync", LocalRunnable, bevy_tasks, IoTaskPool, wasm, "web build", Closure, "patch.crates-io", crates/web_task, multithread.rs, platform/storage.rs, webcall.js, cdp_probe.ts]
updated: 2026-10-17
---

# A cancelled task left the browser holding a freed callback

**Symptom.** The deployed web viewer logged `Error: closure invoked recursively
or after being dropped` within seconds of boot and again every few minutes,
always in pairs or bursts, on a desktop browser. Measured 2026-10-12 with
`tools/cdp_probe.ts --gpu` against cartopolis.org/viewer/: five in ten minutes,
all with one stack, `wasm-function[…]` frames only (the published module has no
name section).

**How it was found.** A debug-profile `trunk build` keeps the names, and the
stack then said the dead closure was a `Closure<dyn FnMut(JsValue)>` — a type
the client never creates, so a dependency. Naming *which* one took patching the
local copy of the wasm-bindgen glue so `makeMutClosure` records
`new Error().stack` at creation and prints it when a dead one fires (strip
`integrity=` from that copy's `index.html`, or the browser refuses the edited
file). All 23 dead closures in a minute came from
`web_task::runtime::multithread::LocalRunnable::new`, under
`bevy_tasks::TaskPool::spawn` for ordinary loads — weather, roofs, crops,
municipal cells. This is a reusable method for any closure error the stack
cannot name.

**The bug, in `web-task` 1.1.3** (the latest release, and unfixed on its
`main`). `bevy_tasks` runs every task in the browser on it. A local task that
is waiting parks on `Atomics.waitAsync()` at its own `state` address, with one
`Closure` stored inside the task as the promise's continuation. When the task is
cancelled mid-wait — dropping a `Task` does that, and the streamers drop tasks
for cells they no longer want all the time — the closure is freed with the task,
but the browser still holds the pending promise. Three things then go wrong:

1. The `state` address is freed and reused by a later task.
2. That task's `schedule` notifies the address with a count of **1**, which wakes
   the **oldest** waiter: the stale one. It calls the freed closure — the error.
3. The wake was spent on the stale waiter, so **the new task never runs.** Its
   load simply never finishes, with nothing in the log beyond the error above.

The third is the instability; the error message is only its visible half.

**The fix** (`crates/web_task`, patched in by `[patch.crates-io]`, every
change marked `cartopolis:`). That fork is the only reason the crate is in the
workspace: `web-task` 1.1.3 vendored whole, changed only in
`src/runtime/multithread.rs`, opted out of the workspace lints so it stays
diff-comparable with upstream:

- Each wait gets its own continuation from `Closure::once_into_js`, owned by
  JavaScript: the promise keeps it alive until it runs, and it can never be
  freed while the browser holds it. It holds a `Weak` to the task, which also
  keeps the `state` address allocated, so it cannot be reused while a waiter
  is parked on it.
- `Drop` for the task notifies its own address, so a waiter left behind by a
  cancellation wakes once, fails to upgrade the `Weak`, and returns — and only
  then is the allocation freed. Nothing leaks and nothing stale is left to
  steal a later wake.

**Verified** by running the debug build before and after under the same probe;
see the commit that added the fork for the counts. Drop the fork, and the
`[patch.crates-io]` entry, once upstream publishes a release with an equivalent
fix — check `src/runtime/multithread.rs` there for a continuation that is no
longer a field of `LocalRunnable`.

**The same mistake was in this tree twice**, found while looking for this one,
and is worth knowing as a rule: dropping a `wasm_bindgen` `Closure` does **not**
unregister it from whatever JavaScript object holds it.
`platform::storage::settle` left its `IDBRequest` handlers registered when the
future was cancelled mid-await (fixed with a guard that clears them on drop),
and `webcall.js` kept calling the viewer's event callback after a hang-up had
freed it (fixed by replacing `onEvent` in `close()`). Anything that hands a
`Closure` to the browser owes an unregister in `Drop`, or must be owned by
JavaScript (`into_js_value`, `once_into_js`).
