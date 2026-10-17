---
topic: simulator-tick-budget
triggers: [tick_loop, WorldSnapshot, avatar_snapshot, broadcast, "tick timing", TickMeter, spawn_blocking, "blocking the executor", journal_mode, WAL, flush_dirty, "world write lock", RwLock, "sqlite is slow", "snapshot is large", tokio, "schedule latency", Bytes, "chat fan-out", crates/simulator/src/net, crates/simulator/src/db.rs, crates/simulator/src/state.rs, crates/simulator/src/prims.rs, crates/simulator/src/chat.rs, crates/simulator/src/library.rs]
updated: 2026-09-15
---

# What one simulator tick may spend, and on what

The simulator is a single Tokio runtime. Every connection handler, every HTTP
route, the script command drain and the 20 Hz tick loop share the same worker
threads, so anything one of them does synchronously is time the others are not
running. This note records what that costs here and which shapes have already
been ruled out, so the next person does not re-derive it.

Prompted by [Principles for fast Tokio
applications](https://dial9-rs.github.io/blog/principles-for-fast-tokio-applications/),
which is worth reading before changing anything in `net.rs`.

## The tick has a budget and now says when it misses it

`net::TickMeter` summarises roughly thirty seconds of ticks into one `tick
timing` line. Two numbers, because they answer different questions:

- **`locked`** — how long `tick_loop` held the world's write lock. Every
  connection's `world.write().await` queues behind this, so it is the number
  that makes *other* people's input feel late.
- **`total`** — the whole tick, including the encoding and fan-out that happen
  after the lock is dropped.

`over` counts ticks in the window that exceeded the period. The line is `info`
when `over > 0` and `debug` otherwise, so a healthy server is quiet and a
struggling one does not need the log level raised first.

A large `total` with a small `locked` is a busy but healthy server. A large
`locked` is the thing to go and fix.

## The repeating snapshot carries no prims, and never usefully did

`WorldSnapshot` has a `prims` field and the tick loop used to fill it with the
whole world — up to the 1,000-prim ceiling — twenty times a second, cloned out of
`SimWorld`, postcard-encoded, and copied once per connected client.

**No client has ever read it after the first one.** `network.rs` builds its prim
entities from the first snapshot it receives and takes the
`received_initial_world` early return on every one after that; prims change
through `PrimUpsert` and `PrimRemoved`. So the largest single thing the server
did per tick reached no screen.

The split is now: `SimWorld::snapshot` (the full world) is sent once, inline at
the handshake, and `SimWorld::avatar_snapshot` (an empty prim list) is what
repeats. This is **not** a wire change — the field is still there and every
connection still receives a full one — so an older client sees exactly what it
always saw.

Measured 2026-09-15 in the agent container, `vps-1` slot, a world filled to the
1,000-prim ceiling, **no client connected** (so this is the clone and the encode
alone, with none of the per-client copies on top). Method: boot the server, read
the `tick timing` line for a 600-tick window, with `avatar_snapshot` returning
the full prim list and then the empty one.

| | mean tick | max tick | mean lock held | encoded snapshot |
|---|---|---|---|---|
| prims in every snapshot | 3.00 ms | 5.58 ms | 0.245 ms | 157,841 bytes |
| prims at the handshake only | 0.063 ms | 0.23 ms | 0.012 ms | ~100 bytes |

That is 2.9 ms of every 50 ms tick — about 6 % of the budget — and 3.2 MB/s per
connected client, for something no client reads.

One consequence to know, and it predates the split: a client whose broadcast
receiver falls behind drops frames as `Lagged`, and a dropped `PrimUpsert` is
never resent. The repeating full prim list *looked* like it would repair that and
never did, because the client discards it. If prim resync becomes a real problem
the fix is to notice the lag and resend, not to go back to broadcasting the
world.

## Fan-out carries `Bytes`, not `Vec<u8>`

`tokio::sync::broadcast` clones its payload for every receiver, so a
`Sender<Vec<u8>>` was a full copy of each frame per connected client. The chat
hub had the same shape in `frame.to_vec()` per recipient. Both carry `Bytes` now,
where a clone is a reference count. The frame is encoded once either way; this is
only about the copies after it.

Anything new that fans one encoded frame out to many connections should carry
`Bytes` for the same reason.

## SQLite is in WAL, and one flush is one transaction

`db::tune` sets `journal_mode = WAL`, `synchronous = NORMAL` and a 5 s
`busy_timeout` at open. The defaults — a rollback journal with `synchronous =
FULL` — create, fsync and delete a separate journal file around every
transaction, which is the worst possible shape for a connection that is shared by
a mutex and written from async worker threads. `db.rs`'s
`the_database_opens_in_wal_mode` pins it; nothing else downstream would notice if
`tune` stopped being called.

`PrimService::flush` (`prims.rs`) wraps the batch in one transaction rather than
letting each `UPDATE` be its own implicit one. It used to run as
`SimWorld::flush_dirty` on the tick loop's thread holding both the database mutex
and the world write lock, so a flush of forty moved prims was forty commits inside
the most contended section in the server. Since 2026-09-29 `SimWorld` holds no
connection at all: the flush copies the dirty rows out under the world lock
(`take_dirty`), releases it, and writes on the blocking pool, as a task of its own
the tick loop starts every 40 ticks. The failure granularity is unchanged: a
rolled-back batch re-marks every id dirty (`restore_dirty`), so the retry writes
the same set the database rejected rather than a half-applied one.

## File reads and writes go through `spawn_blocking`

Three paths were doing synchronous disk I/O on a worker thread in response to a
network message, and all three now hand it to the blocking pool:

- `net.rs`'s `TextureCatalogRequest` (a directory listing plus a manifest read)
  and `TextureRequest` (a whole file).
- `library.rs`'s model upload and download — up to `MAX_ASSET_BYTES` (16 MB)
  each way. Only the *parse* had been moved when that route was written; the
  `fs::write` and `fs::read` either side of it were left behind.

The rule for anything new: if a handler touches the filesystem, it goes through
`spawn_blocking`. The blocking pool is a shared global resource and batching into
it is worth doing, but a 16 MB read on a worker thread is never the trade.

## Not done, and why

- **Delta snapshots for avatars.** The avatars are the only thing left in the
  repeating snapshot and there are as many of them as there are connections. At
  the scale this server runs at they are a few dozen bytes; a delta scheme would
  be a wire change and a resync mechanism for a payload nobody has measured as a
  problem. Measure with `tick timing` first.
- **Reserving cores for the runtime.** The article's point about isolating Tokio
  workers from other OS work applies directly to the hosted simulator, which
  shares a box with nominatim, overpass and the tile server, and which takes
  every core by default through `#[tokio::main]`. Untried here: it is a
  deployment change, not a code change, and there is no measurement yet that says
  the runtime is being descheduled.
- **A second runtime for the tick loop.** Same answer. It is the standard fix for
  keeping latency-sensitive work away from background work, and there is nothing
  yet that says the tick is being delayed by its neighbours rather than by its own
  contents.
