---
topic: calls
triggers: [calls, "voice call", "video call", str0m, sframe, SFrame, SFU, opus, opus-pure, VP8, VP9, WebRTC, webrtc, Galene, "video on a prim"]
updated: 2026-09-29
---

# Calls: encrypted voice and video, forwarded by the simulator

Private-chat calls (one-to-one and group, voice and video) are **end-to-end
encrypted** and travel **over UDP through the simulator**, which forwards frames
it cannot open. Proximity voice (`voice-chat.md`) is a different thing and stays
as it is: it is public, like nearby chat.

## Decided (2026-09-29), and why

- **WebRTC protocols, not Google's library.** Native clients and the simulator
  use **str0m** (pure Rust, no sockets or threads of its own, so it fits the
  simulator's loop and one UDP port). The browser uses its own WebRTC. Google's
  C++ libwebrtc (what Signal's RingRTC and LiveKit's Rust SDK wrap) was rejected:
  C++ toolchain, size, and no wasm.
- **str0m over webrtc.rs.** webrtc.rs's advantages are TURN and ICE-over-TCP,
  and we chose UDP-only through a server with a public address, which needs
  neither. Its current architecture was also two months old (0.20, 2026-07).
- **UDP only.** No TCP fallback yet; a network that blocks UDP gets a clear
  "calls are blocked on this network" instead. Galène's built-in TURN port is the
  model if that ever changes.
- **The simulator is the SFU, always — even one-to-one.** No peer-to-peer: the
  server has a public address so every call connects without TURN, nobody
  learns anyone's IP, one path serves one-to-one and groups, and the server can
  refuse a blocked person before media flows. Forwarding only, no transcoding.
- **Encryption is SFrame (RFC 9605) keyed from the private chat's MLS group**
  (`MLS-Exporter("SFrame 1.0 Base Key", "", …)`, RFC 9605 §5.2), with the `sframe`
  crate's `rust-crypto` backend. A group change is a new epoch and therefore a
  new key, so someone who left cannot hear what comes after.
- **VP8 leaves its payload header in the clear** — 10 bytes on a keyframe, 3
  otherwise, as Jitsi does — carried as SFrame *metadata*, so it is
  authenticated but readable. That is what lets the forwarding server see
  keyframes and so answer keyframe requests without seeing a pixel.
- **Opus via `opus-pure`** (pure Rust, BSD-3). This reverses the ADPCM decision
  in `voice-chat.md` for calls only: the reason Opus lost (every binding needed
  `cc`) no longer holds.
- **VP8 is the baseline video codec, VP9 an upgrade later.** Every Android
  device must ship Google's software VP8 encoder (`c2.android.vp8.encoder`,
  CDD §5.1), reachable through MediaCodec, so no C is compiled for the phone.
  Hardware VP8 *encoding* is the chip maker's choice and often absent. VP9
  decodes everywhere but software VP9 encoding is too slow for a phone; a
  sender may use it only where it has a hardware encoder. Receivers decode
  both, so senders can differ within one call.
- **H.264 is ruled out.** Cisco's openh264 licence only covers Cisco's own
  downloaded binary; building it ourselves is a patent question we will not take.
- **Galène is a reference, not a dependency.** Go, and it decrypts media on the
  server. What we take from it: one direction per connection with the sender
  offering; receivers request streams (and a low-quality variant) explicitly;
  two simulcast layers, not three; NACK + PLI/FIR repair and automatic restart
  of failed flows; full congestion control server→receiver, simpler sender→server;
  one UDP port demultiplexed in user space; ~20 people per meeting per core.

## Measured

2026-09-29, a throwaway crate outside the workspace:

- `str0m` 0.24 (`default-features = false, features = ["rust-crypto"]`),
  `sframe` 2.0 (`rust-crypto`) and `opus-pure` 0.2 check for x86_64,
  `aarch64-linux-android` and `wasm32-unknown-unknown` (web flags, nightly), and
  `cargo tree -e build -i cc` prints nothing — no C anywhere.
- Two str0m clients through a str0m forwarder over loopback UDP, 6 s: 280 of 281
  SFrame-sealed Opus frames arrived, decrypted and decoded (tone RMS 0.350 in,
  0.354 out); 56 of 56 sealed fake VP8 frames arrived bit-exact; the forwarder
  flagged all 6 keyframes from the clear header; no sealed packet equalled its
  plaintext.

## Not yet known

Echo cancellation on Android needs AAudio's `VOICE_COMMUNICATION` input preset,
which cpal cannot set; the browser's encoded-transform worker glue against our
wasm build; whether the Adreno 840 has a hardware VP8/VP9 encoder; how the
phone's upload budget (`upload-bursts.md`) copes with a decoded video stream,
which matters doubly for video on a prim.
