---
topic: android-build
triggers: [cartopolis_android, build-apk.sh, AndroidManifest.xml, android_main, NativeActivity, cargo-ndk, deploy-android, apksigner, platform::android, hasCode, classes.dex, JNI, "android", "apk", "phone", "ndk"]
updated: 2026-08-13
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

Phase 1 is the **offline viewer**, the same shape as Phase 1 on the web: the
streamed OSM / 3DBAG / AHN map and a local avatar, with no simulator, no auth and
no world persistence. Two consequences that look like bugs if you don't know:

The APK is built `--no-default-features`, so the `solo` feature — and with it the
in-process `cartopolis_simulator` and its bundled SQLite — is not compiled at all.
Nothing cross-compiles `rusqlite` for Android today, and nobody has tried.

What that leaves on the phone is a *map viewer*, and it is worth being concrete
about the edges, because two of them are not Android's doing. The camera works
fully — pan, pinch, twist, tilt are all `TouchGestures`, and taps reach picking,
the gizmo and the pins through the emulated mouse (`docs/notes/touch-input.md`).
**The avatar cannot walk**: movement is WASD and there is no touch equivalent
anywhere in the tree, so this is the same gap the web build has on a phone rather
than something the port introduced. **Typing works, but only through a keyboard
we draw ourselves** — the platform IME is unreachable under NativeActivity (the
trade above), so `systems::onscreen_keyboard` stands in for it everywhere:
address bar, geo search, bookmark rename, chat, the AI panel.

`lib.rs`'s `parse_cli()` hands Android a fixed `--offline`. It cannot call
`Cli::parse()`: a NativeActivity process is started by the zygote, so the `argv`
Rust captured belongs to `app_process` and is full of flags clap has never heard
of — and clap's response to an unknown flag is `exit(2)`, i.e. the app dying on
launch with nothing on screen and one line in `logcat`. There is no equivalent of
the web build's `?query=string` switch surface yet; anything the phone needs to be
told has to become UI or an env default.

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
stack against a runner with capacity 2 is not that.

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
