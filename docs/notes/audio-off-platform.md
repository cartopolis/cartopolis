---
topic: audio-off-platform
triggers: [audio, soundscape, bevy_audio, cpal, rodio, AAudio, WebAudio, AudioContext, autoplay, "no sound", "silent on Android", "silent in the browser", "--no-default-features", data-cargo-features, "audio feature"]
updated: 2026-08-15
---

# Audio on the phone and in the browser

Both were silent, for the same reason and one extra one. Fixed 2026-08-15; this
is what to know before touching either lane again.

## `audio` is a default feature, and every non-desktop lane turns the defaults off

`cartopolis`'s `audio` feature is default-on. That is a **Linux desktop**
decision and nothing more: `bevy_audio` pulls `alsa-sys`, whose build script
needs `libasound2-dev` and hard-fails without it, so the agent container and CI
build `--no-default-features --features solo` to avoid a new system prerequisite.

The trap is that Android and the web *also* build `--no-default-features` — for a
completely unrelated reason (they cannot cross-compile the bundled SQLite that
`solo` needs) — and cargo has no way to say "drop that default, keep this one".
So both shipped for months with the whole soundscape compiled out: no footsteps,
no landing thud, no wind, and no driver turn cues, which on a phone in a car is
the one part of navigation you can use without looking at the screen.

Nothing reported it. `systems::player::soundscape` is `#[cfg(feature = "audio")]`
end to end, so the absence is not a runtime failure — there is simply no system
registered. The only visible trace was Settings ▸ Audio saying "this build has no
audio support", on a screen nobody opens to find out why it is quiet.

**The rule this leaves behind:** a build lane that passes
`--no-default-features` must name `audio` back. Three do, and they are listed in
`crates/cartopolis/Cargo.toml` beside the feature itself. A fourth would be the
same bug again.

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

Microphone capture is a separate seam (`platform::audio`) and separate news: the
web arm is now compiled and `AudioInput::available()` is finally `true` in the
browser, while **Android capture is still unimplemented** and correctly reports
so.
