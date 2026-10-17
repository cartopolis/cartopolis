---
topic: audio-off-platform
triggers: [audio, soundscape, bevy_audio, cpal, rodio, AAudio, WebAudio, AudioContext, autoplay, "no sound", "silent on Android", "silent in the browser", "--no-default-features", data-cargo-features, "audio feature", soundscape.rs, systems::soundscape, platform/audio.rs, crates/cartopolis/index.html, crates/cartopolis_android/Cargo.toml, crates/cartopolis/Cargo.toml, .forgejo/workflows/ci.yml, headroom, to_i16, effects_volume]
updated: 2026-10-17
---

# Audio on the phone and in the browser

Both were silent, for the same reason and one extra one. Fixed 2026-08-15; this
is what to know before touching either lane again.

## `audio` is a default feature, and every non-desktop lane turns the defaults off

`cartopolis`'s `audio` feature is default-on. That is a **Linux desktop**
decision and nothing more: `bevy_audio` pulls `alsa-sys`, whose build script
needs `libasound2-dev` and hard-fails without it, so a host without the headers
builds `--no-default-features` to avoid a new system prerequisite.

The trap was that Android and the web *also* built `--no-default-features` — for
a completely unrelated reason (they could not cross-compile the bundled SQLite
that the since-removed `solo` feature needed) — and cargo has no way to say
"drop that default, keep this one".
So both shipped for months with the whole soundscape compiled out: no footsteps,
no landing thud, no wind, and no driver turn cues, which on a phone in a car is
the one part of navigation you can use without looking at the screen.

Nothing reported it. Every system in `systems::player::soundscape` is
`#[cfg(feature = "audio")]` (the synthesis beside them is not — see below), so the
absence is not a runtime failure — there is simply no system registered. The only visible trace was Settings ▸ Audio saying "this build has no
audio support", on a screen nobody opens to find out why it is quiet.

**The rule this leaves behind:** a build lane that passes
`--no-default-features` must name `audio` back. Three do, and they are listed in
`crates/cartopolis/Cargo.toml` beside the feature itself: the APK
(`crates/cartopolis_android/Cargo.toml`, `features = ["audio"]`), the web build
(`crates/cartopolis/index.html`, `data-cargo-features="audio"`) and CI's wasm
check (`.forgejo/workflows/ci.yml`, `--features audio`). A fourth that forgets
would be the same bug again.

Neither platform needs a byte of ALSA, which is what makes the original gate
irrelevant to them:

- **Android** — `bevy_audio` → rodio → cpal resolves to the NDK's **AAudio**
  backend (`libaaudio.so`, a plain C API in the platform since API 26; minSdk
  here is 28). No `oboe`, no C++ build, no `classes.dex`, and **no permission** —
  `RECORD_AUDIO` gates capture, not playback. `default_output_device()` returns a
  device with no JNI at all; the only JNI in that backend is
  `AudioManager.getFramesPerBuffer`, which is fallible with a fallback and which
  `android-activity`'s glue has already made reachable
  (`ndk_context::initialize_android_context`).
- **wasm** — `bevy_audio` enables `rodio/wasm-bindgen` (hence `cpal/wasm-bindgen`)
  on `target_arch = "wasm32"` by itself, and cpal's WebAudio backend has an
  explicit `target_feature = "atomics"` arm for copying out of a
  `SharedArrayBuffer`, which is what the shipped threads build compiles to.

## The browser's second problem: a context that never starts

Turning the feature on is necessary and not sufficient. cpal builds its
`AudioContext` inside `AudioPlugin::finish` — seconds into page load, with
nothing clicked — and calls `resume()` exactly **once**, right there. Under every
browser's autoplay policy that promise is rejected, and there is no retry. The
context stays `suspended` for the whole session.

Nothing throws, nothing logs, `open_default_sink()` still succeeds (a rejected
promise is not an `Err` from `web_sys`), every `AudioPlayer` entity still gets its
sink. It is silent and it looks healthy.

It cannot be fixed from Rust. rodio owns the cpal stream, `bevy_audio` owns the
rodio sink, and no layer exposes the `AudioContext`. So `index.html` subclasses
the global `AudioContext` constructor before the module is fetched, keeps every
instance, and resumes them from a capture-phase gesture listener on `window`.
Three details there are load-bearing:

- **A subclass, not a wrapping function**, so instances remain real
  `AudioContext`s and wasm-bindgen's `instanceof` checks still hold.
- **Capture phase.** winit `preventDefault`s pointer events on the canvas, so a
  bubble-phase listener would miss the one gesture that matters — a click on the
  map.
- **Not `once`.** The first gesture usually lands *during* the ~70 MB download,
  before any context exists, and a backgrounded tab can suspend one again later.
  The listeners stay armed and the constructor also resumes on creation, so
  whichever of the two happens second does the unlocking.

`window.__audioContexts` is there for diagnosing this from a console.

Cost of the stall while suspended: cpal keeps scheduling buffers into the dead
context, so on resume it replays the backlog at once until its clock catches up.
Seconds of page load are a short burst; it is audible and it is over. Unlocking
on the earliest gesture is what keeps it short.

## What it plays is made at startup, and a made clip must keep its headroom

Every sound is synthesised into WAV bytes when the soundscape starts
(`soundscape::synth`) rather than shipped — `avatar-game-feel.md` has why (a
shipped clip would be an LFS object). The synthesis and the volume/step-rate
maths are pure functions outside the `audio` cfg, so they compile and are tested
on a host that cannot build the backend; only the ECS wiring is gated.

**A synthesised clip must leave headroom, and the suite asserts it**
(`no_synthesised_clip_runs_out_of_headroom`). The wind loop was `lp2 * 6.0`:
measured, it peaked at exactly 1.000 with **1.08 %** of its samples clamped by
`to_i16`. Clipping a low-passed rush manufactures precisely the top end the
filter was there to remove, so it arrived as a wind *tunnel* at any playback
volume — and no `Volume::Linear` undoes it, because the distortion is in the
asset. The footstep had the matching fault the other way: one pole at 90 ms,
measured at a **~2.5 kHz** zero-crossing rate — a bright hiss fired two or three
times a second, which a phone user reported as wind rather than walking. Both
were re-cut against measurements on 2026-08-16: the step to two poles (energy
around 300–400 Hz) and a 70 ms decay, the wind to ~120 Hz with ~3 dB to spare
(peak ≈ 0.46). Loudness is then the listener's: `AppPrefs::effects_volume` is the
slider for the half no measurement can judge.

## What has been verified, and what has not

**Android: the output stream really opens.** Measured 2026-08-15 on the VPS
emulator (API 34 x86_64, the arm64 APK through `ndk-translation`, started without
`-no-audio` so the guest has a real `AUDIO_DEVICE_OUT_SPEAKER` mixer). The signed
APK links `libaaudio.so` and imports `AAudioStreamBuilder_openStream`; on launch
logcat gives

```
AAudioStreamBuilder: rate = 44100, channels = 2, callback: ON with frames = 2048
AAudio  : AAudioStreamBuilder_openStream() returns 0 = AAUDIO_OK for s#1
AAudio  : AAudioStream_requestStart(s#1) returned 0
AF::TrackHandle: OpPlayAudio: track:55 usage:1 not muted
```

and AudioFlinger's primary mixer thread goes from `Standby: yes` before launch to
`Standby: no` after, mixing continuously — that is the startup wind loop, which
runs from `setup_soundscape` at volume 0. So the path from a synthesised WAV to a
device track is whole. What that does *not* prove is an Adreno/Mali handset, or
that the levels are right: the emulator's sink is `QEMU_AUDIO_DRV=none`, so
nobody heard it. `docs/notes/android-build.md` has the emulator setup.

**Web: compilation only.** `cargo check` passes for `wasm32-unknown-unknown` with
the feature on, and the unlock script's logic is unit-exercised against a stubbed
`AudioContext` — but there is no browser here, so the autoplay behaviour itself is
untested against a real policy. Still open on both: whether the resume burst is
objectionable in practice, and whether Android audio survives an app
pause/resume cycle.

Microphone capture is a separate seam (`platform::audio`), under the same feature.
The web arm is `getUserMedia`; on Android the desktop cpal arm serves capture
over AAudio unchanged, behind a runtime `RECORD_AUDIO` ask, so
`AudioInput::available()` is `true` on both (`voice-chat.md` has the permission
and what is still unverified). The comment on the `cartopolis` dependency in
`crates/cartopolis_android/Cargo.toml` that says the capture half "stays off
here" predates that and is stale.
