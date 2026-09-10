---
topic: web-persistence
triggers: [storage.rs, user_store.rs, photo_store.rs, IndexedDB, idb, OPFS, localStorage, "web build", wasm32, "user.json", "does not persist", "settings lost", navigator.storage, persist, quota, Namespace, read_now, preload, "clear cache", "reset cache", ClearProgress, cache_reset.rs]
updated: 2026-09-10
---

# Everything the client keeps goes through one seam, IndexedDB-backed on the web

`platform::storage` is the whole of this client's persistence on every target: a
get/put over `/`-separated, path-shaped keys in two namespaces. Natively a key is
a file under `cache_root()` or `app_data_root()`; on the web it is a record in
IndexedDB, one object store per namespace.

Until 2026-08-13 the web build persisted **nothing**. `user.json` went through
`std::fs`, which fails on wasm, so every setting, bookmark and folder was
silently lost on reload — the app came up on defaults every time and said
nothing, because each write was best-effort by design.

## The one constraint that shapes all of it

**No browser offers a synchronous read on the main thread**, and two callers need
one: `UserStore::load` runs while the `App` is still being built, and
`photo_store::read_thumb` runs inside an egui pass. Neither can `await`.

Options considered, and why they lost:

- **OPFS** is the closest thing to a real filesystem in a browser, and its
  `createSyncAccessHandle` genuinely is synchronous — **but only inside a Web
  Worker**. Bevy's systems run on the main thread. It solves the wrong half.
- **localStorage** *is* synchronous, and would have let `UserStore` keep its
  exact shape. It caps around 5 MB, holds strings only (so photos could never
  live there), and is main-thread only — meaning two mechanisms instead of one,
  with the split falling exactly where a feature is most likely to cross it.
- **Hydration** — start on defaults, fold the stored values in when they land —
  needs `restore_layer_prefs`, `restore_sky_prefs`, `restore_start_location` and
  the session's startup decision to move onto a hydration event. That is a
  behaviour change on the platform that is hardest to test, in exchange for
  nothing the option below does not give.

## What it does instead: preload, then everything downstream is native-shaped

`run()` on wasm pulls the keys that must be readable synchronously into memory
*before the `App` is constructed*, and starts the client inside the completion:
`user.json`, and every `photos/*.thumb.jpg` (thumbnails only — a full photo is
megabytes and only wanted when someone opens it). `read_now` answers out of that
map. Startup then runs in exactly the order it does natively.

This is the same answer Emscripten reached with MEMFS + IDBFS + `FS.syncfs()`,
and for the same reason. It is not a workaround for something a better API would
fix; it is what a synchronous read *is*, in a browser.

The cost is a few milliseconds of IndexedDB before the app starts, behind a page
that is already showing its loader.

What cannot be preloaded goes through the same `read_now`, which answers
`Cached::Loading` on the first ask, starts the read, and has the value on a later
frame. **`Loading` and `Missing` are different states and collapsing them is a
bug that never recovers**: a caller that caches "not there" the way it would
cache a filesystem miss will keep that answer forever. The photo viewer's
`unavailable` flag exists for exactly this — see `bookmarks::photo_viewer`.

## The namespaces are a durability statement

`Namespace::Cache` is re-fetchable (tiles, 3DBAG, Overpass, transit).
`Namespace::Data` is the user's own (`user.json`, photos). Natively that is the
cache root against the data root, so clearing tile cache cannot take a user's
saved places. On the web it is what the browser is asked to keep: the first Data
write calls `navigator.storage.persist()`, because otherwise a browser under
pressure — or Safari after seven idle days — discards the origin's data without
asking, which reads as the saved places vanishing on their own. A refusal is
logged, not surfaced: best-effort is the status quo, not a new failure.

`the_two_namespaces_do_not_see_each_other` in `storage.rs` is the test that keeps
this real rather than conventional.

It is also what makes `storage::clear(Namespace::Cache)` — Settings ▸ General ▸
Storage, added 2026-09-10 — safe to offer to anybody: the button cannot reach a
saved place, a photo or a setting, because those are not in the half it empties.
`Namespace::Data` is deliberately **not** clearable through the same call. It
would not work as anybody expects: `user.json` is written back from memory on a
timer, so it would reappear within seconds, and "delete everything I have" is a
different feature with a different confirmation.

Two things about a clear are worth knowing before writing another one:

- **It is safe mid-flight.** Deleting a cache entry under a running streamer costs
  a re-fetch and nothing else, which is the same reason `sweep_cache_root` is
  allowed to run at startup while the streamers are already going.
- **The store is the bottom of three caches, not the only one.** In front of it
  sit `tile_source`'s 32 MiB of in-memory bodies and the tiles already drawn on
  screen. Emptying only the store looks, to the user, like the button did nothing.
  `systems::cache_reset` is where the three are sequenced; anything that adds a
  fourth owes it a line there.

## Gaps, deliberately left

- **No eviction of the web cache namespace.** Natively `sweep_cache_root` bounds
  it by size; in a browser the quota is the browser's, and a refused write is
  taken as "the cache is full", latched for the session so the tile streamers do
  not pay an exception per tile. Tiles simply stop being cached; nothing breaks.
- **A write queued on the web is lost if the tab closes in the same frame.**
  There is no API that would have prevented it — the in-memory view is updated
  first, so nothing in the running app is inconsistent.
- **Session prefs still come off the store at `App` build time**, which the
  preload covers. Anything *new* that reads the store before the app exists must
  either be in the preload list or accept defaults on the web.
