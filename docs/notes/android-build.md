---
topic: android-build
triggers: [cartopolis_android, build-apk.sh, AndroidManifest.xml, android_main, NativeActivity, cargo-ndk, deploy-android, apksigner, platform::android, hasCode, classes.dex, JNI, "android", "apk", "phone", "ndk", "intent filter", launch_url]
updated: 2026-08-17
---

# The Android app is a manifest, a `.so` and the assets directory

There is no Gradle project in this repo and no Java or Kotlin source, and that is
a decision rather than an omission. The whole app is:

- `crates/cartopolis_android` — a `cdylib` that owns the `android_main` symbol and
  calls `cartopolis::run()`. Fifteen lines.
- `crates/cartopolis_android/AndroidManifest.xml` — the app definition.
- `crates/cartopolis_android/build-apk.sh` — `cargo ndk` → `aapt2` → `zipalign` →
  `apksigner`, in that order, about eighty lines.
- `assets/`, copied into the APK unchanged.

## Why NativeActivity, and what it costs

`android.app.NativeActivity` ships in the framework, so an APK that uses it needs
**no Java at all** — hence `android:hasCode="false"` in the manifest and no
`classes.dex` in the archive. That is the single fact the whole packaging story
rests on: without it there is a `classes.dex` to build, which means `d8`, which
means resolving the activity class's dependencies, which is what a Gradle project
is for.

GameActivity is where Google points, and where `winit`'s default feature points
(`android-game-activity`; we select `android-native-activity` explicitly in
`crates/cartopolis/Cargo.toml`). Its C++ half is vendored *into* the `.so` by the
`android-activity` crate and costs nothing extra — but its Java half ships as
`androidx.games:games-activity`, an AAR with transitive androidx dependencies.
Resolving those by hand is a Maven client; resolving them with Gradle is a Gradle
project. Rejected for Phase 1, worth revisiting only alongside the item below.

**The price is the system IME, and that price is still being paid.**
NativeActivity has no built-in IME integration: `android-activity` exposes
`show_soft_input()`, but the characters the keyboard produces do not arrive as
text events the way they do under GameActivity. So no egui text field on the
phone can ever receive a character from the platform's own keyboard. Everything
that is a tap, a drag or a pinch works, because the touch bridges
(`docs/notes/touch-input.md`) were already there.

What changed on 2026-08-10 is the *consequence*, not the constraint: the client
draws its own keyboard (`systems::onscreen_keyboard`, see
`docs/notes/touch-input.md`), so the address bar and the geo search are usable
without an IME. The IME fact above is why that system exists and must stay in
the note — nothing in the tree will re-enable the platform keyboard. A real IME
is still the trigger for taking on GameActivity and Gradle, and the bar for that
is now higher than "typing does not work": it is autocorrect, the user's own
layout, non-Latin scripts and dictation.

## What the phone runs

The phone opens on the **offline viewer** — the streamed OSM / 3DBAG / AHN map
and a local avatar — and can sign in to a remote sim over the WebSocket carrier
like any other build; since 2026-08-21 startup also honours the remembered
session, so "stay signed in" works (it did not before — see `parse_cli` below).
What it cannot run is **solo**. Two consequences that look like bugs if you
don't know:

The APK drops the `solo` feature — and with it the in-process
`cartopolis_simulator` and its bundled SQLite — so it is not compiled at all.
Nothing cross-compiles `rusqlite` for Android today, and nobody has tried.

That is expressed as `default-features = false` on
`crates/cartopolis_android/Cargo.toml`'s dependency line, **plus `features =
["audio"]` naming one default back**. Until 2026-08-15 it did not, and the
consequence was a phone with no sound of any kind: no footsteps, no wind, and no
driver turn cues, which is the one part of navigation usable without looking at
the screen. Android needs nothing of the ALSA headers that made `audio` a feature
in the first place — cpal resolves to the platform's AAudio.
[audio-off-platform.md](audio-off-platform.md) has the rest, including why the
same omission silenced the web build.

What that leaves on the phone is a *map viewer*, and it is worth being concrete
about the edges, because two of them are not Android's doing. The camera works
fully — pan, pinch, twist, tilt are all `TouchGestures`, and taps reach picking,
the gizmo and the pins through the emulated mouse (`docs/notes/touch-input.md`).
**The avatar is visible in Follow mode** — measured on a device-shaped emulator
2026-08-15, not inferred: `avatar: meshes drawn … meshes = 10; rendered = 10;
mode = "follow"`, with the character in the captured frame. It was not visible on
any Android build before 2026-08-14, and the cause was neither the port's nor the
renderer's: see the `std::fs` entry under "Things that are not obvious" below.
**But the app does not *open* in Follow** — `free_camera::seed_start_view` seeds
`CameraMode::Orbit`, and on a fresh install there is no saved location to restore
a mode from, so nothing leaves Earth view until the locate button is tapped. The
avatar is drawn the whole time, 1.5 km below the camera and sub-pixel, which is
why "the avatar is not visible" and "the avatar is not rendered" are different
claims and only the instrument below tells them apart. **The avatar walks from the on-screen
stick** (2026-08-14). Until then it could not be moved at all: movement was WASD
and no gesture anywhere in the tree stood in for it, which was the same gap the
web build had on a phone rather than something the port introduced — and it is
fixed in the same place for both, in `systems::map_osd`'s Follow cluster.
**Typing works, but only through a keyboard we draw ourselves** — the platform
IME is unreachable under NativeActivity (the trade above), so
`systems::onscreen_keyboard` stands in for it everywhere: address bar, geo
search, bookmark rename, chat, the AI panel.

**The phone can now hand back what it knows** — ☰ ▸ Share diagnostics…
(`systems::diagnostics`). Everything needed to diagnose a rendering fault is
already logged; on a handset that log is in `logcat`, which needs a cable, a
workstation, `adb` and developer mode, i.e. it is unavailable to exactly the
person holding the failing device. The menu entry re-measures the live facts —
build, adapter, tier, sky, and the avatar's holders/meshes/rendered/position —
and puts them plus this run's log through the same `ACTION_SEND` the crash
reporter uses. It is deliberately *not* inside the developer panel: the person
who needs it is reporting a bug, not inspecting a renderer. The live header is
not a duplicate of the log, either — logd's buffer is a ring and this client is
chatty, so by the time somebody notices, scrolls and taps, the lines that
explain it may already have rolled out.

`lib.rs`'s `parse_cli()` hands Android a fixed configuration — **no flags**. It
cannot call `Cli::parse()`: a NativeActivity process is started by the zygote, so
the `argv` Rust captured belongs to `app_process` and is full of flags clap has
never heard of — and clap's response to an unknown flag is `exit(2)`, i.e. the
app dying on launch with nothing on screen and one line in `logcat`. Until
2026-08-21 the fixed configuration was `--offline` (right for Phase 1, which had
no transport), and that flag is also what skips the remembered-session startup —
so the phone persisted the "stay signed in" token and prefs faithfully and never
consulted them, which read as the checkbox being broken. The tell in diagnosis:
a reopened app made *zero* connection attempts in the sim's log, where even a
stale token shows a rejected handshake. There is no equivalent of the web
build's `?query=string` switch surface yet; anything the phone needs to be told
has to become UI or an env default.

### An emulator answers most Android questions without a device

Everything in this note used to come from someone holding a phone, which made a
hypothesis cost a round trip and left claims like "the avatar is visible" asserted
rather than measured. A **headless emulator settles most of them**, and the part
that makes it worth trusting is that `ndk-translation` on a `google_apis;x86_64`
API 34 image runs the **arm64-v8a APK unmodified** — the artifact under test is the
one that shipped, not an x86_64 rebuild that might differ.

    adb install -r cartopolis-arm64-v8a.apk
    adb shell monkey -p eu.garage44.cartopolis -c android.intent.category.LAUNCHER 1
    adb logcat -d | grep 'avatar:'

It proves asset loading, scene spawn, startup order, camera mode, touch targets and
panics. It proves **nothing about a real Adreno or Mali driver**: it renders on
SwiftShader, so the same rule as the software-renderer shot harness applies — take
geometry, placement and visibility, and leave colour, exposure and anything
driver-shaped to a device (`docs/notes/headless-shots-software-renderer.md`).

Two things to know before reading a run. `adb shell pm clear` first, or
`offer_last_crash` puts the previous run's **share sheet in front of the app** and
every capture is of that sheet rather than the map. And a crash report is offered
only when the last run died *without* a lifecycle event, which is what makes
"Force stop, then reopen" the way to get a log off a real phone with no cable —
swiping the app away is a clean suspend and deletes the breadcrumb.

**And it gives you about nine seconds.** SwiftShader aborts the run with
`egui-wgpu: Tried to update a texture that has not been allocated yet` shortly
after the first frames, which is long enough for startup, asset loading and the
first `avatar:` lines and *not* long enough to tap a control and capture the
result — a launch/tap/screencap sequence loses the race more often than it wins
it. So the emulator answers "what does the app do on its own", and anything that
needs the app driven into a state is still a device question.

### What the emulator could not settle: the avatar on a real handset

Worth recording as a **negative** result, because it is the shape of a whole class
of bug and it is what the diagnostics share below exists for. On 2026-08-16 the
avatar was reported invisible in Follow mode on a Fairphone-class handset running
the *published* APK. Three measurements, and none of them reproduced it:

- the **same published artifact** on the emulator logs `avatar: model requested
  holders = 1` and `avatar: meshes drawn meshes = 10; rendered = 10`;
- `CARTO_QUALITY=phone --shot --shot-mode avatar` on lavapipe renders the
  character, so the phone *budget* — the dome sky, `AcesFitted` in-shader
  tonemapping, `render_scale` 0.5, no bloom, no shadows — is not what hides it;
- on the device, forcing a rebuild from the appearance chooser changes nothing,
  and no figure appears at any follow distance out to `AVATAR_CAM_MAX`.

Which leaves the avatar spawned, unmasked and submitted, and the handset's driver
not drawing it — the avatar being the only **skinned** mesh in the scene, and the
only one loaded from a glTF. That is a claim about a driver, and neither harness
here can see one. It is also, precisely, a fact that only the device holds.

### Running the phone's configuration on a workstation

Worth knowing before the next Android bug is diagnosed by `adb install`, because
the two facts above compose into one nobody had written down: **a default
`cargo run -p cartopolis` does not exercise anything the phone does.** The
desktop default is solo — an in-process simulator, server-authoritative movement
— and the phone has no `solo` compiled in at all, so it takes the offline arm
(`avatar::handle_avatar_movement` moving the transform itself) which a desktop
run never reaches. On top of that `quality::auto()` hands Android
`Budget::phone()` unconditionally, which no desktop resolves to.

Both are reachable locally, and together they are as close to the device as a
machine with a keyboard gets:

```bash
CARTO_QUALITY=phone cargo run -p cartopolis \
    --no-default-features --features audio -- --offline
```

`--no-default-features` is the part that is easy to leave off and the part that
matters most: it is what drops `solo`. `--features audio` is what puts back the
one default the APK keeps — without it this "closest thing to the device" is
silent in a way the device is not, which is a worse lie than either extreme. What this still does *not* reproduce is
the device — the GPU and its driver, the `AssetManager`, the NativeActivity
lifecycle — so it narrows a bug rather than settling one. That is worth a lot on
its own, given the alternative is a thirteen-minute build and an install.

## `hasCode="false"` is a design constraint, not a packaging detail

Reaching a framework service from `android_main` is plain JNI and costs nothing.
Reaching one that wants a **callback** costs the whole Gradle project, because
every push-shaped API in the platform is typed on a Java interface that only a
`classes.dex` can implement. `platform::android` (added 2026-08-10) is the first
place this bit, and the shape generalises:

- `LocationManager.requestLocationUpdates(…, LocationListener, Looper)` needs an
  interface implementation. So does every sibling: `getCurrentLocation` wants a
  `java.util.function.Consumer`, `registerGnssStatusCallback` a
  `GnssStatus.Callback`, `addNmeaListener` an `OnNmeaMessageListener`.
- **`java.lang.reflect.Proxy` is not the escape hatch it looks like.** It
  synthesises an implementation of an interface at runtime and needs an
  `InvocationHandler` — itself an interface. The recursion does not bottom out.
- `InMemoryDexClassLoader` over hand-authored dex bytes would work and is
  rejected on this note's own grounds: it is Java bytecode we author and `d8`
  compiles, wearing a byte array.

The **launch intent** is the second place it bit, and it bit differently — nothing
here needs a callback, and the JNI call (`getIntent().getDataString()`) is one
line. What the no-dex rule costs there is the *trampoline activity* that would
re-deliver a link tapped while the app is already running, because
`NativeActivity` never calls `setIntent` and a replacement for it is a Java class.
So a place link is honoured on a cold start and ignored on a warm one; the whole
reasoning is in [place-links.md](place-links.md).

So location is **pulled, not pushed**: `requestLocationUpdates` is registered
with the `PendingIntent` overload — a broadcast with deliberately no receiver,
whose only job is to make the provider *run* — and the fix is read by polling
`getLastKnownLocation`, which the framework refreshes from any fix the provider
reports, whoever asked for it. Permission likewise goes out through
`Activity.requestPermissions` and comes back through polled
`checkSelfPermission`, because the result callback is unreachable for the same
reason.

**Four claims in that design need a device**, and they are written out in falling
order of confidence in `run_location`'s doc comment — read them there, not here,
because they are what a first `adb install` is for. The load-bearing one: *fixes
caused by our own registration land in the cache we poll*. If AOSP's
`LocationProviderManager` turns out to keep last-location per client rather than
per provider, the poll returns the same position forever and the symptom is an
arrow that never moves. **There is no third dex-free option if it fails** — the
fallback with the best chance is `getCurrentLocation`, which needs a `Consumer`,
i.e. GameActivity and Gradle. That is the standing fact here: this seam has
exactly one dex-free implementation and one fallback, and the fallback is the
move this note already names as expensive.

Two things that were *not* hard, recorded so they are not re-investigated:

- **Keep-screen-on needs no JNI at all.** `AndroidApp::set_window_flags` wraps
  `ANativeActivity_setWindowFlags`, and that NDK function posts to the UI
  thread's looper itself — which is the hop we would otherwise have needed a
  `Runnable` for, and a `Runnable` is an interface.
- **`open_url` is plain framework classes** (`Uri`, `Intent`, `Context`), started
  from the *application* context and not the activity: `android_main`'s thread is
  not the UI thread, `Activity.startActivity` touches the decor view from the
  calling thread, and `ContextImpl.startActivity` is a pure binder call.

The manifest declares FINE **and** COARSE deliberately: since API 31 the
permission dialog only offers an Approximate choice when COARSE is declared, so
asking for FINE alone forces someone who wants to share a rough position to
refuse outright. Background location is not declared and should not be.

## Things that are not obvious and cost time

**`arboard` is not a native-platform dependency, it is a desktop one.** It has X11,
Wayland, AppKit and Win32 backends and no Android one, so it does not build for
`aarch64-linux-android`. It lived under `cfg(not(target_arch = "wasm32"))` in
`crates/cartopolis/Cargo.toml`, which is a gate Android passes. This is the general
shape of the trap: this tree's platform seams were built as *native vs wasm*
(ADR-030/033), and Android is a third thing that answers "native" to every one of
them. Expect the next such dependency to be found the same way — by the build
failing, since nothing else distinguishes them.

**`AssetPlugin { file_path }` is ignored on Android.**
`AssetSource::get_default_reader` returns `AndroidAssetReader` for the target, and
that reader passes paths straight to the framework `AssetManager`, rooted at the
APK's `assets/`. So the repo's `assets/` layout is the device's, unchanged, and
the `file_path` string is dead — kept honest (`"assets"`, not a build-machine
absolute path) only so nobody debugging an asset load is sent to a directory that
does not exist on the phone. There is also no `AssetWriter`: assets are read-only
on the device, which is fine, since nothing writes them.

**…and the corollary is why the avatar was invisible on every Android build ever
made (found 2026-08-14).** Because the asset root is not a directory the process
can stat, **any code that checks for an asset with `std::fs` is asking the wrong
question**, and it does not get an error for its trouble — it gets `false`.
`avatar::avatar_asset_exists` did exactly that, and worse, against a *relative*
`"assets"` rather than the root `AssetPlugin` was given. The two agreed only when
the binary was launched from the repo root. Where they did not, all twenty
catalog styles read as absent, `resolve_avatar_holders` returned an empty vec,
and `spawn_avatar` marked the avatar loaded and spawned no meshes: the entity was
still there, so Follow mode was offered, the camera followed it and it walked —
it was simply never drawn. That was the permanent state of **Android**, of the
**web build** (`std::fs` on wasm32 is stubbed, so `is_file()` is always false),
and of any desktop binary started from another directory. Three things to take
from it:

- The asset root now has exactly one answer, `cartopolis::ASSET_ROOT`, and the
  probe returns `true` on both bundle platforms by construction. A missing file
  there is the asset server's to report, and it names the file — which a boolean
  never could.
- **The failure was silent by design, at every layer.** An empty holder list is a
  legal appearance, `AvatarModelLoaded` is inserted regardless so nothing retries,
  and every user-visible consequence (Follow offered, camera follows, walking
  works, collision blocks) is driven by the `Avatar` entity rather than by its
  model. Nothing was ever going to log. It does now, at the one place that can
  tell the two apart: `avatar: model requested` carries `holders`, and `holders=0`
  is the bug.
- **It was reachable from a unit test the whole time**, and the test now exists
  (`the_default_appearance_resolves_to_a_model`). Cargo runs a test binary from
  the *package* directory, so the old relative probe failed there too — the same
  bug the phone had, on the machine that could see it, going unasserted.

The general shape is the one this section opens with, and this is its sharpest
instance: a seam written as *native vs wasm* where the real distinction is
**filesystem vs bundle**, and Android is on the bundle side while answering
"native" to every `cfg` in the tree.

**The cache root has to be asked for.** A NativeActivity process starts with its
working directory at `/`, and there is no checkout to walk up to, so both of
`utils::cache`'s fallbacks resolve to an unwritable `cache/`. Every write through
`platform::storage` is best-effort, so the failure is *silent*: the app works
perfectly and re-fetches every tile it has ever fetched, forever.
`ANDROID_APP.internal_data_path()` is the only writable answer, and it is also the
one the system clears when the user clears the app's cache — which is the correct
semantics for this tree.

**Stderr on Android goes nowhere, so the logging seam flips.** Nothing forwards a
process's standard streams to logcat unless the app arranges it. This tree
disables Bevy's `LogPlugin` on native and installs its own `tracing_subscriber`
writing to stderr — which on a phone means the app logs nothing at all, on the one
platform where the first run is entirely diagnosis and the log is the only
instrument there is. So Android is on the *wasm* side of that split: `LogPlugin`
stays (its `android_log-sys` layer writes to logcat properly) and the subscriber
does not run. Its default filter is replaced with the desktop one, because
`LogPlugin`'s own default lets everything through at INFO and buries the app's
lines under Bevy's startup and asset chatter. Read it with
`adb logcat -s cartopolis:V bevy_render:V wgpu:V`.

**`--platform` must equal `minSdkVersion`.** The NDK bakes the API level into the
`.so`'s symbol requirements. Build against 34 while the manifest claims 28 and the
APK installs happily on an API 28 device and then fails in the dynamic linker at
launch, complaining about a symbol rather than about a version.

**Sign after aligning, never before.** APK Signature Scheme v2 covers the whole
archive, so `zipalign` on a signed APK invalidates the signature it just moved.

**The shipped `release` profile cannot link this target on a normal box**, which is
why `android-release` exists. Fat LTO over one codegen unit puts the bitcode for
the entire Bevy stack in a single rustc process; on the 22 GB build container that
process was killed by the OOM killer (signal 9) during the `cartopolis_android`
cdylib, *after* an hour spent compiling everything below it — so the failure
arrives at the very end and looks like a mystery rather than a resource limit.
`android-release` is `release` with thin LTO over 16 codegen units, the same trade
`web-release` already makes for the same reason. Measured only as "it links"; the
frame-time claim is inherited from the web profile's note, not re-measured here.

## The CI gate is a check, and why the NDK is in it

`ci.yml`'s `check` job compiles `cartopolis_android` for `aarch64-linux-android`.
This matters more than the equivalent line for wasm does: `cartopolis_android`'s
dependencies are target-gated, so in the plain `cargo check --workspace` the crate
is *empty* — an ordinary workspace build proves nothing at all about it.

That lane needs the NDK, and the wasm lane's `rustup target add` trick is not
enough, because `blake3` (under `bevy_asset`) compiles `blake3_neon.c` for every
little-endian aarch64 target and a build script's `cc` runs at check time. blake3's
`no_neon` feature would remove the requirement — and would also turn off SIMD in
the shipped app, which is the exact trade `crates/cartopolis/src/simd_baseline.rs`
exists to refuse. So the lane downloads and caches an NDK instead. Baking it into
`cartopolis-ci:latest` would be strictly better and is only not done because that
image lives in another repo. Reproducing that lane in the agent container needs
two more steps that CI gets for free —
[container-android-toolchain.md](container-android-toolchain.md).

The APK is a *separate* workflow, not a step here, for deploy-web.yml's reason:
this lane must stay the fast gate on every push, and a link of the whole Bevy
stack against a runner with capacity 3 is not that.

**A cheap gate placed first hides every expensive gate behind it**, and this lane
is where that cost two months of Android releases. `cargo fmt --check` used to be
the *first* step of the `check` job; a failing step ends the job, so one
unformatted merge meant the `cargo check`, wasm and Android lanes below it never
ran — and this is the only lane in CI that compiles the APK's configuration.
Behind the formatting error sat a real compile error, so `deploy-android.yml`
fell over on every push while the fast red X read as "someone forgot rustfmt",
which is exactly what it was and exactly what it was concealing. `fmt` is last
now. The general rule, worth applying to any gate added here: **whatever gates
last cannot hide anything.**

The break it hid is this tree's recurring platform-vs-feature confusion, and it
will come back. The APK builds `--no-default-features`, so no `solo`, so
`SessionMode::Solo` and `SessionStatus::Starting` are never constructed — and
`[workspace.lints.rust] unused = "deny"` makes dead code a hard error. Both
carried `cfg_attr(target_arch = "wasm32", allow(dead_code))`: a *platform* gate
standing in for a *feature* condition, which Android passes and therefore fails
on. Gate on `not(feature = "solo")`, which is the condition that is actually
true and covers wasm as a side effect.

## Publishing: one APK, no history, and why the key lives on the host

`deploy-android.yml` builds `arm64-v8a` on every push to `main` and writes the
result to `/data/cartopolis/site/apk/`, i.e. **inside the wasm client's web root**,
which is what buys the whole serving story for free: Caddy already has that
directory mounted and `file_server`'d, so there is no second site block, no DNS
record and no compose edit. The phone install path is
`https://cartopolis.org/apk/`. The cost is a coupling that is invisible from
either file alone — `deploy-web.yml`'s `rsync --delete` would remove `apk/` on the
next web deploy, so it carries an `--exclude '/apk/'`.

The filenames are **fixed** (`cartopolis.apk`, its `.sha256`, an `index.html`), so a
build overwrites its predecessor and the directory stays at one APK. That is the
whole retention policy, and it is deliberate: nobody installs last week's build,
and the runner host's disk is 76% full with a 24 GB Actions cache on it.

**The signing certificate is the thing that has to be stable**, not secret.
Android keys an installed app to the certificate that signed it and refuses an
update signed by another one (`INSTALL_FAILED_UPDATE_INCOMPATIBLE`, whose only
cure is uninstalling first). A key generated per CI run — which is what
`build-apk.sh`'s built-in debug keystore would be in a fresh container — therefore
turns every deploy into an uninstall/reinstall. So the key is generated once and
kept at `/data/cartopolis/android-key/` on the deploy host, mounted read-only into
the job as `/signing`, and named in the runner's `valid_volumes` alongside the web
root. It is an ordinary self-signed identity: this is not a Play upload key and
gains nothing from a passphrase that would have to sit in the same directory to be
usable.

It is a `.pk8` + `.x509.pem` pair rather than a Java keystore because the host has
no JDK and no other reason to grow one — `apksigner --key/--cert` takes exactly
what `openssl` can produce, while `keytool` cannot be asked for that pair. To
regenerate (and accept that every phone must uninstall once):

```bash
openssl req -x509 -newkey rsa:2048 -sha256 -days 10950 -nodes \
    -subj "/CN=Cartopolis/O=garage44/C=NL" -keyout /tmp/k.pem -out cartopolis.x509.pem
openssl pkcs8 -topk8 -inform PEM -outform DER -in /tmp/k.pem -out cartopolis.pk8 -nocrypt
```

**The `.so` is stripped into the APK, and that is the one step that can silently
break the app.** `build-apk.sh` runs `llvm-strip --strip-unneeded` on the *copy*
it packages (never on cargo's artifact, which would make the next incremental
build's freshness check lie). `android_main` survives because it is a dynamic
symbol — but nothing in Rust references it, so a future change here would produce
an APK that builds, signs and installs and then does not start, which is why the
deploy job re-asserts the symbol out of the packaged archive rather than trusting
the build.

The job installs what the CI image lacks and nothing more: a **JRE** (`apksigner`
is a shell script around `java`; no JDK is needed because nothing compiles Java),
`zip`, `cargo-ndk` — needed here and not in the check lane because this one
*links* — and the SDK's `aapt2`/`zipalign`/`apksigner`/`android.jar` as two direct
zips from Google. Those are deliberately **not** cached: 124 MB fetched in seconds
against 124 MB living on the runner host indefinitely. The NDK *is* cached, under
the same key the check lane uses, so on `main` it is a restore rather than a
second download.

## Verification status, 2026-08-10 (superseded — see the hardware section below)

Built and packaged in the headless container; **never run on a device or an
emulator**. The container has no GPU, no KVM and no Android runtime.

**The APK has never once published.** `deploy-android.yml` failed on every push
to `main` from the day it landed, and `https://cartopolis.org/apk/` was a
404 for the whole of that period — for the CI-ordering reason above, with a real
compile break behind it. `cargo check -p cartopolis_android --target
aarch64-linux-android` passed for the first time in this repo's history on
2026-08-10. So the packaging facts below, which predate that, were measured on a
one-off manual build and not on anything CI ever reproduced.

What is actually proven, and it is worth knowing exactly how far it goes:

- `arm64-v8a`, `android-release`, 28 min from cold. APK 46 MB; the `.so` is
  152 MB uncompressed, being unstripped — measured *before* the packaging strip
  landed with `deploy-android.yml`, so both numbers are now upper bounds.
- `apksigner verify` passes (v3 scheme).
- The archive holds `AndroidManifest.xml`, `resources.arsc`, `res/`,
  `lib/arm64-v8a/libcartopolis_android.so` and all 35 files of `assets/` — and
  **no `classes.dex`**, which is the check that `hasCode="false"` is telling the
  truth.
- `android_main` is present in the `.so`'s dynamic symbol table as a `GLOBAL
  FUNC`. This is the one worth re-running after any change to the crate graph:
  the symbol is looked up by name from C, nothing in Rust references it, and if
  the linker ever drops it the APK still builds and signs and simply fails to
  start.
- Its only `DT_NEEDED` entries are `libdl`, `liblog`, `libandroid`, `libm`,
  `libc` — all on every device. Nothing needs `libc++_shared.so`, so the APK does
  not ship one; if a future dependency pulls in C++, that changes and the missing
  library is a load-time failure, not a build one.

Everything past "does it load" is open, and the first `adb install` answers it:
whether Bevy acquires a Vulkan surface and survives pause/resume, whether the
touch bridges behave as they do in a browser, whether the drawn keyboard is
reachable with a thumb, whether the location poll ever moves, and whether 63 MB
of glTF loads through the `AssetManager` in a tolerable time. Treat a first run
as diagnosis and watch `adb logcat -s cartopolis:V bevy_render:V wgpu:V`.

The 2026-08-10 pass added a good deal of Android-only behaviour on top of this —
a location provider, an on-screen keyboard, `AppLifecycle::WillSuspend` flushes,
a phone render budget, an app-data root for `user.json`. Every one of them is
compiled and unit-tested and **none of them has run on hardware**. Do not read
"it checks" as more than it is; this note's whole point is that the gap between
compiling for `aarch64-linux-android` and running on a phone is where all the
remaining risk lives.

## First hardware run, 2026-08-12: it installs, it starts, it is killed

Build `1.0.0+6e80d5dc` (the CI APK off `main`) was installed on a real phone and
**died before drawing a frame**. That is a worse outcome than "it works" and a much
better one than the previous state of this note, because of what it settles.

The evidence is the crash reporter's own, and reading it is the point of this
section. The next launch produced the *breadcrumb* message — no panic report — so:

- **Everything up to `run()` works on hardware.** The breadcrumb is written at
  `lib.rs`'s `run()`, after the panic hook and after `offer_last_crash`. Finding
  it means the APK installed, `System.loadLibrary` found the stripped `.so`,
  `android_main` resolved, `ANDROID_APP` was set and
  `internal_data_path()` was writable. Every packaging and load-time risk this
  note spends its length on is now *measured*, not argued.
- **The crash reporter itself works**, including the JNI share sheet from a
  process with no window — the one part of the 2026-08-10 batch that had to work
  before any of the rest could be diagnosed. It is worth keeping that dependency
  in mind: it is the instrument, so a change that breaks it blinds everything.
- **Nothing in our Rust panicked.** The hook is global and the profile aborts, so
  a panic on any thread would have left the file. The process was killed.
- **It was not a clean background**, or `WillSuspend` would have removed the
  breadcrumb. It died on screen.

What that leaves is the renderer bring-up: the wgpu adapter, the Vulkan surface, or
the low-memory killer during the 63 MB of glTF. **Those three are indistinguishable
from the report as it stood**, which is what the same-day follow-up fixed:

- The Android `LogPlugin` filter carried the desktop's `bevy_render=error`, so the
  adapter and backend the app actually chose were never logged — on the one
  platform where nobody can attach a debugger. `bevy_render` and `bevy_winit` run
  at INFO here now. **The filter is the crash report's contents**, not just a
  console preference.
- `platform::android::own_logcat` reads the ring buffer back on the next launch
  and the report carries the tail. logd filters by UID, so this needs no
  `READ_LOGS` and returns only our own lines — which also means it can show a
  native tombstone (`crash_dump` reports under the crashing UID) but **never** an
  OOM kill, announced by the system UID. Evidence for the LMK is therefore
  negative: our log stops with no `signal` line.

## The crash, named: `FLOAT32_FILTERABLE`

The follow-up log answered it in one round trip, and the answer was none of the
three suspects above:

```
In Device::create_bind_group, label = 'transmittance_lut_bind_group'
  Texture binding 5 expects sample type Float { filterable: true },
  but was given a view with format Rgba32Float (sample type Float { filterable: false })
… and the same for multiscattering_lut, sky_view_lut, render_sky
ERROR bevy_render::error_handler: Quitting the application due to Validation RenderError
F libc: FORTIFY: pthread_mutex_lock called on a destroyed mutex
```

bevy_pbr's atmosphere keeps its four LUTs in `Rgba32Float` and samples them with a
filtering sampler. wgpu allows that only when the adapter offers
**`FLOAT32_FILTERABLE`**, and the Adreno 619 does not — as many mobile GPUs do
not. Every LUT bind group fails validation, and Bevy's default error handler
answers a validation error by writing `AppExit::error()`. The app was therefore
*quitting itself*, correctly, three seconds in.

Three things are worth keeping from this:

**The gate is on the adapter, not on Android** (`free_camera::setup_camera`).
Whether this device can filter a 32-bit float texture is a property of the device
this run found, not of a class of machine — the same argument the MSAA clamp a few
lines below already makes, and the same trap this note's platform-seams section
describes. A Mali that supports the feature keeps its sky; a desktop GPU that
lacks one still starts.

**A render error no longer ends the app.** The default handler is why the failure
presented as a kill: the process aborted inside libc on the way out, leaving no
Rust panic, no exit message, and a crash report that could only say the process
died. `RenderErrorPolicy::StopRendering` keeps it alive, so the log keeps flowing
and the next unknown error costs a look rather than another CI build and install.
Not `Ignore` — that resumes rendering into an error that recurs every frame, and
bevy's own warning on it is rapid flashing.

**The instrument paid for itself immediately.** The logcat tail in the crash
report is what produced the block above; without it this was three indistinguishable
suspects and a 30-minute loop to tell them apart. Note also what nearly hid the
answer: the Android log filter had inherited the desktop's `bevy_render=error`,
which would have suppressed the `AdapterInfo` line — and it was `bevy_render` at
ERROR that carried the validation failure, so that one lane was doing both jobs.

Still unknown, in the order a device answers them: whether the surface survives the
first resume, whether the asset load fits in memory, and what the sky looks like on
a phone that has no atmosphere to draw. Nothing below the renderer is in doubt any
more.

## The first freeze: it happens when driver mode asks for GPS (2026-08-13)

Reported from the device: tapping **Drive** hangs the app, and the system's
location indicator comes on. Unresolved — no log yet — but the shape of it is
worth writing down, because the obvious reading and the likely one are different.

The obvious reading is that the location code hangs. It is the one thing that is
new on that tap, and it does the only two questionable things in this file: it
runs `Activity.requestPermissions` from a thread the framework did not intend
(see the note on that function), and it polls `getLastKnownLocation` at 1 Hz
across JNI. Neither can block the render thread: the worker is a detached thread
of our own, and everything it shares with Bevy is a `Mutex<Option<…>>` held for
the length of a move.

The likely reading is the one the section above already listed as the next
unknown: **whether the surface survives the first resume.** A runtime permission
dialog is another activity on top of ours — the activity pauses, the native
window is destroyed and (on dismissal) recreated. Nothing in this app's life
does that until the moment the user taps Drive, because it is the only runtime
permission the client asks for. So "it freezes when GPS starts" and "it freezes
the first time it is backgrounded" are the *same event* on this build, and they
are indistinguishable from the outside.

Two log lines now separate them, both `INFO`, both in the tail the crash reporter
shares on the next launch:

- `asking for the location permission — the system dialog is about to appear`
  (`platform::android::settle_permission`), plus `location permission granted`
  with the wait, `location worker starting`, the provider list, and `first
  location fix` with its accuracy and age.
- `app lifecycle` for every `AppLifecycle` Bevy emits (`quiesce_while_backgrounded`).

The reading is then mechanical. `WillSuspend`/`Suspended` with no `WillResume`
after it is a lifecycle failure and has nothing to do with location — take it to
the surface-recreation path and reproduce it with the Home button, which needs no
permission and no route. Lifecycle events that come back cleanly while the
location lines stop, or never appear, points back here. And if the GPS indicator
is on but no `first location fix` line ever lands, that is the per-provider cache
inference in `run_location` failing, which is the fallback this file names.

**Repro instructions for whoever has the device**: reproduce the hang, force-stop
the app, then open it again — the previous run's report plus the log tail goes to
the share sheet on launch (`offer_last_crash`). The Home-button test above is the
cheaper half and worth doing first.

## The freeze, half-answered: a halted renderer *is* a hang (2026-08-14)

Still no device log, so the section above stands as written. What changed is that
one reading of it was chased down in the source and turned out to be a mechanism
rather than a hypothesis, plus two things on the path to it that were wrong on
their own merits. All three are fixed; none of them has run on hardware, and if
the freeze survives them the section above still says what log lines to read.

**`RenderErrorPolicy::StopRendering` is terminal, and terminal looks exactly like
a hang.** The 2026-08-12 pass installed it to stop the app *vanishing* on a render
error, and it did that. What was not noticed is that bevy leaves the render state
`Errored` for the rest of the process: nothing is drawn again, ever, while the
window, the input and every system keep running. From outside the phone that is a
still photograph of the last good frame which answers no touch — "it froze and I
had to restart it", indistinguishable from a deadlock and with the explanation
only in `logcat`, i.e. only reachable with the cable this whole note exists to do
without. A pause/resume is the ordinary way for an Android app to lose its
surface, and a system dialog is the ordinary way to be paused, so *any* GPU error
that a dialog provoked cost the entire session.

`lib.rs::handle_render_error` answered by kind for a fortnight: `DeviceLost` — the
error wgpu delivers through its own device-lost callback, and the one that is
plausibly transient — rebuilt the renderer through `RenderCreation::Automatic`,
twice at most, while `Validation` and `OutOfMemory` halted (they recur the instant
rendering resumes, and `Ignore` on a handheld screen is a strobe).

**That recovery was removed on 2026-08-18, because it never once worked and it
aborted the process both times it ran.** See "The recovery that could not be
written" below. Every kind halts now. The trade is stated rather than hidden: a
transient device loss costs the session instead of being ridden out — but it costs
a *restart*, where recovering cost a *crash*.

A halt is **written to `crash_report_path()`**, so the next launch hands it to the
share sheet exactly as a panic — which is the whole point, and is how
`FLOAT32_FILTERABLE` was named in one round trip. It is also logged once rather
than once per frame; bevy calls the handler on every extract for as long as the
state stays `Errored`.

**The first GPS fix used to warp the avatar across a stale map anchor.**
`driver::place_avatar` writes flat-frame metres measured from the *map anchor*,
and nothing in that path moves the anchor. The fix a receiver reports knows
nothing about where the app happens to be looking, so the everyday case — plan a
route in the city you were browsing, tap Drive, phone reports the town you are
actually in — dropped the avatar, and through the chase rig the camera,
potentially hundreds of kilometres from the origin of a frame the tiles,
buildings, terrain and route line were all still built around. Every streamer
then rebuilds at once, in one frame, on a phone GPU: the mass mesh/texture churn
this project already documents as the way to lose the device, after which the
paragraph above applies. `driver::GPS_TELEPORT_M` decides: past 2 km the fix is
applied as a `Teleport`, which re-anchors the map, retires the old tiles through
the bounded despawn queue and brings the camera with it; under it, the cheap
position write it always was. Note this is also a *correctness* fix independent
of the freeze — the avatar now ends up on a map of where the device is.

**Two threads were writing the activity's `Window` at the same instant.**
`platform::android`'s own `open_url` documents that `Activity.startActivity` from
a non-UI thread reaches `cancelInputsAndStartExitTransition`, which reads the
decor view, and sidesteps it via the application context.
`Activity.requestPermissions` reaches the same code through
`startActivityForResult` and has no such escape — there is no dex-free way to post
a `Runnable` to the UI thread, which is the constraint this whole file is shaped
by. What was avoidable is the *other* writer: driver mode turns on and asks for
the permission on one frame, and `apply_screen_wakelock` answers the same frame by
setting `FLAG_KEEP_SCREEN_ON`, which the framework performs on the UI thread, on
that same window, while the framework is also tearing its input channel down for
the incoming dialog. `PERMISSION_DIALOG_UP` now defers the wakelock until the
dialog is gone. A flag on a window can wait a second; three writers on one window
is not a race worth having.

**What a device still has to settle**, unchanged in priority: whether the surface
survives the first resume (the Home-button test needs no permission and no
route), and whether `getLastKnownLocation` ever moves. If the freeze persists,
the new evidence is a *share sheet on the next launch* naming the render error —
and if there is no share sheet and no crash report, the renderer never errored
and the hang is above it.

## The freeze, on hardware at last: it is an ANR, and it is not the permission dialog (2026-08-14, second run)

Build `1.0.0+90e41a75` produced a real log, and it settles more by elimination
than by naming a culprit. The whole sequence around the tap:

```
37.752  driver mode on gps=true
37.752  location worker starting
37.753  location permission already granted
37.763  android location providers providers=["fused", "gps", "network"]
37.767  teleported to geo target lat=53.2058 lng=6.5642
37.767  map re-anchored / tile fetches queued queued=8 finest_zoom=17
37.773  globe placed radius=6364465
37.783  W System.err: java.lang.IllegalArgumentException: pending intent must be mutable
37.783  requestLocationUpdates failed — falling back to polling the cached fix only
        … silence …
44.223  Signal Catcher: reacting to signal 3 / Wrote stack traces to tombstoned
51.124  (a fresh process)
```

**`signal 3` is `SIGQUIT`, which is how `ActivityManager` announces an ANR.** So
the app was not deadlocked in the sense of a native crash and was not killed for
memory: the system decided it had stopped responding, dumped its threads to
tombstoned and took it. Six and a half seconds of silence before that, in a log
that had been emitting `tile fill` every two seconds, is the main loop stopping.

Four readings die on this log, and that is most of its value:

- **It is not the permission dialog, and not a lifecycle failure.** `location
  permission already granted` — this run never raised a dialog, was never paused
  and never resumed. The entire 2026-08-13 hypothesis, and the `PERMISSION_DIALOG_UP`
  work that followed from it, are beside the point for *this* freeze. (Both stay:
  the reasoning about two threads on one `Window` is still right, it is simply not
  what happened here.)
- **It is not the renderer halting.** No `render error` line, so
  `handle_render_error` never fired and `StopRendering` never latched. That fix
  removes a real terminal state; it did not remove this one.
- **It is not the GPS position path.** The freeze is at 37.78 and the first
  `getLastKnownLocation` poll is a second later. `teleported to geo target` is
  driver mode's ordinary route-start snap, which has always been a teleport.
- **It is not the location worker blocking the main loop.** The worker logged its
  own Java exception at 37.783, i.e. *after* the main thread had already gone
  quiet, and everything it shares with Bevy is a mutex held for the length of a
  move.

**What the log did name outright: `FLAG_IMMUTABLE` was wrong.**
`LocationManager.requestLocationUpdates` asserts `pendingIntent.isMutable()` and
throws, so the registration never happened, the provider was never started, and
the poll read a cache nothing was refreshing — an arrow that would never have
appeared however long you waited. The 2026-08-10 note called this out as the
residual risk and said which constant to flip; it is flipped
(`PENDING_INTENT_FLAG_MUTABLE`). This is the second time the shared logcat tail
has converted an argument into a measurement in one round trip.

**What is still unnamed is where the main loop stopped** *(answered one round
later — it was the screen wakelock, third of the three below; see "The freeze,
named" at the end of this note)*, and the log physically
cannot say: the last line before the silence is whichever system logged most
recently (`globe placed`), which is not where it stopped. Three things ran on that
frame — driver mode's entry, the teleport's re-anchor with everything it makes
rebuild, and the screen wakelock's JNI call — and a truncated log cannot choose
between them.

So the next round is instrumented rather than argued:

- **`platform::watchdog`** is a thread that watches a per-frame heartbeat and, at
  2.5 s of silence — inside Android's five-second ANR window, so the report is
  written *before* the kill — logs the stall and then one line per thread out of
  `/proc/self/task`: name, scheduler state, and `wchan`. A process may read its own
  `/proc` with no permission and no root, which is what makes this reachable when
  the ANR trace (tombstoned), the `ActivityManager` announcement (system UID) and
  a debugger (a cable) are not. A main thread in `R` for the whole stall is one
  enormous frame; parked in a futex is a deadlock and the other rows say behind
  what; parked in binder is a framework call that never came back.
- **`set_keep_screen_on` now logs on both sides of the call.** Its single log line
  came *after* `ANativeActivity_setWindowFlags`, and the absence of that line in
  this log is equally consistent with "the system never ran" and "the call never
  returned" — different bugs, no way to choose. `keep-screen-on: asking` without
  `keep-screen-on: done` names it in one line.

The NDK documents `ANativeActivity_setWindowFlags` as callable from any thread
(it writes a work item the UI thread's looper performs), so the expectation is
that both lines always appear together; the bracket is there to notice the day
that stops being true.

## Why that log could not name the cause, and what makes the next one able to

Worth writing down, because "we had a full log and still could not say" is the
kind of conclusion that gets re-litigated. Two separate reasons, and both are
fixed rather than argued around.

**A stalled main loop cannot log where it stalled.** The last line before the
silence is whichever system logged most recently, which is a different thing and
was in this case `globe placed` — a system that finished. Nothing in the process
was going to write the interesting line, because the thread that would write it
was the thread that had stopped. This is not a gap in the log filter; it is the
shape of the failure.

**And the excerpt only reached back fifteen seconds.** `LOGCAT_TAIL_LINES` was
600, sized against the binder limit the share `Intent` crosses — correct
reasoning about the wrong quantity. What a report needs is not *n lines*, it is
*enough time*, and this client emits two messages **per frame**:

- `could not set cursor position` (`bevy_winit`, ERROR). `bevy_winit::system`
  pushes `Window`'s cursor position into winit whenever it changes and winit's
  Android backend cannot set one; `systems::touch` changes it on every frame a
  finger moves, deliberately (`docs/notes/touch-input.md` — the world-click
  consumers read the `Window`). So the platform answers every real touch move
  with an `error!`, and there is no filter level that keeps `bevy_winit` at INFO
  for surface diagnostics while dropping this.
- `Missing texture: Managed(2)` (`egui_wgpu`, WARN). Undiagnosed. It has been
  firing continuously since the first run that drew anything, so it is neither
  new nor the freeze — but it is a real bug and it is still open.

Between them they were ~95% of a 600-line tail. The fix is on the *report*, not
on the log: ask logd for 8,000 lines, drop the two known repeats
(`LOG_NOISE` / `strip_log_noise`), and let the existing byte cap do the capping.
The count of removed lines is printed, so nobody is quietly handed a filtered
log.

**The bigger miss: the framework had already written the answer.** This note says
twice that an app cannot learn why it was killed — the LMK announces itself under
the system UID, the ANR trace goes to tombstoned. That is true of `logcat` and it
is **not true of `ActivityManager`**. Since API 30
`getHistoricalProcessExitReasons` hands an app its own recent deaths with a reason
code each, and for `REASON_ANR` `ApplicationExitInfo.getTraceInputStream()`
returns *the very trace the system wrote to tombstoned* — every thread, Java and
native frames — with no permission, no root and no cable. Framework classes only,
so the no-dex rule never threatened it.

`platform::android::recent_exit_reasons` reads both, and `offer_last_crash` puts
them **above** the log excerpt because they answer a bigger question than the log
can: ANR, native crash, low memory and "the user swiped it away" are four
different reason codes and were previously one silence. The `main` thread is the
first in an ANR dump, which is why the trace is capped from the head.

`platform::watchdog` stays. It reports a stall *live*, before the kill, and it
works on a stall the system never escalates to an ANR — but where the two overlap,
the system's own dump is strictly better evidence than a `/proc` state column.

## Every log line was going to logcat twice, and the second copy went through a pipe

Found by reading the 2026-08-14 log rather than by adding anything to it, which is
the lesson as much as the fix. Each event appears in that log as a clean `event`
line **and** as an ANSI-escaped `RustStdoutStderr` duplicate:

```
E event …/bevy_winit-0.19.0/src/system.could not set cursor position: …
I RustStdoutStderr: [2m2026-08-14T14:07:18.665011Z[0m [31mERROR[0m …
```

`LogPlugin` adds bevy's `AndroidLayer` *in addition to* a
`tracing_subscriber::fmt` layer writing to stderr. The trap is the field: the
default is `fmt_layer: |_| None`, and `None` means "use the stderr one", not "use
none" (`bevy_log`'s `unwrap_or_else` supplies it). Android captures a
NativeActivity's standard streams into logcat, so the second copy reappears
under `RustStdoutStderr`, wearing the terminal colour codes that a share sheet
has no way to interpret. This note's own claim that our `tracing_subscriber`
"does not run" on Android was true and was read as "nothing writes to stderr
here", which was not.

Three costs, and the third is the one that matters:

- **Twice the logcat traffic and twice the formatting**, on the device with the
  least of both to spare.
- **A crash report interleaved with `[2m[31m`**, for a reader holding a phone.
- **A pipe.** stderr here is a kernel pipe with a fixed buffer, drained by a pump
  thread. A writer that outruns the pump does not drop — it **blocks in `write`**,
  and the writer is whichever thread logged, which is usually the one running the
  schedule. A burst is exactly what a re-anchor produces, and a blocked schedule
  for five seconds is an ANR. This is not proof that it is *the* freeze — nothing
  here is proof without the thread dump — but it is a mechanism that fits the
  evidence exactly: total silence from every thread at once, starting on a burst,
  ending in `SIGQUIT`.

Fixed by giving `fmt_layer` a layer that writes to `std::io::sink`. `AndroidLayer`
is the one that reaches logcat properly and the one `own_logcat` reads back, so
nothing is lost.

**What is deliberately *not* fixed**: `bevy_winit::system`'s
`could not set cursor position`, which is an `error!` per touch-move frame and
cannot be filtered by level. Turning that module off would also lose
`Creating new window`, which is the line that says the surface came back on
resume — a diagnostic this note has been waiting on for three rounds. It stays
noisy in the log and is dropped from the *report* (`LOG_NOISE`), which is the
right place for that trade. `egui_wgpu`'s `Missing texture: Managed(2)` is
likewise still open: the delta ordering in `egui_manager::egui_pass` is correct
(set → upload → render → free, all gated on the generation actually changing), so
the cause is upstream of it — most likely a `textures_delta` produced by a
main-world frame that the render world never applied, since `end_egui_frame`
*replaces* the delta rather than accumulating it and egui announces a texture
exactly once. Not fixed blind.

## The freeze, named: `AndroidApp::set_window_flags` deadlocks against the frame it is called in (2026-08-14, third run)

Build `1.0.0+e37999e5` closed it, and the instrument from the previous round is
what closed it. The log carries

```
19:33:22.648  driver mode on gps=true
19:33:22.696  keep-screen-on: asking on=true      ← thread 7719, a Compute Task Pool worker
              … no `keep-screen-on: done`, and no further line from any pool thread …
19:33:25.640  MAIN LOOP STALLED — no frame for 2966 ms
              ANR: Input dispatching timed out … Waited 5000ms for MotionEvent
```

`asking` without `done` was the whole point of the bracket: **the call never
returned.** The system's own thread dump says where — the task-pool thread is
parked on a futex with every frame inside `libcartopolis_android.so` and **no
`libandroid.so` frame at all**, so it never reached the NDK. It is blocked in a
Rust lock, before the syscall.

**The lock is android-activity's, and it is a write lock taken under a read lock
held by the same frame.** `AndroidApp` is an `Arc<RwLock<AndroidAppInner>>`.
`set_window_flags` is one of exactly three methods that take `inner.write()` (the
others are `enable_motion_axis` / `disable_motion_axis`, which nothing here
calls). And `AndroidApp::poll_events` takes `inner.read()` **for the duration of
its callback** — which on this stack is winit's `single_iteration`, which invokes
the Bevy handler, which is `App::update()`, which is the entire schedule:

```
android_main ──read()──▶ poll_events ──▶ winit ──▶ App::update ──┐
       ▲                                                         │
       └─ cannot return, so the read guard cannot drop            ▼
                       task-pool thread ──write()──▶ set_window_flags (blocks forever)
```

`apply_screen_wakelock` is an ordinary Bevy system, so it runs *inside* that read
guard on whichever thread the parallel executor picked. It waits for a writer's
turn that arrives only when the frame ends, and the frame ends only when it
returns. The schedule stops, input is never drained, and five seconds later the
system posts an ANR. That is the freeze, and it was never about GPS — driver mode
merely happens to be what turns the wakelock on. The same tap with
`keep_screen_on` off would not have frozen, and any *other* system calling this
would freeze without GPS anywhere near it.

**The generalisation, which is the reason this is a note and not a commit
message: never call an android-activity method that takes `inner.write()` from a
Bevy system.** There are three of them and they are not marked; the crate's docs
discuss thread-safety in terms of the *NDK* calls underneath, which really are
callable from any thread. `ANativeActivity_setWindowFlags` writes a work item to
the activity's main-work pipe and the UI thread's looper performs it — thread-safe
by construction, exactly as this note claimed. The wrapper adds a lock the wrapped
call never needed, and inherits none of its safety.

The fix is a dedicated `carto winflags` thread fed by an `mpsc` channel: it is
free to block until the frame that asked has ended, which is the one thing a
system cannot do. Applied in queue order, so a fast off/on cannot land as on/off.
Two prices, both small and both real: the flag lands a frame late (against a
screen timeout measured in tens of seconds), and while a writer is queued
`RwLock`'s writer preference blocks *readers* — `vm()` and `activity()` take
`inner.read()` — so a JNI call on the location thread can wait out the rest of a
frame. Bounded, and off the schedule.

**Why not call the NDK function directly and skip the lock entirely:**
android-activity 0.6 keeps `native_activity()` `pub(crate)` and exposes no raw
`ANativeActivity*` — `activity_as_ptr()` is the Java `Activity` jobject, which is
the wrong pointer. There is no way to reach the thread-safe call underneath.

Three rounds of instrument earned this: the watchdog said the schedule stopped
rather than the renderer, the ANR reader said which thread and that it was a futex
in our own code, and the bracketed pair said which call. None of the three could
have named it alone.

## The same freeze again, on the way *out* of driver mode — the dedicated thread was a race, not a fix (2026-08-15)

Build `1.0.0+f09669b8`. Driver mode and GPS worked; leaving driver mode froze the
app. The log is the previous section's log with the sign flipped:

```
10:26:51.034  driver mode off
10:26:51.099  keep-screen-on: asking on=false     ← thread 12812, `carto winflags`
10:26:51.103  map LOD zoom changed …              ← thread 12727, android_main, mid-frame
              … no `keep-screen-on: done` …
10:26:54.086  MAIN LOOP STALLED — no frame for 2833 ms
              12812 carto winflags  state=S  wchan=futex_wait_queue_me
              12727 android_main    state=S  wchan=futex_wait_queue_me
              ANR: Input dispatching timed out … Waited 5002ms for MotionEvent
```

Note what moved: the blocked writer is no longer a Bevy pool thread, it is the
dedicated one, exactly where the previous fix put it. **Moving the call off the
frame's thread did not break the cycle, it added a third party to it.**

The sentence that names it is in the previous section, written as a *bounded cost*:

> while a writer is queued `RwLock`'s writer preference blocks *readers*

That is true of every reader, and the reader it matters for is **`android_main`
itself**. `poll_events` holds `inner.read()` across its callback, and inside that
callback winit's `single_iteration` calls `input_events_iter()` and
`native_window()` — both of which take `inner.read()` **again, on the same
thread**. `std::sync::RwLock` parks a reader behind a waiting writer so writers
cannot starve, so a re-entrant read is a deadlock the moment a writer is queued:

```
android_main ─read()─▶ poll_events ─▶ winit ─▶ Bevy update ─┐
       ▲                                                    │ native_window()
       │                                       ─read()─ parked behind the writer
       └─ outer guard never drops                           ▼
           carto winflags ─write()─▶ set_window_flags ─ parked on the outer guard
```

Three parties, one cycle, and it needs no bad luck beyond the write landing while
a frame is running — which is most of the time, because this client renders
continuously. **Entering driver mode won that race and leaving it lost.** The
23 ms between `asking` and `done` on the way in (`10:25:22.710` → `.733`) was the
writer catching the gap between two `poll_events` calls; there is no such gap to
catch reliably.

**There is no third place to put the call.** Every point reachable from Bevy is
inside that read guard. android-activity 0.6 offers no `try_` variant that would
let a writer decline to queue, and — as the previous section established — no raw
`ANativeActivity*` to skip the wrapper with. The window flag is unreachable from
this process, full stop.

So `set_keep_screen_on` is a **`PowerManager.WakeLock`** now
(`SCREEN_BRIGHT_WAKE_LOCK`, uncounted, tag `cartopolis:driving`, plus
`android.permission.WAKE_LOCK` in the manifest). `acquire`/`release` are ordinary
binder calls: safe from any thread, no UI-thread hop and therefore no `Runnable`,
and — the point — they never touch `AndroidApp::inner`.

**The property that makes this a fix and not a fourth round: with the window-flag
call gone, nothing in the process takes `inner.write()` at all.** It was the only
caller; winit, Bevy and the rest of `platform::android` take nothing but `read()`.
An `RwLock` with no writers cannot park a reader, so the re-entrant reads winit
performs every frame — and the `vm()` / `activity()` reads every JNI call in this
file makes from a Bevy system — are unconditionally safe again. That is a
structural claim about the whole process, and it is worth re-checking with
`grep -rn 'set_window_flags\|motion_axis'` before adding any android-activity call.

**The generalisation from the previous section stands and should be read one notch
stronger.** It said: never call an android-activity `inner.write()` method from a
Bevy system. The correct rule is *never call one at all*, from any thread, while
the winit event loop is running. A dedicated thread does not make a write safe; it
only moves which thread is holding the deadlock when the picture stops.

Two things about the wake lock are inference and need the device to settle:

- **`SCREEN_BRIGHT_WAKE_LOCK` is deprecated (API 17) in favour of the very flag we
  cannot use.** Deprecated is not removed and `PowerManagerService` still honours
  the level, but if a future release stops doing so the failure is graceful — the
  screen times out mid-drive, and nothing freezes.
- **`held` on the `keep-screen-on: done` line is `WakeLock.isHeld()` read back
  after the call.** It proves the lock was taken; it does not prove the display
  obeyed it. Watch the screen for thirty seconds of driving — that is the
  measurement, and it is the only one that counts here.

## The recovery *was* the crash, and the tier was a stand-in (Adreno 840, 2026-08-17)

A new phone — a OnePlus on Android 16, Adreno 840, i.e. two GPU generations past
the Fairphone 4 everything above was written against — reported this on launch:

```
panic: AndroidSurface failed: ERROR_NATIVE_WINDOW_IN_USE_KHR
at: wgpu-hal-29.0.4/src/vulkan/instance.rs:485:69
```

The panic is not the fault. Seven seconds into a cold start, with the first
tiles still uploading, the Adreno driver lost the device — the mass mesh/texture
churn this project already documents, on a device fast enough to queue more of it
per frame. `handle_render_error` did exactly what the section above added it for:
`DeviceLost`, attempt 1, rebuild the renderer. **That rebuild is what aborted the
process.**

`RenderErrorPolicy::Recover` builds a whole new wgpu instance, adapter and device,
and the first thing bevy does with them is create a surface for the primary
window — while the *old* surface is still in the render world's `WindowSurfaces`,
alive and still holding that window. On a desktop compositor two surfaces on one
window is merely wasteful, and that is why this shipped. On Android it cannot
work: `vkCreateAndroidSurfaceKHR` on an `ANativeWindow` another surface has
`native_window_api_connect`-ed answers `ERROR_NATIVE_WINDOW_IN_USE_KHR`, wgpu-hal's
Android arm is a bare `.expect("AndroidSurface failed")`, and the abort is
unreachable from our error handling. The driver says so plainly one line earlier
in logcat, which is the line to look for:

```
E BufferQueueProducer: ... connect: already connected (cur=1 req=1)
E vulkan  : native_window_api_connect() failed: Invalid argument (-22)
```

The fix was `lib.rs::release_window_surfaces`: drop the old surface before the
handler returns, since dropping is what disconnects the window (wgpu-hal's
`Surface::drop` destroys the `VkSurfaceKHR`, and libvulkan's `DestroySurfaceKHR`
is where `native_window_api_disconnect` lives).

## The recovery that could not be written (2026-08-18)

**That fix swapped one abort for another, and the second one is the lesson.** With
the surface released first, the *next* run on the same device died one layer down:

```
E ... Caught DeviceLost error: Unknown Device is lost
W ... render device lost — rebuilding the renderer  attempt = 1
F ... Abort message: 'Trying to destroy a SwapchainAcquireSemaphore that is still
      in use by a SurfaceTexture'
      wgpu-hal-29.0.4/src/vulkan/swapchain/native.rs:359
```

Both aborts are a `panic!`/`expect` inside wgpu-hal, on the render thread, and
neither is ours to catch. The reason there is no third attempt is that the shape
is wrong, not the details: **the error handler runs inside the render schedule,
mid-frame**, and a swapchain image acquired for that frame is still referenced by
bevy's view targets and by wgpu's own presentation state. There is no point in
there at which a surface can be torn down safely. "Give back everything the new
renderer is about to ask for" — the rule the previous section ended with — turns
out not to be satisfiable from where the handler stands. Deferring the rebuild to
a frame boundary is what would work, and that is upstream's job:
`RenderErrorPolicy::Recover` is supposed to *be* that affordance.

So `DeviceLost` halts like every other kind, and the halt writes the report. If
bevy or wgpu grow a recovery that survives an Android surface, `handle_render_error`
is the function to revisit — and the way to know it works is the device.

**The second finding from the same report: the phone tier is a stand-in for a
device, and this device was not it.** The startup log showed `no atmosphere on
this adapter — drawing the analytic sky dome instead` on a GPU that offers
`FLOAT32_FILTERABLE` and could have drawn the real one. Both halves were wrong.
The *cause* was `quality::Budget::sky_dome`, not the adapter — `quality::auto()`
answered `Budget::phone()` for every Android device, because the budget must be
resolved before a renderer exists (`platform::workers` is sized from it), so
"Android" silently meant "a Fairphone 4". And the *message* asserted the one
reason it was not, which sent the report chasing a missing feature the device has.

The first fix was `quality::gpu_affords_atmosphere`, a table of GPU families
(Adreno ≥ 700, Immortalis, Mali-G ≥ 700, Xclipse ≥ 900) consulted at
`setup_camera` to lift the tier's veto. **It lasted a day, and it was the same
mistake one level down**: a guess about hardware nobody in the loop owns, wrong by
default for every part not on the list, needing an edit per device. Both it and
`Budget::sky_dome` were deleted on 2026-08-18. The adapter is asked whether it can
filter a float32 texture, and if it can, it gets the sky it can draw
(`quality::Capabilities`). A machine that *should not* spend the frame is put on a
lower rung deliberately — Settings ▸ Map, or `CARTO_QUALITY` — which is a person
deciding rather than a table guessing.

The rule that came out of it, and the reason `Capabilities` is a type rather than
three call sites: **if a question can be asked of the device, ask it; if it can
only be judged, it belongs in `Budget` and a person may override it.** The sky is
the worked example. Frame pacing is the second: `frame_limiter` skips itself when
the *window* reports a vsync-paced present mode, rather than when the tier says
`fps_cap: None` — which is how the guard came to be silently lost when Android
moved from the phone rung to the balanced one, with the test that was supposed to
protect it still passing, because it tested the rung.

Only the Adreno arm of that table comes from hardware. The rest is vendor
numbering, which is why the default is "keep the tier's answer".

**Still open, and the more important half:** the device loss itself. A cold start
at 1.7 km altitude uploaded ~174 tiles and, in the run before it, spent five
consecutive seconds at ~1,010 ms per frame with `dequeueBuffer timed out` on every
one. The recovery being survivable is not the same as the burst being affordable;
`docs/notes/headless-shots-software-renderer.md` and the churn entry in CLAUDE.md
are where that thread continues.


## The halt the app could not survive being backgrounded (2026-08-19)

Same phone, two reports five minutes apart, and between them they close the thread
the section above left open.

**The halt is not the end of the process, it is a delay.** Report one: device lost
at 23:38:15, `handle_render_error` halts as designed, the picture freezes and the
report is written — and then at 23:38:40, the moment the app is backgrounded:

```
F ... Abort message: 'Trying to destroy a SwapchainAcquireSemaphore that is still
      in use by a SurfaceTexture'   wgpu-hal-29.0.4/.../swapchain/native.rs:359
```

The same assertion as the abandoned recovery, reached without any recovery at all.
`StopRendering` skips the entire `Render` schedule from then on, and *presenting*
is a system inside it (`renderer::render_system`) — so the swapchain image
`prepare_windows` acquired for the frame that failed is never presented and never
dropped. It sits in `ExtractedWindows`, with views of it in every `ViewTarget` and
in `ViewTargetAttachments`, which the same halt stops anything from clearing.
Extraction, though, keeps running: `set_extract` is not gated. So when
`bevy_winit` removes the primary window's `RawHandleWrapper` on suspend,
`extract_windows` drops the surface, wgpu-hal tears down the swapchain, and the
outstanding acquire semaphore aborts the process.

`lib.rs::release_halted_frame` gave the image back at the halt — all three holders
together, since one surviving `Arc` is as good as three. **It did not work**, and
the reasoning above is where it goes wrong: the holder is not in the render world
at all. `wgpu-core`'s `Surface::discard` is gated on `check_is_valid()`, a lost
device never is, and the raw texture stays parked in `present.acquired_texture`
whatever bevy drops. The 2026-08-20 report is that same abort from a build
containing the fix. It was replaced by `abandon_lost_surface`, which leaks
`WindowSurfaces` so the teardown never runs — see
[device-loss-strands-the-swapchain.md](device-loss-strands-the-swapchain.md) for
the sources and for why no wgpu version available to bevy 0.19 fixes it.

Report two ends in `Error in Buffer::get_mapped_range … Buffer is invalid`, which
CLAUDE.md already names as downstream noise: a readback polled after the device is
gone. It runs on an `AsyncComputeTaskPool` thread, out of reach of any error
handler, so it cannot be prevented from here — only outrun, which is one of the
two reasons Android now leaves at the halt (`exit_on_halt`) instead of holding a
frozen screen. **Both tombstones are the device loss wearing a different hat, and
the fix for both is not losing the device.**

**Which brings the tier question back, with a number this time.** The 23:43 run had
no upload ceiling at all, because a rung chosen in Settings ▸ Graphics bypasses
`android_budget()` — see `upload-bursts.md`, which has that story and the fix. But
the 23:38 run *had* the ceiling and lost the device anyway, and no per-frame
throttle could have helped it: the phone was holding ~1 GB of streamed content.
Its own report said so and nobody could read it — `upload_total_mb = 993` beside an
asset census that had released almost none of it. `UploadMeter::resident_bytes`
makes that a first-class reading (`resident_mb` in the census and the slow-frame
line, `gpu_resident_kb` in `--dump-state`), and `quality::handset_guarded` caps the
two fields that drive it on every rung a handset can reach: 991 MB → 371 MB over
Groningen, with shadows, bloom, the detail layers and the native render scale all
kept.

Three rules came out of this and are worth more than the diff:

- **A rate guard and a total guard are different guards.** Every bound in this
  codebase before today was a rate. A driver refusing an allocation does not care
  how slowly you asked.
- **The tier has to be in the log.** `quality::init` answers before `LogPlugin`
  exists, so the rung had never once appeared in an Android log and had to be
  inferred from `peak_asset_kb`. `quality::report_budget` says it again from
  `Startup`, with `chose=` naming which of env / settings / auto won.
- **A `#[cfg]`-gated rule is a rule no test can see** — for the second time. The
  guard is a plain function compiled everywhere; only its *application* is
  `cfg!`-conditional.
