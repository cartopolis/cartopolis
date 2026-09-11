---
topic: ecs
triggers: [lib.rs, session.rs, tile_loader.rs, network.rs, "Entity despawned", "entity is invalid", try_insert, try_despawn, "set_error_handler", "crashes when minimized", "app closed itself", "generation 1"]
updated: 2026-08-25
---

# A command outlives the entity it names, and on a phone that used to be fatal

`commands.entity(e).insert(..)` records the entity now and applies the insert at
the next sync point. Between those two moments another system can despawn `e`.
Bevy's fallback error handler answers that with a **panic**, which aborts the
process — so a race that costs nothing in principle killed the Android build.

## The one that shipped

Reported 2026-08-25 as "the viewer crashes when minimized". The Android crash
report says:

```
panic: Encountered an error in command `…`: Entity despawned:
       The entity with ID 582v0 is invalid; its index now has generation 1.
```

The log around it, to the millisecond:

1. The app is suspended. Nothing drains the network mailbox while it is off
   screen, and the system aborts the socket a few seconds later.
2. The app resumes. In the **first frame back** `session::drive_session` sees
   `LinkState::Failed`, runs `teardown`, and queues a despawn for every `Prim`
   and `RemoteAvatar` the server owned, and for the local `Avatar`.
3. Same millisecond: panic.

**Which command it was is not established**, and the report could not say —
see the next section for why, and for what was done about it. What *is*
established is the shape: `teardown` is unordered against every streamer and
against `network::apply_network_snapshot`, all of which hold **live** views that
a queued despawn has not touched yet, and any of them queueing an insert for one
of those entities in that frame lands on a dead entity at the flush. Nothing in
that is wrong on its own — every system did the correct thing with the world it
could see.

## Two rules

**Any `insert`/`remove`/`despawn` aimed at an entity another system may retire is
`try_insert` / `try_remove` / `try_despawn`.** "The entity is gone" is the outcome
the caller wanted anyway; a component queued onto an object that no longer
exists is not a failure to report. The server-owned entities are the standing case — `teardown`
is unordered against every streamer — and `network::apply_network_snapshot` is
the mirror image, because a mailbox backlog can retire the same entities in the
same frame from the other side. Converted 2026-08-25: `tile_loader`,
`rendering::refresh_prim_textures`, `avatar::spawn_avatar`, the three despawn
loops and two inserts in `network`, and `teardown` itself.

**And release builds do not panic on a command error at all**
(`lib.rs`'s `App::set_error_handler(bevy::ecs::error::warn)`). This is the same
policy `handle_render_error` follows and for the same reason: an abort on a
handheld reads as "the app closed itself", with the explanation only in `logcat`
on a device with no cable attached. Debug builds keep the panic, because that is
where a new race is meant to be found — so a `cargo run` still fails loudly and
only the shipped artifact is forgiving.

`warn` and not `error`: these are *expected* on the frame a session ends, and a
red line every frame is how a log stops being read.

## The Android build names the command

The report above says `command \`<Enable the debug feature to see the name>\``,
which is bevy's placeholder for a type name it was not built to keep — so the
crash report named the *kind* of failure and nothing whatever about where it came
from, which is why the paragraph above has to say "not established".
`crates/cartopolis_android/Cargo.toml` therefore turns bevy's `debug` feature on
**for the Android build only**: it retains type names in the APK (size, not
runtime) so the next report says which command it was. Android is the platform
where a text file mailed back from a phone is the entire instrument; desktop and
web keep the smaller binary and a terminal.

## What this does not license

The handler is a floor, not a licence to stop thinking about ordering. A command
that fails every frame is still a bug, and it now says so once per frame in the
log instead of once per install in a crash report. If you see
`Entity despawned` in a steady stream rather than at a session boundary, find the
pair of systems and fix the race — the `try_*` call is what makes "gone" an
outcome; it is not what makes it correct.
