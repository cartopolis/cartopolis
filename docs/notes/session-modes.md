---
topic: session
triggers: [session.rs, login_screen.rs, network.rs, "sign in", login, "run mode", offline, solo, "--connect", disconnect, auth]
updated: 2026-09-26
---

# Two modes, decided at runtime: offline and online

The client is **offline** (no sim: the streamed map and a local avatar) or
**online** (a simulator reached over the network), one state machine
(`systems::session`) driven by a request field, with `systems::login_screen` as
its only UI. What makes the switch cheap: every online system is
`Option<Res<OnlineSession>>`-guarded, so *offline is the absence of a resource*
rather than a separate configuration of the client. The sim is offline plus an
authority — nothing about the map, the camera or the avatar changes when it
arrives.

## Removed: solo mode (2026-08-21)

There was a third mode — *solo*, an in-process simulator on loopback behind a
`solo` cargo feature, ADR-016's one-command dev loop — and it is gone, because
three modes were an architecture quiz nobody needed: solo already went through
the same authenticated connect path as any server, so it was "online to
yourself" wearing a mode of its own, plus a bundled SQLite that neither wasm
nor Android could compile, a `can_solo()` gate, dead-code allows keyed to the
feature, and a feature flag every build lane had to remember (forgetting it is
how the phone shipped silent for months). The dev loop is now explicit:
`cargo run -p cartopolis_simulator` (provision an account once with
`--create-admin dev` + `CARTO_ADMIN_PASSWORD`), then sign in to
`127.0.0.1:4747` — remembered thereafter like any server, or pinned with
`--connect`/`--login`. The simulator crate stays a *dev-dependency* of the
client so the loopback tests still boot the real thing. A `user.json` that
remembers `"solo"` deserialises as offline (`SessionKind` carries
`#[serde(other)]` on `Offline`; an unparseable kind would quarantine the whole
file, bookmarks included).

## Rejected: a login screen in front of the world

The obvious shape — a full-screen sign-in that the map appears behind once you
choose — was rejected because it makes the first frame after "Enter" an empty
blue void: tiles have not started streaming, so the screen the login was hiding
is replaced by a worse one. It also has to exist *twice*, since going offline
again mid-session needs the same runtime path anyway. The screen is therefore a
modal over the running offline world, and dismissing it is not a load — you were
already offline, which is why the card no longer offers "offline" as a *choice*
at all (below).

## The card asks one question, and offline is not one of the answers

It opened on three rows — Explore offline / Solo simulator / Connect to a server —
which asked a first-time user to pick an architecture before they had seen a map,
and two of the three answers were "carry on as you are". Since 2026-08-17 it is a
form for a *server*: address, user, password, one tick. Offline is where everybody
starts (`SessionPrefs::default()` is `Offline` + `auto_enter`, so a fresh install
opens onto the map and is asked nothing) and is reached again by **☰ ▸ Log out**,
which is a single menu entry rather than a button inside the modal you would have
to open first. A kind an older `user.json` may
still name (`"solo"`) reads as offline (see above).

The two tick-boxes became one for the same reason. "Remember me on this device"
and "Enter this way next time" were separately settable, and the combinations that
made them different are both broken: a remembered token that still asks, or an
auto-enter with no token, which reaches the server with an empty password and
fails. One tick writes both.

## Rejected: showing it on launch

A viewer that asks who you are before it will draw a map is a worse map. The
remembered session (`UserStore::data.session`) enters itself and the screen lives
in the app menu beside Developer tools. A CLI flag still wins over the file — a
flag is an instruction for this launch and is never written back.

**Since 2026-08-18 the card opens from that menu entry and from nothing else, and
a live session is the only thing that closes it.** It used to raise itself
whenever a session failed or dropped, and `lib.rs` opened it on a launch with
nothing to enter — a modal the app pushed in front of you, mid-flight, for a
question you had not asked, and on the first frame the loading-screen-pretending-
to-be-a-login this whole design rejects. What a failure owes the user is the
*reason*, not a form: `ui::render_main_menu` prints `SessionState::error` under
the account entry, next to the "Sign in…" that offers to try again. A failure
arriving on a card that is already up still lands on the card — the question is
open, so the answer belongs there.

Closing on `session.is_online()` rather than on the card's own `awaiting` flag is
deliberate for the same reason the opening rule is one place: a sign-in that
arrives from startup's auto-enter or from `--connect` answers the card's one
question exactly as a submit here does, and a form still asking it over a world
you are already in has nothing left to submit.

`SessionPrefs::default()` used to be Solo + `auto_enter`, i.e. a fresh install
booted straight into an in-process simulator with a SQLite database, for somebody
who had come to look at a map. It is Offline + `auto_enter` now, which reads as
"enter offline without asking" and is the only default that leaves the card shut.
(Android additionally forced `--offline` until 2026-08-21, which is the bug that
made "stay signed in" a no-op there — `docs/notes/android-build.md`.)

## A lost session comes back by itself (2026-09-26)

Android takes a backgrounded app's socket away, and the client only learns so on
the first frame back — "Software caused connection abort (os error 103)". Until
2026-09-26 that was treated like any failure: teardown, offline, the reason under
the sign-in entry, and the user signing in again by hand with "stay signed in"
ticked. It read as being logged out every time you switched apps.

A link that fails **after it was online** is now retried by `drive_session` with
the `ConnectAuth` it was opened with — so it rides the remembered token exactly as
a launch does — on `RECONNECT_DELAYS_S` (immediately, then about half a minute of
back-off), reading as *Connecting…* throughout and keeping the chat log.
`AppLifecycle::WillResume` calls `SessionState::retry_now`, because a back-off
spent in the background against a dead network has run out by the time anyone
looks. Three things stop it, and each is deliberate:

- **Any request.** A log-out or a sign-in elsewhere wins over a reconnect.
- **A refused credential** — `LinkState::Rejected`, split from `Failed` for this
  and set by both carriers. A dropped socket is worth another try; an expired
  token is refused identically every time.
- **A first sign-in that fails is never retried.** Its reason belongs on the card
  in front of whoever pressed the button, not behind thirty seconds of spinner.

The background-connection service (`docs/notes/background-connection.md`) is the
other half: with it on the socket is not taken away at all, and this is only
what happens when it is.

## Teardown is the inverse of connect, and both halves are load-bearing

Dropping `OnlineSession` is what actually closes the socket (the network thread's
`select!` sees the intent channel end), but it is not a disconnect on its own: the
server's prims and other people's avatars are ordinary entities and outlive
it, and `AuthIdentity` would keep naming a user who is no longer signed
in. `session::teardown` and `session::connect` are written as a pair for that
reason — anything one installs, the other removes.

**The local avatar goes with the session**, and is spawned by
`local_avatar::sync_local_avatar` when one comes up. It used to be spawned at startup and
never despawned, so offline left a body on the map with no sim behind it:
nothing owned its position, nobody else could see it, and the Follow camera that
is its whole point was already refused (`free_camera::follow_available`). Offline
is the streamed map and a camera. The one exception is the shot harness — every
scripted run is offline, so `ShotHarness::wants_avatar` reads what each shot says
it is of (`--shot-mode avatar`, `--ui sit`, `--ui driver`) and spawns one for
those.

## The wire has no account creation

`AuthCredential` is `Password` or `Token` (`cartopolis_core::protocol`); there is
no register message, and `simulator::ensure_account` / `--create-admin` are
server-side provisioning on the sim's own database. So the sign-in screen can
only *sign in*. Adding
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
