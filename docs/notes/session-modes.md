---
topic: session
triggers: [session.rs, login_screen.rs, solo.rs, network.rs, "sign in", login, "run mode", offline, solo, "--connect", disconnect, auth]
updated: 2026-08-10
---

# Run modes are a runtime state, not a launch-time fork

Offline, solo and split (ADR-016) used to be decided in `lib.rs` before the app
was built and could not be changed without quitting. They are now one state
machine (`systems::session`) driven by a request field, with `systems::login_screen`
as its only UI. What made that cheap: every online system was already
`Option<Res<OnlineSession>>`-guarded, so *offline is the absence of a resource*
rather than a separate configuration of the client. The sim is offline plus an
authority — nothing about the map, the camera or the avatar changes when it
arrives.

## Rejected: a login screen in front of the world

The obvious shape — a full-screen sign-in that the map appears behind once you
choose — was rejected because it makes the first frame after "Enter" an empty
blue void: tiles have not started streaming, so the screen the login was hiding
is replaced by a worse one. It also has to exist *twice*, since going offline
again mid-session needs the same runtime path anyway. The screen is therefore a
modal over the running offline world, and "Explore offline" is a dismissal rather
than a load.

## Rejected: showing it on launch

A viewer that asks who you are before it will draw a map is a worse map. The
remembered session (`UserStore::data.session`) enters itself, the screen lives in
the app menu beside Developer tools, and it opens by itself for exactly one
reason: a session that failed or dropped, which is the only moment the app has a
question it cannot answer. `SessionPrefs::default()` is Solo + `auto_enter`, so a
fresh install boots straight into solo mode exactly as it did before any of this
existed, and a CLI flag still wins over the file — a flag is an instruction for
this launch and is never written back.

**Except where solo does not exist.** Solo is a cargo feature, and the APK is
built `--no-default-features` because nothing cross-compiles `rusqlite` for
Android — so `can_solo()` is `cfg!(feature = "solo") && !cfg!(target_arch =
"wasm32")`, `lib.rs` forces `--offline` on Android, and there the remembered
session never enters anything and no flag can win over it. A fresh install on the
phone boots **offline**, permanently, and that is not a preference the user can
change from the sign-in screen. Two consequences worth keeping: `SessionMode` and
`SessionStatus` gate their dead-code allows on `not(feature = "solo")` rather than
on a target — the platform-vs-feature confusion this tree keeps making, and the
one that hid a broken Android build for two months
(`docs/notes/android-build.md`) — and the sign-in screen's offline arm is the
*only* arm the phone ever reaches, so it is the arm to check when touching it.

## Teardown is the inverse of connect, and both halves are load-bearing

Dropping `OnlineSession` is what actually closes the socket (the network thread's
`select!` sees the intent channel end), but it is not a disconnect on its own: the
server's regions, prims and other people's avatars are ordinary entities and
outlive it, and `AuthIdentity` would keep naming a user who is no longer signed
in. `session::teardown` and `session::connect` are written as a pair for that
reason — anything one installs, the other removes.

The local avatar entity is deliberately *not* despawned. It is spawned at startup
and its offline movement path takes over the moment the session resource is gone,
which is what makes going offline a change of authority rather than a reload.

## The solo sim has to be stoppable

`solo::start` used to park on `std::future::pending()` for the process lifetime,
which was fine when the mode was chosen once. Switching solo → remote at runtime
would then leak a tokio runtime, a SQLite handle and a listening port per switch,
so the sim is now owned by a `SoloSim` whose drop stops it. It also starts
*asynchronously* (`SoloPending`, polled once a frame): opening `data/world.db` and
running migrations is tens of milliseconds, and doing it inline drops frames of
the very modal that is meant to be showing a spinner.

`mpsc`'s channel halves are `Send` but not `Sync`, and a Bevy `Resource` must be
`Sync` — hence the `Mutex` around both ends in `solo.rs`. Nothing contends them.

## The wire has no account creation

`AuthCredential` is `Password` or `Token` (`viberfox_core::protocol`); there is no
register message, and `simulator::ensure_account` is a server-side function solo
mode calls on its own database. So the sign-in screen can only *sign in*. Adding
"create an account" is a protocol change (a `PROTOCOL_HISTORY` row), not a UI one
— which is why the screen does not offer it and does not pretend to.

## The modal's two layering facts

The card must be at the top of `Order::Foreground` and put there with
`move_to_top` every frame: the address-bar card is itself a `Foreground` area, so
an `egui::Window` at its default `Middle` order renders *underneath the menu it
was opened from*.

And the backdrop must be an actual widget (`Sense::click_and_drag` over the whole
viewport), not a painted wash. The world's mouse consumers gate on
`egui_wants_pointer_input()`, which in this client collapses to "egui claimed this
press" (see CLAUDE.md on `is_pointer_over_area`) — so a press on a merely painted
dim layer reaches the camera and spins the world behind the card.
