---
topic: container-audio
triggers: [audio, voice, microphone, cpal, ALSA, asoundrc, snd-aloop, opus, AEC, echo cancellation, platform::audio, "fake audio device", "no sound device", "can we test audio"]
updated: 2026-08-10
---

# The agent container can build an audio stack but cannot hear one

Voice work (issue #32 and its client half) keeps raising the same question, so
here is the measured answer rather than a fresh investigation each time.

## What is present

Measured 2026-08-10 in `cartopolis-agent`:

- `libasound.so.2` **and** `/usr/include/alsa/asoundlib.h`, with
  `pkg-config alsa` reporting 1.2.11. So `cpal`/`alsa-sys` compile here with **no
  system package install** — the usual first blocker is already absent.
- `pcm.null { type null }` is defined in `/usr/share/alsa/alsa.conf:335`, so a
  PCM exists that needs no card.

## What is absent, and cannot be added from inside

- No `/dev/snd`, no card. `default` therefore fails to open at all
  (`parse_card: cannot find card '0'`) until something defines it.
- No `aplay`/`arecord`/`pactl`/`pulseaudio`/`pipewire`.
- The container's bounding capability set has no `CAP_SYS_MODULE`, so
  **`snd-aloop` cannot be loaded from in here**. It is a host change or nothing.

## The measurement that decides how useful `null` is

Writing `pcm.!default { type null }` into `~/.asoundrc` makes `default` open
cleanly for both playback and capture at 48 kHz / S16_LE / mono. That is enough
for `cpal` to enumerate a device and for a stream to start.

But it does not clock. A C probe (`snd_pcm_set_params` at 48 kHz mono, then 100 ×
480-frame writes = exactly 1.000 s of audio) returned in **0.000 s of wall time,
in both directions** — thousands of times realtime, in a free run.

So `pcm.null` answers *"does the code path open, negotiate a format and not
panic?"* and nothing else. Anything whose correctness is a function of time —
jitter buffer behaviour, underrun and overrun recovery, clock drift between
capture and playback, callback pacing — will **pass instantly against `null`
while proving nothing**. That is worse than having no test, because it reports
green.

## The three tiers, and what each actually buys

| Tier | Cost | Buys |
|---|---|---|
| `pcm.null` via `~/.asoundrc` | two lines, no install, works today | stream opens, format negotiation, no panic. **No timing.** |
| PulseAudio `module-null-sink` + a virtual source | pulseaudio in the agent image (Dockerfile change; no host change, no kernel module) | properly clocked virtual devices — capture→encode→decode→playback end to end |
| `snd-aloop` | `modprobe` on the VPS + `/dev/snd` mapped in — **a host kernel change, so ask first** | true loopback: what is played on one subdevice is captured verbatim on another, so tests can assert on the samples themselves |

None of the three buys **acoustic echo cancellation quality**. AEC is judged
acoustically and a null sink has no acoustics; on web `getUserMedia` supplies it,
on native it is ours to solve and to judge on a real machine with a real headset.

## Consequence for how voice gets tested

The highest-value tests need no device at all: Opus framing, sequence handling
and reordering, resampling, the distance-attenuation curve, jitter-buffer logic
driven by an injected clock. These are pure functions over buffers and run here
today — write them first regardless of which tier is chosen.

Reach for tier 2 only when a bug is suspected at the device boundary itself, and
tier 3 only when a test needs to assert on samples that made a full round trip.
