---
topic: calls
triggers: [calls, meeting.rs, meeting.ts, "core::meeting", "meeting record", poll, agenda, "voice call", "video call", str0m, sframe, SFrame, SFU, opus, opus-pure, VP8, VP9, WebRTC, webrtc, Galene, "video on a prim", calls.rs, call_screen.rs, call_tones.rs, systems::call_screen, platform/camera.rs, hw_video.rs, web_call.rs, webcall.js, call-worker.js, call.ts, call-tones.ts, webcall.d.ts, web_call_test.ts, web-push.ts, crates/atlas/web/src/sw.js, crates/calls, cartopolis_calls]
updated: 2026-10-17
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
- **`cartopolis_calls` is split by feature the way `e2ee` is split by crate.**
  `client` (SFrame sealing and opening, the codecs, a device's session) is what
  the app takes; `sfu` (a forwarder of bytes it cannot open) is what the
  simulator takes, and it must take `client` only in its dev-dependencies, so
  the server binary contains no code that could open a frame even if handed a
  key (`crates/simulator/Cargo.toml`, `encrypted-chat.md`).
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
- **VP8 was the baseline; the app sends VP9 since 2026-10-04 (protocol 32).**
  The 2026-09-29 assumption that "software VP9 encoding is too slow for a
  phone" was about libvpx and was never measured here. `rusty_vp9` (pure Rust,
  no dependencies, decoder bit-exact against libvpx's 315 conformance vectors)
  measured faster than the VP8 encoder already in use, at well under half the
  bitrate (below). Every receiver decodes both — the app through `video::Decoder`,
  which follows a sender's codec frame by frame, a browser through its own
  WebRTC. A browser sends VP9 profile 0 too since 2026-10-17, VP8 only where it
  has no VP9 encoder (`webcall.js`, `sendCodecs`); no protocol bump, since
  protocol 32 already made every receiver decode VP9. See "Browsers sending
  VP9" below for how its worker knows each frame's codec.
  Hardware VP9 *encoding* is still rare on phones (Qualcomm has historically
  shipped a VP9 decoder only); it does not matter at 320×240, where the CPU
  keeps up.
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

- **VP9 against VP8** (2026-10-04, release build, VPS at load 0.4, 45 frames
  of a synthetic camera-like picture: a moving bright disc on a gradient with
  sensor noise; VP8 at the call's settings, VP9 with no lookahead):

  | 320×240                 | encode   | decode | kbit/s at 15 fps | luma PSNR |
  |-------------------------|----------|--------|------------------|-----------|
  | VP8 (`oxideav-vp8`, q60) | 45.1 ms | 2.1 ms | 421              | 33.8 dB   |
  | VP9 speed 6, crf 32     | 25.2 ms  | 1.6 ms | 182              | 34.4 dB   |
  | VP9 speed 6, crf 40     | 16.0 ms  | 1.3 ms | 84               | 33.8 dB   |

  At 640×480: VP8 169 ms, VP9 speed 6 crf 32 102 ms — neither keeps up at
  15 fps on a CPU, so the call stays 320×240. The call uses crf 34. Synthetic
  content flatters a codec less than it seems to (noise is the hard part, and
  it is there), but a real face over a webcam is the measurement still owed.
  `rusty_vp9` has no "force a keyframe", so a request makes a fresh encoder,
  whose first picture is one.

## Hardware encoding on the phone (2026-10-04)

`platform::hw_video` encodes a call's video with the phone's own codecs,
`MediaCodec` through its C API (`libmediandk`, as the camera uses
`libcamera2ndk`): no Java, no Gradle, no C of ours. It takes the first of: the
system's VP9 encoder *if it is hardware*, the system's VP8 encoder if hardware,
Google's software VP9 (`c2.android.vp9.encoder`, the system's libvpx), else
the pure-Rust encoder. "Hardware" is decided by name: Google's codecs are
`c2.android.*` / `OMX.google.*`. NV12 input is asked for first, I420 second,
and the input format's `stride` / `slice-height` are honoured. Keyframe
requests become `request-sync`; a turn of the phone reopens the codec at the
new size. It sits behind `video::WithFallback`, so its first error hands the
stream to the Rust encoder from a keyframe for the rest of the call.

**A hardware encoder sends 640×480** (`video::HW_WIDTH`/`HW_HEIGHT`), four
times the pixels — the camera's own size, so the phone's picture is only
turned, never shrunk. The encoder says its size (`VideoEncode::picture_size`),
the session publishes it (`Session::send_size`) and the camera is told each
frame (`Camera::set_send_size`), so a hardware encoder that fails mid-call and
hands over to software also shrinks what the camera sends. The bitrate grows
with pixels to the power 0.6: ~575 kbit/s VP9, ~1 Mbit/s VP8 — through a
forwarder that does no congestion control yet, which is the real limit on
going higher. Software encoders (Google's and ours) stay at 320×240.
`CARTO_VIDEO_SIZE=small` keeps hardware at 320×240 too.

What a receiver pays for 640×480, measured in Rust on the VPS: ~7–8 ms to
decode a frame (VP8 or VP9), on the decode thread, plus a 1.2 MB texture
upload per picture at 15 fps on the main thread — fine one-to-one, worth
watching in a group of several senders on a phone.

Only encoding: at 320×240 decoding is ~1.5 ms in Rust, and a hardware decoder
would add a copy out of its buffers for nothing.

**Compiled for aarch64 only, never run.** The emulator here cannot run the
app. The phone's log says what happened: `call video: encoder chosen` (name,
codec, hardware) and `call video: encoding` every 5 s with the mean ms a
picture took. `CARTO_VIDEO_ENCODER=software` skips it for an A/B. Which
encoders a phone has: `adb shell dumpsys media.codec | grep -iE "vp8|vp9"`.

## Where the call's CPU goes (2026-10-04)

An audit of the media path found the main thread doing work for pictures
nobody saw. What changed, so it is not undone:

- **Decoding has its own thread** (`session::VideoDecoder`), which converts
  each picture to RGBA there and keeps only the **newest per person**
  (`Session::take_pictures`). Before, decode ran on the session thread between
  voice frames, and every decoded picture was an event on an unbounded
  channel converted to RGBA on the main thread: a phone back from the
  background converted seconds of pictures in one frame to show the last.
- **Folded to the bar, nothing is decoded or uploaded** (`set_video_wanted`).
  Unfolding asks every sender for a keyframe, since what was skipped is what
  the next delta refers to.
- **Keyframe requests are throttled per sender (500 ms) and deltas are
  skipped until the keyframe arrives**: after a loss every delta failed and
  asked again, fifteen requests a second.
- **The camera skips a frame before converting it, and shrinks before
  turning it** (`platform::camera::to_call_size`): a 30 fps webcam converted
  every picture at full size and dropped half, and the phone turned
  640×480 pixel by pixel before the encoder shrank it.
- Video RTP time is the **capture time**, not "pictures sent / 15", which ran
  behind wall-clock whenever the busy encoder dropped one.
- The session thread sleeps until str0m's next timer, at most 4 ms (was a
  fixed 2 ms: ~500 wakeups a second for nothing).

And on the screen: the timer counts from the answer, not from dialling;
"Reconnecting…" shows while the connection is in its 30 s grace
(`link::Output::Lost`); a remote picture with nothing after it for 3 s goes
back to the person's initial (a camera turned off used to freeze on its last
frame); the in-call buttons carry labels; hanging up says "Call ended · 04:12";
and a private message that fails to send goes back into the message box.

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

The browser's own WebRTC, microphone, camera, Opus and VP9 (`web/webcall.js`,
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

### Browsers sending VP9 (2026-10-17)

The worker seals with a clear prefix whose length depends on the codec (VP8 10
or 3 bytes, VP9 10 or 1), so it must know every outgoing frame's codec, and a
frame sealed with the wrong prefix opens for nobody.

- **Never by reading the frame's bytes.** A VP8 inter frame's first byte holds
  partition-size bits, and about a quarter of them start `0b10`, VP9's frame
  marker (`0x91` even parses as a VP9 profile-2 keyframe). Trying both readings
  is safe only when receiving, where the AES-GCM tag decides.
- **Offer one video codec.** `setCodecPreferences` with only VP9 profile 0 (or
  only VP8), so the answer names one codec before any frame is encoded.
- **Per frame, in order** (`sframe.js`, `frameCodec`): the frame's
  `getMetadata().mimeType` (Chrome 127+, Safari 26; not Firefox), then its
  `payloadType` through the payload-type table the page posts after applying
  each SDP (Chrome 127+, Firefox 145+, Safari 26), then the codec the up
  answer's first video format names, then the codec given at attach.
- **`scalabilityMode: "L1T1"`.** Chrome sends VP9 with temporal layers by
  default (L1T3), and a spatial layer is a separately sealed frame that str0m
  would join to its picture's others: two ciphertexts in one frame, which
  nobody can open. str0m's VP9 depacketizer reads the descriptor's keyframe bit
  (which Chrome writes from the encoder, so sealing does not hide it), and its
  packetizer reads the clear first byte, as for the app's VP9.
- Tests: `webcall.test.ts` (codec choice, SDP tables, `frameCodec`), run with
  `sframe.test.ts` by `deploy-web` since 2026-10-17 — before that nothing ran
  them.

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

### On the chat page (2026-10-10)

The chat page (`crates/atlas/web`, `src/lib/call.ts`) calls with the same
`webcall.js`, and the split is forced by where things can live: media
(`getUserMedia`, `RTCPeerConnection`) exists in a page and not in a worker,
while the keys and the connection live in the chat worker. So the page runs
the call and the worker carries its signalling (`call_join`, `call_answer`,
`call_leave`, `call_decline`) and answers `call_keys` — the chat's 16-byte
SFrame key for its current epoch, from the MLS exporter — which the page asks
for again every second so a commit mid-call moves the key with it. One call at
a time, in the tab that placed or answered it; other tabs only see it ring.
The rules are the app's: a roster without you rings, an empty one is the call
over, a minute without an answer ends a call placed here. `webcall.js` gained
an `onStream` option so a page with its own layout gets the streams instead of
the viewer's elements, and `deploy-web.yml` watches the three files it shares
with the viewer. `chat-browser.ts` checks it end to end: two headless
Chromium with fake devices ring, answer, and each sees the other's video.

**It is drawn as the app's call screen (2026-10-11).** A call coming in is a
dialog over the page — the caller large on a pulsing initial, Decline red,
Video glass and Answer green, round keys with a label under each — and a banner
with the same keys over a call already going on or once folded with the arrow.
The call itself is a panel to the right of the chat, and one button (Swap)
puts it in the middle with the chat as the panel; a phone has room for one, so
there it is the whole screen or a green bar. The keys are `theme::call_button`'s
four tones in CSS, and the app's chat header now uses those keys too instead
of a text button. The people are an **even grid, not a masonry**: every picture
is a camera frame of one shape, so the only decision is the column count
(`gridColumns`: the squarest grid in the middle, one column then two in the
panel), and one other person fills the panel with your own picture in its
corner. `standInClient` lets a preview mount the real page against a stand-in
client with `canvas.captureStream()` cameras — how layouts with several people
were looked at with nobody to call; nothing about a real multi-party call has
run in the browser.

The page makes the app's call sounds too (`src/lib/call-tones.ts`): the
ringtone, the 425 Hz ringback, the call-waiting beep and the join, leave and
end cues, synthesised from the same notes and levels as `call_tones.rs`. A
browser keeps audio suspended until the page has had a click or a key press, so
a call arriving in a tab nobody has touched since it loaded rings silently.

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

## Ringing a closed browser (2026-10-11)

The web chat rings a browser whose chat page is closed through the browser's
own push service (Web Push): `src/lib/web-push.ts` subscribes once
notifications are allowed and registers the address with the same
`SetPushEndpoint` as the app, marked `webpush:` (`simulator::push::WEB_PREFIX`);
`src/sw.js` shows "Incoming call" and opens the chat on a tap, and the chat
then rings with the caller's name as usual. Decisions:

- **Still empty.** No payload, so nothing to encrypt (RFC 8291) and nothing
  for the push service to read. What it learns — Google for Chrome and Edge,
  Mozilla for Firefox, Apple for Safari — is that this browser got a push from
  this server at that moment. Unlike a phone's distributor, a browser's push
  service is not the person's choice and cannot be self-hosted.
- **Signed (VAPID, RFC 8292)** because every browser's push service requires
  it: a P-256 key made on first start and kept as `vapid.key` beside the
  database (`simulator::vapid`), its public half at `GET /api/push/key`. A new
  key would silently unsubscribe every browser, so it is kept, and a page
  holding a subscription made with another key replaces it.
- **The marker rides in the endpoint string**, so neither the message nor the
  table changed and no protocol revision was needed: a server older than this
  refuses a `webpush:` address as "not https", which is the right answer from a
  server that cannot sign. A browser's address cannot be told from a phone's by
  its host — both can be Google's.
- **Registered on every connect** (the server keeps the newest owner of an
  address, so a shared browser rings whoever is signed in), and taken back and
  unsubscribed on sign-out — on the same connection, since the server only
  takes back an address the asking connection registered.
- **`/api/push/key` must be routed to the simulator by Caddy** (the `@webapi`
  list in `/data/stack/Caddyfile`), or the web UI answers it and the page
  quietly never subscribes.

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

## Ringing like a phone (2026-09-30, protocol 31)

A call rings with a synthesised ringtone and an incoming-call card (Decline
red, Answer green); your own ringing plays the 425 Hz ringback and gives up
after 60 s ("No answer"); cues mark joins, departures and the end; a call
that stopped ringing unanswered leaves "Missed call from …". Which tone plays
is `call_tones::wanted_loop`, a pure function of the call state. **Declining is
on the wire** (`CallDecline` → `CallDeclined` to the chat's connected
devices): in a one-to-one chat the caller's call ends at once with
"… declined", in a group it is only said, and the decliner's other devices
stop ringing without counting it as missed. Agents have `decline_call`.

## The call screen, and coming back to it (2026-10-01)

`systems::call_screen` draws calls the way Signal and WhatsApp do: an incoming
call takes the whole screen (Decline red left, Answer green right, at thumb
height); in a call the other person fills the screen — their picture cropped to
fill when it is the screen's way up, letterboxed when not — with your own
picture in a corner, a grid for groups, and the controls on the bottom edge,
fading over a picture after 4 s. The arrow, or Back, folds it to a green bar at
the top so the chats and the map work during a call; it never ends a call.
`--ui callincoming|call|callvideo|callgroup|callfolded` captures each.

Four faults behind "the call flow is broken", each with its cause:

- **Hanging up with the camera on froze the app.** `Session::drop` stopped
  the call thread by closing its command channel and then joined it, but the
  camera thread's `PictureSink` is a second sender, so the channel never
  closed and the main thread waited forever. The session now sends an explicit
  `Stop` and does not join (`a_session_ends_while_a_camera_still_holds_its_sink`).
- **A phone opened from a call's push never rang.** It connected after the
  call started, and the roster was only sent on a change. Registering a
  private-chat device now asks the forwarder for every running call in the
  account's chats (`a_device_that_connects_mid_call_is_rung`).
- **Back on the root screen quit the app**, and on Android quitting ends the
  process, so coming back was a cold start: the map streamed again, the
  session signed in again and the tab was gone. It now moves the task to the
  background (`platform::android::move_task_to_back`), as Android 12+ does for
  a launcher's root activity.
- **Reconnecting opened the wardrobe.** Every resume drops the socket, and
  the Appearance panel's "close on sign-out" was a *toggle*, which opened it
  whenever something else was on screen.

When Android does stop the process (a swipe from recents, a reclaim), Chats or
World and the open chat come back from `user.json` (`UserData::screen`), put
back at each sign-in, including the one after a resume. A ringing call is drawn
over whatever was restored. The map itself still streams again after a real
cold start — from the disk cache, not the network — because the GPU state does
not survive the process.

**Not verified on a device**: the emulator cannot run the app, so the back
gesture, the restore and the call screen on a phone are unwatched.

## A call is a meeting (2026-10-17, protocol 41)

Design: `docs/mockups/meetings/` (v2). Every call has a **host** — the first
person in; when the last host or co-host leaves, whoever has been there
longest takes over (`simulator::calls::succeed`), so a call never loses its
host. Decisions, so they are not undone:

- **What a host decides is carried out at the server, with what it already
  sees.** A held member's sound and a member's screen while sharing is the
  hosts' alone are dropped *at the forwarder* (`Sfu::hold`,
  `Sfu::hold_screen`), not hidden in the other apps, which could be rewritten
  to ignore it. A removed account is refused a join for as long as the call
  lasts; a locked call refuses any account that was not in it, but lets back
  one that was (a dropped connection rejoins). All of it lives in one pure
  function, `simulator::calls::control`, with its unit tests beside it.
- **Hands, held, sharing and muted are in the roster, readable by the
  server.** The mockup drew hands as sealed chat messages; they moved into
  `CallMember` because the server must act on them (Let talk lowers a hand and
  lifts a hold in one step) and a late joiner must see the queue. They say no
  more than who is in the call already does. A device's own *mute* is only
  reported (`CallAction::Muted`), never enforced; a host's mute is `held`.
- **Reactions stay sealed** (`CallReact`, `cartopolis_calls::events`,
  `call-events.js`): HMAC-SHA256 of the call's base key under its own label
  gives an AES-128-GCM key per epoch, so an event key is never a frame key.
  The server relays the bytes and stamps the sender. The browser and native
  sealing are held together by `crates/calls/testdata/event-vectors.json`.
- **The screen is a third send-only track offered at the join**, like the
  camera, so sharing needs no renegotiation; `CallJoin.screen` names its media
  id and the forwarder labels it `Kind::Screen`. It is dropped at the
  forwarder whenever its sender has not said `Sharing(true)`, so a picture sent
  before the roster says so goes nowhere. Receivers key decoders and pictures
  by `(participant, kind)`. The app receives screens; sending one is the
  browser's for now (`getDisplayMedia`), the desktop app and Android
  (MediaProjection) to follow.
- **`CallWatch` lets a receiver say whose cameras it draws**; the forwarder
  sends no camera nobody looks at. Screens are always sent.
- **A refused host action does not end the call.** It comes back as a
  `ServerError` under `CALL_CONTROL_REQUEST`, which both clients show as a
  notice; any other error in the call's request range still ends it.

**On the chat page** (`src/lib/call.ts`, `chat.tsx`'s `callStage`): the stage,
a floating dock, notices with their own button, a people island, Speaker and
Gallery, M / V / H and Space held to talk. Who is talking, and talking into a
muted microphone, are measured on the page from the sound it already plays
(Web Audio analysers; `webcall.js` keeps an unmuted clone of the microphone
for the second), so the server learns nothing from either. **Verified
2026-10-17** by `crates/atlas/web/meeting-browser.ts`: three Chrome for
Testing profiles on mac-2 against a real simulator — host role, a raised hand
and its notice, a sealed reaction opened by another browser, a host's mute
that the held browser cannot undo, Let talk, a shared screen as the stage, end
for everyone. A canvas stands in for `getDisplayMedia`, which a headless
browser has no screen for. Running it took the node: the VPS node has no
browser and its glibc is newer than this container's, so its binaries do not
run here.

What it found: **the page's `WebCall.close()` threw** on every hang-up — with
`onStream` it marked a person's tracks as `true` and `close` then called
`.remove()` on them — so the page never set "The call ended" or any other
reason. Older than the meetings; fixed in `webcall.js`.

## What a meeting leaves in the chat (2026-10-17)

Planning a meeting, its answers, polls and votes, notes taken in the call and
the record afterwards are **sealed chat messages**, not call state: they must
outlast the call, reach people who were not in it, and the server must read
none of them. Each is an ActivityStreams object in `Payload::Object`, read and
written by `cartopolis_core::meeting` (the reference) and the web's
`src/lib/meeting.ts`, both tested against `crates/core/testdata/meeting-vectors.json`.

- **No new `Payload` variant and no new history format.** `Object` already
  carries points; an object that is not a point and reads as a meeting comes
  out of the engine as `Event::Line { object: Some(json), body: caption }`.
  `HistoryItem::point` carries either kind: a build that reads only points
  finds no place in a plan and shows its caption. A build older than this shows
  "(a message this version can't display)", as for any unknown object.
- **Answers and votes name what they answer by its `id`** (`urn:uuid:`), never
  by the chat's log position, because the app keeps no log positions for its
  lines and an id survives a shared history. Per person the newest answer
  counts, so changing one's mind is sending again. They are not drawn as lines.
  Votes are not anonymous: everybody in the chat sees who chose what, and the
  poll says so.
- **A plan and a meet-up are told apart by `location`**: a meet-up (a place
  and a time) stays a `Point`; a plan has `cartopolis:call` and no place.
- **The agenda's position is not a chat message**: a host's device sends it as
  a sealed call event (`{"g": item, "p": plan id, "t": started ms}`, beside
  captions' `{"c": …}`), only a host's is believed, and it is sent again when
  somebody joins. Builds that predate it drop the JSON.
- **The record is written by one device**: the host who ends the call for
  everybody, or the host left last; a host can also post it from the notes
  panel. It gathers the notes and polls posted while the call ran, who was in
  it, and the transcript if captions were on *on that device*. A meeting that
  left none of these posts no record.
- **Reminders are made on the device** (the server cannot read the plan): the
  page reminds ten minutes before a plan the person answered Going or Maybe to,
  while it is open, and the calendar file (`ics`) carries its own alarm. The
  app reminds from its calendar while it runs (`calendar.md`); a plan can
  repeat, and an answer then names its date.

**Verified 2026-10-17** by `meeting-browser.ts` (plan, answer, poll, vote,
a decision in the notes panel, the agenda started by the host and seen by a
member, and the record arriving after End for everyone). It found that a
record card mixing keyed sections and empty holes threw on every redraw, which
froze the page after the call; Mithril refuses that mix.

## Guests, by link (2026-10-18)

Somebody with no account can join one meeting. Decided with the person
steering, between two ways of giving a guest the call's key: **the key is
in the link** — not "add the guest to the chat", which would show a guest
the chat's messages for as long as they were in it.

- **A link is a code and a secret.** A member asks (`CallInvite`); the
  server keeps a 10-character code for 24 hours with the chat it opens
  (`call_links`, migration V37) and answers `CallInvited`. The asking
  device draws a 16-byte secret and makes `https://…/?meet=<code>#<secret>`:
  the part after `#` is never sent to any server. The link is posted in the
  chat, sealed, so **every member's device knows the secret**, and
  whichever host lets the guest in can hand it the key.
- **A guest is a short-lived account that can do one thing.** Its hello is
  `AuthCredential::Guest { code, name }`; the server makes an account
  (`accounts.guest_of`, `guest_expires_at`, 24 hours, swept) whose username
  is the name made unique (`Sanne-Jansen~d27`; pages show "Sanne Jansen
  (guest)"). It may join that chat's call, always through the waiting
  room, and nothing else: `ClientRequest::guest_may` is an exhaustive
  whitelist, the HTTP API refuses a guest's session, and an expired guest
  or a session that outlived its swept account is refused at the hello.
  The browser worker reconnects a guest with the session token, never the
  code again, or each reconnect would make another guest.
- **The key: sealed under the secret, relayed unread.** Once a guest is
  in, a host's device seals the epoch's base key, the members' sender
  indices and the guest's own (`0x8000 | seat`) with AES-128-GCM under
  HMAC-SHA256(secret, "cartopolis guest key"), the chat's id as associated
  data (`cartopolis_calls::events::seal_guest`, `call-events.js`, held
  together by `event-vectors.json`), and sends `CallKeyDrop`; the server
  checks only that it comes from a host and goes to a guest in the same
  call. A new epoch is handed out again. The app's frame keys accept a
  sender index in the guest range; the browser's never checked.
- **What it costs.** The guest holds the epoch's call key. It cannot get
  any media but this call's (the forwarder sends media only to people let
  in), but a later call in the same chat *and epoch* would be openable by
  the guest together with a server that kept the frames. Rotating the epoch
  after a meeting with guests is not built.
- **Guests are browser-only.** The app hosts guests (invites, hands keys)
  but cannot be one.

**Verified 2026-10-18** by `meeting-browser.ts`'s fifth browser: the link,
the door, the waiting room, and video both ways, which only works if the
handed key opens both ways.

**It found an older bug**, from when the join started offering a screen
track: the server dropped any call answer over 16 kB without a word. A
browser's answer to a down offer has a media line per track of everybody
else (about 2.4 kB each), so once a fourth person joined a browser meeting
every answer was dropped and nobody received anything new. The limit is
256 kB now (`calls::MAX_SDP_BYTES`, with the measurement), and a dropped
answer is logged.

## The app shares its screen, and suppresses noise (2026-10-18)

- **Screen** (`platform::screen`, More → Share your screen): Android's
  MediaProjection (`ScreenCapture.java`, with a foreground service of type
  `mediaProjection`, required since API 34, and the system's own dialog
  every time); Core Graphics on macOS; the X11 root window over `x11rb` on
  Linux; GDI on Windows. **Not Wayland**: the screen there comes only
  through the desktop portal and PipeWire, a C library; the app says to
  share from the browser instead. Pictures go out about twice a second on
  the third track every join offers, through a VP9 encoder of their own at
  up to 1280×720. Checked by building for each target and a two-session
  forwarder test; the Windows code only against `windows-sys`' signatures,
  and none of it yet on a device.
- **Noise** (`cartopolis_calls::denoise`, More → Noise suppression, on by
  default): RNNoise's network (`nnnoiseless`, pure Rust) on the
  microphone's own 48 kHz samples, before gain and resampling; 10 ms of
  delay. On flat white noise it took the ordinary background level to 27 %
  of its energy but cut little at much quieter or louder levels — the
  network learned recorded noise.
