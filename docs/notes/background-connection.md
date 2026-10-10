---
topic: background-connection
triggers: [background.rs, ConnectionService.java, SetPresence, presence, "foreground service", "push notification", FCM, "Firebase", "notification", POST_NOTIFICATIONS, foregroundServiceType, "background", "battery", "stay connected", quiesce_while_backgrounded, presence_active]
updated: 2026-08-23
---

# Staying reachable with the app off screen

A chat message sent to someone whose phone is in their pocket had nowhere to go:
`ChatHub` fans out to connected sockets and nothing else, so an app that is not
running is an app that hears nothing. This note is the shape of the answer and
the reasons it is not the obvious one.

Two halves, and neither is worth much alone:

- **A foreground service** (`ConnectionService.java`, `systems::background`) so
  Android does not freeze the process.
- **A presence flag on the wire** (`NetMessage::SetPresence`, protocol 18) so a
  held connection is cheap enough to be worth holding.

## Why not FCM

Firebase Cloud Messaging is the standard answer and it is **Google Play services
only**. That is a project constraint — this app is distributed as an APK from its
own site, runs on de-Googled phones, and the user's instruction was explicit — so
it is not a trade-off that was weighed and lost. It is off the table.

What that costs is the thing FCM is genuinely good at: one radio connection per
*device*, shared by every app, rather than one per app. Our foreground service is
one connection for one app, and if that ever proves too expensive the non-Google
answer is **UnifiedPush** (an ntfy distributor holding the single connection),
not FCM. Recorded so the next person does not have to rediscover the option.

## The service holds nothing

`ConnectionService` starts nothing and connects to nothing, and that is the
design rather than an unfinished part of it. The wire already runs on its own OS
thread with its own tokio runtime (`network.rs`), entirely outside Bevy's
schedule — the connection never needed the activity. What it needed was
*permission to keep running*, and a foreground service with an ongoing
notification is the only way to buy that without a push service. So the whole job
is `startForeground`; the notification **is** the feature. Two owners of one
connection is the bug this shape avoids.

Stopping the service therefore does not disconnect anything. It withdraws the
permission to stay alive once the activity is gone.

## Three things that would have been wrong

**Starting it on suspend.** The obvious trigger — `AppLifecycle::WillSuspend` —
is the one that crashes. Since Android 12 a `startForegroundService` from the
background throws `ForegroundServiceStartNotAllowedException`, and WillSuspend is
already too late to count as foreground; the permission prompt could not happen
there either, since a dialog raised while the activity is leaving is a dialog
nobody sees. So the service starts when the **setting** is flipped, with the app
plainly in front, and simply keeps running. It costs nothing while the app is
visible — the process was staying alive anyway.

**`foregroundServiceType="location"`.** LOCATION is the type with no runtime cap,
which makes it the tempting one. It would also be a false declaration: this
service holds a socket. DATA_SYNC is what it does. When the avatar-tracking work
lands and the service really does read the device's position, LOCATION becomes
the honest type and the manifest changes with it. The cap that buys: apps
*targeting* Android 15 get ~6 h of DATA_SYNC per 24 h. `targetSdkVersion` is 34,
so it does not bite yet — and when that number moves the answer is the tracking
work, not a quieter type.

**Defaulting it on.** The feature *is* a permanent notification plus a process
that no longer gets frozen. Neither is something to hand somebody who did not ask
for it, so `AppPrefs::background_connection` defaults to `false` and the switch
says both consequences out loud before the fact.

## The presence flag is the low-power half

Keeping the process alive is not the same as keeping it cheap. A backgrounded
client that still receives a `WorldSnapshot` every tick wakes the radio and the
CPU constantly, and a held connection that costs as much as a visible one is not
a feature. `SetPresence { active: false }` tells the sim to stop sending
snapshots to that connection.

**Only the snapshot.** It is the largest message on the wire and the most
frequent, and it is the one thing a client that draws nothing has no use for.
Chat, voice, prim upserts, capacity — everything else is delivered unchanged,
because being reachable is the entire reason to hold the connection. A filter
that dropped everything would be indistinguishable from hanging up.

Three implementation facts worth keeping:

- The filter is **per-kind, not per-channel**: the same broadcast channel carries
  prim upserts and capacity updates alongside snapshots. `protocol::frame_kind`
  reads the 8-byte header rather than decoding the payload — decoding a whole
  snapshot per connection per tick in order to discard it would cost more than
  sending it.
- The flag **starts `true`**, so a client that never sends the message behaves
  exactly as it always did. That is the compatibility guarantee that let protocol
  18 land without touching any other client, and it is pinned by
  `a_client_that_never_mentions_presence_is_active`.
- Presence is **not rate-limited**, unlike intents and chat. It is an edge — one
  message out, one back — and a dropped "I'm back" is not corrected by the next
  frame: it leaves a foregrounded client with no world state and nothing to make
  it ask again. A client that flapped the flag would only be choosing whether to
  receive its own snapshots.

The client sends this from `quiesce_while_backgrounded` on **every** platform,
not only Android: a desktop window that is not on screen has the same nothing to
draw. Resuming is the same message with `active: true`, and the next tick's
snapshot is the resynchronisation — there is no catch-up and no ack, because a
snapshot is the whole state.

## What it can hold across, and what it cannot

**The connection survives the app being *stopped*. It does not survive the
activity being *destroyed*, and it never could.**

While the app is merely off screen the activity is retained, the process is
retained with it, and the socket in `network.rs`'s own thread never notices —
that is the ordinary case and the one the feature is for. Android may instead
destroy the activity outright: under memory pressure, with the developer option
"Don't keep activities", or when the back gesture writes `AppExit`
(`dismiss.rs::handle_back_gesture`) and the glue answers by calling
`ANativeActivity_finish`. In each of those the *process* used to stay, because
this service was holding it up.

That is worse than losing the connection, and it is what a user reported on
2026-08-30 as "I click the notification and the app stays black".
`ANativeActivity_onCreate` starts a **fresh `android_main` thread for every
activity instance** (android-activity 0.6.1, `native_activity/glue.rs`), and
`bevy::android::ANDROID_APP` is a `OnceLock` that `bevy_winit` clones straight
into its event-loop builder. So the second entry silently kept the *dead* window
handle, built its event loop on an activity that no longer existed, and was
handed no window and no events. Nothing errored. The app was simply black, with
the service still running and the notification still in the shade.

`android_main` therefore ends the process when `run()` returns, and the
once-per-process claim is now enforced rather than assumed
(`crates/cartopolis_android/src/lib.rs`).

**That fix only covered the back gesture, and the 2026-09-28 report ("the app
does not reopen at all") is the rest.** `run()` returns only when Bevy writes an
`AppExit`. On `MainEvent::Destroy` winit 0.30's Android backend does nothing but
`warn!("TODO: forward onDestroy notification to application")`, so an activity
destroyed by anything else — swiped out of recents, reclaimed for memory — leaves
`android_main` polling forever, the process alive, and the guard's `exit(0)` to
fire on the *next* launch: the app opens and closes itself, and the tap after
that works. `MainActivity` (a `NativeActivity` subclass, `java/`) now kills the
process in `onDestroy`, which is the one place that is told. Not run on a device:
this container has no JDK or SDK to build the APK, and the emulator lane is gone.

**And that kill never ran until 2026-10-01**, because it came after
`super.onDestroy()`. `NativeActivity.onDestroy` calls android-activity's
`on_destroy`, which posts `Destroy` and then **blocks the UI thread until
`android_main` has returned** — which, per the paragraph above, it never does. So
the activity's destruction wedged the Java main thread, the foreground service
held the wedged process up, the notification stayed in the shade, and every later
launch went into that process and showed the theme's splash forever (reported
2026-10-01 with a recents card showing exactly that). The kill now comes first.
The consequence to know: destroying the activity ends the process, so it ends the
service and its notification with it — the held connection survives the screen
going off, never a swipe out of recents.

## Why there is often no notification

It shows only when all three hold: the setting is on (off by default), there is
an online session (offline has nothing to hold), and POST_NOTIFICATIONS was
granted. Until 2026-10-01 a fourth, accidental condition applied: a change of
want that arrived while the previous one was still being applied was logged and
dropped, after the caller had already recorded it as applied — and startup is
that case, "off, no session yet" followed by a session restored from a
remembered token.

Signal, for comparison, does what this does on phones without Google Play
services — a foreground service holding its websocket, with a permanent
"background connection" notification — and nothing visible on phones with them,
because FCM wakes it. The difference that matters is not the service but the
activity: Signal's UI is ordinary Java views, so the system can destroy and
recreate its activity inside a live process. Ours cannot (one window handle per
process), which is why ours must die with its activity.

Anything that wants the process to
outlive its activity has to solve the window handle first; until then, a held
connection is a thing that survives a screen going off, not a thing that
survives Android reclaiming the activity.

## What is verified, and what is not

Verified on the API 34 emulator (arm64 APK through `ndk-translation`) and by
`crates/simulator/tests/presence.rs`:

- the service starts, the notification is posted, and the process is no longer a
  cached one after the activity goes away;

That emulator lane is **gone as of 2026-09-02** and none of it can be re-run
there: the arm64 APK aborts in `ndk_translation` before a frame, and an x86_64
build takes the emulator's own Vulkan process down at device creation. The
black-screen fix above is therefore reasoned from android-activity's glue and
bevy's window setup, and has **not** been watched working on a device.
- an idle client is sent no snapshots and still receives chat;
- coming back resumes snapshots;
- a client that never mentions presence is treated as active.

**Not verified, and needing a real device:** actual battery draw, Doze behaviour
over hours, and whether an OEM's own power manager kills the service anyway —
several are notorious for it, and no emulator can tell you. The measurement that
would settle it is a phone left overnight with the setting on and a message sent
to it in the morning.

## Not built

Stored direct messages. There are none: `ChatScope` is `Local` / `Global` (rooms
went with protocol 39), nothing is persisted, and so a message to someone who is offline still
does not exist as a case. Everything above makes a *connected* client reachable
while its screen is off, which is a different thing from delivering to one that
is gone. That is the next piece, and it is a server-side feature with no Android
in it at all.
