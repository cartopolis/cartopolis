---
topic: voice-chat
triggers: [voice, VoiceFrame, VoiceData, ADPCM, codec, opus, microphone, RECORD_AUDIO, push-to-talk, jitter, "proximity voice", systems::voice, platform::audio]
updated: 2026-08-16
---

# Proximity voice: what is decided, and what is still unproven

The wire (protocol 15) and the server fan-out landed well before any client
could speak. `systems::net::voice` is the client half; this note holds the parts
that would otherwise be re-argued.

## The codec is IMA ADPCM, and Opus was rejected on toolchain grounds

Not on quality — Opus is better at every bitrate and would be the choice on a
desktop-only client. Every Rust binding to it (`audiopus`, `opus`) compiles
libopus through `cc`, which is a C toolchain requirement on the two lanes this
feature exists for: the Android APK is built without Gradle or a JDK
(`android-build.md`) and the web build is a trunk/wasm job. Acquiring a C build
prerequisite on both to save bandwidth nobody is short of is the trade that lost.

The numbers that made ADPCM sufficient: 16 kHz mono, 4 bits a sample, a 20 ms
frame is **164 bytes** — inside the server's `MAX_VOICE_BYTES` (1500, sized so a
frame never fragments when this moves to datagrams) with an order of magnitude to
spare, and ~8 kB/s per speaker. Raw PCM at the capture rate is 96 kB/s and was
never an option.

The upgrade path is one byte: every payload starts with a codec tag, and a
decoder that meets a tag it does not know drops the frame. That byte exists
*before* there is a second codec because the dangerous moment is two clients of
different versions standing next to each other.

## Frames carry their own ADPCM state, and that is not an optimisation

The natural framing lets the predictor and step index run across frames
implicitly. One lost packet then corrupts every frame after it. Writing the
entry state into each frame costs 3 bytes and makes a loss cost 20 ms. The
suite pins it (`every_frame_decodes_without_its_predecessors`) because the
failure only appears under packet loss, which no test here can produce.

## Rejected: reordering on receipt

Out-of-order and duplicate frames are dropped, not buffered into place. The
carrier is TCP, where a late frame was late for a reason no client-side reorder
fixes; `seq` is on the wire for the datagram transport the protocol anticipates,
and that is when to revisit this. A reorder buffer written now would be untested
code guarding a case the transport cannot produce.

## Android needed no new backend, only a permission

cpal 0.17's Android host is AAudio and implements capture as well as playback, so
the desktop arm of `platform::audio` compiles and runs unchanged on the phone —
no `oboe`, no C++, no dex. What it needed was `RECORD_AUDIO`, which is
*dangerous* and therefore a runtime ask; `platform::android::ensure_permission`
generalises what the location provider already did. The ask happens **on the
capture thread**, because the answer can only be polled
(`onRequestPermissionsResult` is a Java override this app cannot supply) and the
wait is up to two minutes.

`AudioInput::available()` therefore answers `true` on Android now. It still
promises only that a *backend exists* — not that a device opened or that the user
agreed. Those are `AudioInput::error`'s to report, and folding them in would mean
opening a microphone to answer a capability question.

## What is verified, and what is not

Verified here, 2026-08-16, and reproducible: `systems::voice::loopback` runs a
real simulator on loopback, sends three encoded frames at the real 20 ms cadence
(the pacing matters — `net.rs` refuses anything closer than 15 ms and says
nothing when it does) and asserts the waveform survives resampling, compression
and the round trip to within 5 % of full scale. The codec, the resampler, the
gate, the attenuation curve and the jitter bound are unit-tested.

**Unverified, and not verifiable in this container** (`container-audio.md`): that
a microphone opens on any real device, that the permission dialog appears, that
playback is audible, and everything about echo. Capture in the test is a
generated tone and playback is asserted as samples in a queue. The first person
to run this on a phone and a desktop at the same time is doing the part no test
here can do.

## A block already silences a voice, and the client does not have to help

`ChatHub::deliver_to` tests the recipient's block list before it enqueues, and
voice goes through it on `Lane::Voice` exactly as text does on `Lane::Text` — so
a blocked person is inaudible without the voice code knowing blocks exist. This
is the `user-safety.md` rule holding: the refusal is in the fan-out, never in the
panel. Note the one asymmetry it leaves — the client hides *already-received*
text from someone newly blocked, and there is no equivalent for voice, because
audio already played cannot be unplayed.

## Acoustic echo cancellation is a hole

The browser gets it free (`getUserMedia`'s `echoCancellation` constraint, already
requested). Native and Android get **nothing** — cpal hands over raw capture, and
AAudio's `VOICE_COMMUNICATION` preset, which is where an Android device's
hardware AEC lives, is not reachable through cpal's API. So two people in one
room on speakerphone will hear themselves. Headphones make it a non-issue and
push-to-talk mostly does; open-mic on a phone speaker is the case that will be
reported.
