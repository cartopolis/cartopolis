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

- `str0m` 0.24, `sframe` 2.0 (`rust-crypto`) and `opus-pure` 0.2 check for
  x86_64, `aarch64-linux-android` and `wasm32-unknown-unknown` (web flags,
  nightly). **But str0m's own `rust-crypto` backend is not C-free**: it turns
  on `dimpl`'s `rcgen` feature, which turns on `aws-lc-rs`, a C library, only
  to make the DTLS certificate. The first run of this check missed it because
  `cargo tree -e build -i cc` walks only build edges from the root and prints
  nothing; `-e normal,build` finds it (control: it finds `cc` under the
  simulator's `mlua-sys`). So `cartopolis_calls` carries upstream's provider
  with a RustCrypto certificate (`crates/calls/src/crypto`), and with that the
  same query prints nothing for all three targets.
- Two str0m clients through a str0m forwarder over loopback UDP, 6 s: 280 of 281
  SFrame-sealed Opus frames arrived, decrypted and decoded (tone RMS 0.354 in,
  0.350 out); 56 of 56 sealed fake VP8 frames arrived bit-exact; the forwarder
  flagged all 6 keyframes from the clear header; no sealed packet equalled its
  plaintext.

- **Video codec: `oxideav-vp8`, pure Rust, encoder and decoder** (2026-09-29,
  ffmpeg 7.0.2 static as the reference). It decoded a libvpx realtime stream
  (640×480, `-deadline realtime -cpu-used 8`) bit-identically to ffmpeg on 120
  of 120 frames, at ~4.5 ms a frame; ffmpeg decoded its own output
  bit-identically on 90 of 90. Encoding with trellis off: ~40 ms a frame at
  320×240 (~490 kbit/s at 30 fps, 39 dB luma PSNR) and ~150–215 ms at 640×480,
  on the VPS under load 15. Hence 320×240 at 15 fps, encoded off the audio
  thread. libvpx (C, desktop only) or a phone's MediaCodec are the ways up.
- **A late client was dropped, and was not dead.** str0m's ICE-lite side
  prunes a pair after 15 s with no check and reports `Disconnected`; a join
  answered on a slow frame arrives later than that. Both ends now give a
  connection 30 s to come back (`media::RECONNECT_GRACE`). Found by two
  headless clients calling each other on lavapipe.
- **Headless call test** (2026-09-29): two `--mcp-headless` clients, one
  calling over the MCP `call` tool, the other with `CARTO_ANSWER=1`, both with
  the stand-in camera and tone: 50 audio frames a second each way, the test
  pattern drawn in the call bar with the right colours. Video arrived at ~1
  picture a second because a picture is sent once per app frame and lavapipe
  renders about one; the session and forwarder are not the limit.

## Running it

`calls_listen` (UDP) turns calls on; the dev binary defaults it to
`127.0.0.1:4749` as it does the HTTP port. `calls_public_addr` is the
`IP:port` clients reach it at, and is required behind NAT or on `0.0.0.0`:
WebRTC puts that address in the SDP as the one candidate, so it must be an
address, not a hostname. The port is UDP and goes straight to the container —
Caddy carries none of it — so a deployment publishes it in Docker and opens it
in any firewall. `calls_addr` on `SimHandle` says what was bound.

## Known gaps in the server

- A block made during a call applies from the next join, not at once.
- A device that connects while a call is already running learns of it only
  when the roster next changes (someone joins or leaves).
- Ringing reaches connected devices, and a closed Android app through
  UnifiedPush (below). A closed desktop app or browser tab is not rung.

## Not yet known

Echo cancellation on Android needs AAudio's `VOICE_COMMUNICATION` input preset,
which cpal cannot set; the browser's encoded-transform worker glue against our
wasm build; whether the Adreno 840 has a hardware VP8/VP9 encoder; how the
phone's upload budget (`upload-bursts.md`) copes with a decoded video stream,
which matters doubly for video on a prim.

## The phone's camera (2026-09-30)

Camera2 through its C API (`libcamera2ndk`) into an `AImageReader`
(`platform::camera`'s `phone` module): no Java and no GPU surface, and the
pictures arrive as 4:2:0 planes the call encodes directly. The front camera is
chosen when there is one. Three decisions:

- **Upright by the sensor's orientation, assuming a portrait phone.** Following
  the phone as it turns needs the display rotation, which only Java has.
- **The stream turns with the picture.** A portrait picture is sent as 240×320
  rather than squashed into 320×240; a turn starts the stream again with a full
  picture (`video::Encoder::encode`), and the call tiles take each picture's
  own shape.
- **Declaring `CAMERA` changes the place photo.** An app that declares the
  permission must hold it before Android honours `ACTION_IMAGE_CAPTURE`, so
  `platform::photo` now asks for it before opening the camera app.

**Not run on a device**: the emulator on the VPS cannot run this app, so the
permission prompt, the orientation and the picture itself are unverified.

## Calls in the browser (2026-09-30)

The browser's own WebRTC, microphone, camera, Opus and VP8 (`web/webcall.js`,
driven from `platform::web_call`), in the same shape as the native link: one
connection up, one down, both to the forwarder. Every encoded frame passes
through an encoded-transform worker (`web/call-worker.js`) that seals it on the
way out and opens it on the way in, so the forwarder carries SFrame from a
browser exactly as from the app. Decisions:

- **SFrame in JavaScript over WebCrypto, not the Rust compiled twice.** The
  worker would need its own wasm build with the threads flags; ~200 lines of
  HKDF and AES-GCM are cheaper. What keeps it identical is
  `crates/calls/testdata/sframe-vectors.json`, written by the Rust test
  `the_browser_vectors_are_what_this_seals` and checked by
  `bun test crates/cartopolis/web`.
- **Two deviations the port had to copy, found by those vectors.** The suite
  id in the key-derivation label is RFC 9605's `0x0004` for AES-128-GCM, not
  `1`; and the `sframe` crate authenticates the clear VP8 bytes *before* the
  header, where RFC 9605 §4.4 puts the header first. The native client is what
  a browser has to agree with, so the port follows the crate. Another SFrame
  implementation would not interoperate with either until that is settled.
- **Both transform APIs.** `RTCRtpScriptTransform` where it exists (Firefox,
  Safari, Chrome 141+ — this container's Chrome 154 has it), Chrome's older
  `createEncodedStreams()` otherwise; neither means no call, and the button
  says so.
- **The address switches of the call harness (`?call=`, `?answer`) only work
  on a page served from this machine.** On the public site a link carrying
  `?answer` would open a visitor's microphone to whoever rang.

**Verified 2026-09-30**, `tools/web_call_test.ts`: the web client in headless
Chrome 154 (its fake camera and microphone) and the native client (stand-in
camera and tone) in one call through a local simulator. After four minutes the
app had decoded 3,391 pictures and ~11,300 audio frames from the browser; the
browser's RTP statistics showed 3,063 pictures and 11.3 million audio samples
decoded from the app. Every frame both ways was sealed and opened by the two
SFrame implementations, so this is also the cross-check the vectors are for.

What the test found along the way, all fixed:

- **No browser could sign in to a simulator** (`ws_transport::send`): the
  threaded build's memory is a `SharedArrayBuffer`, `WebSocket.send` refuses a
  view onto it, and the hello was sent as one. Fixed by copying first. The call
  keys handed to the worker had the same hazard.
- **The camera pressed in the first moment of a call did nothing**: the video
  sender does not exist until the microphone has opened, and the request was
  dropped while the app went on believing the camera was on.

And what a test has to know:

- **Two people must be contacts** (or have a contact request between them)
  before the server opens a private chat, so before they can call
  (`simulator::contacts`, rule 4). The test inserts the contact.
- **This container's default WebGPU adapter loses the device on the first
  canvas present.** `--enable-features=Vulkan,VulkanFromANGLE,DefaultANGLEVulkan
  --use-angle=vulkan` keeps it; the renderer still halts on a pipeline this
  software GPU rejects, and the call runs on regardless.
- **A debug web build dies on two things that are not calls**: rodio 0.22's WAV
  `size_hint` subtracts a count that is already decreasing (an overflow panic
  under overflow checks), and after a renderer halt a system whose resource was
  removed panics where a release build only logs. Build the test with debug
  assertions and overflow checks off, as the shipped build has them.

## Ringing a closed app (2026-09-30, protocol 29)

A phone with a **UnifiedPush distributor** installed (ntfy is the common one)
is woken when a call starts in one of its chats. `Push.java` asks the
distributor for an endpoint at startup, `PushReceiver.java` keeps it in the
`cartopolis_push` preferences, and the running app hands it to the server
(`SetPushEndpoint`, `systems::push`). When the first person joins a call, the
server posts to every other member's endpoints (`simulator::push`), and the
receiver shows a ringing "Incoming call" for 45 s. Opening it connects, and the
roster rings as it does for any connected device. Decisions:

- **The push is empty.** Not who is calling, not which chat — the
  distributor's server relays it. The app learns the rest over its own
  connection, which is also why the receiver needs no native code.
- **UnifiedPush, not Firebase**, for ConnectionService's reason: no Google
  Play services. Spoken as three broadcasts by hand, because the connector
  library is an AAR, i.e. Gradle.
- **The endpoint is a stranger's URL and the server is inside the private
  network**, so a push goes to `https:` only and to public addresses only,
  checked by the resolver at connect time (no redirects). A 404/410 from the
  distributor removes the endpoint. One push per endpoint per 20 s.
- **Not rung:** the caller's own devices, and anyone who blocked the caller.
  **Not verified on a device**: the emulator here cannot run the app, so the
  broadcast exchange with a real distributor and the notification are
  untested; the server half is tested with a database and the Java is
  compiled against android-34.

## A call that waits (2026-09-30)

Both ends used to create the *down* (receiving) connection at the join and
tick it from then on, though nothing is negotiated on it until a second person
joins. Its DTLS handshake timer ran from creation, so a caller alone for ~30 s
was dropped ("DTLS error: timeout: handshake"), and one answered later still
failed the moment it was used ("timeout: connect"). The down connection is now
**made at its first offer**, on both sides. And the forwarder **never
renegotiates a receiving connection down to nothing**: an offer stopping every
track has no BUNDLE group, str0m refuses it, and that ended the call for the
last person left in it. The departed tracks wait to be stopped by the next
offer; the client drops their pictures on the roster.
`link::tests::a_caller_alone_for_a_minute_is_still_in_the_call` covers all three.

Agents reach contacts through MCP (`request_contact`, `answer_contact`,
`contacts`), because a call needs two people to be contacts first.
