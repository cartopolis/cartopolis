---
topic: web-client-is-the-world
triggers: [framed_session.rs, world-frame.ts, world-browser.ts, discs.rs, comms, "feature = \"comms\"", "cartopolis.session", "cartopolis.ready", crates/cartopolis/index.html, chat.tsx]
updated: 2026-11-22
---

# On the web the client is only the world; the page does the talking

Decided 2026-11-22 with the owner. The app runs as three things: the native app
(desktop, APK, and later Pinnacle), the HTML site at cartopolis.org, and the
app's own wasm build framed by that site as its World tab. Before this, the wasm
build was the whole app, so in one browser a person had two sign-ins, two
connections and **two chat devices** with their own keys (`web-chat.md`), and
every talking feature existed twice in the browser — once in HTML, once in egui
inside the frame — and calls three times (str0m in the app, `web_call` in the
frame, `lib/call.ts` on the page).

## The split

- **`comms`, a default feature of `cartopolis`**, holds everything that talks
  rather than shows the world: the messenger (`chats`), communities and their
  posts, contacts, the calendar, calls and their screen and tones, chat
  pictures, the camera screen, the sign-in screen and external sign-in, Admin,
  access tokens, the bridges, meet-ups, and the two crates that hold keys
  (`cartopolis_e2ee`, `cartopolis_chat`). The APK names it back explicitly in
  `crates/cartopolis_android/Cargo.toml`, for the reason `audio` gives.
- **The web build turns it off without naming it**: `index.html`'s
  `data-cargo-no-default-features` with `data-cargo-features="audio"`, and the
  wasm checks in `tools/verify-branch` and `ci.yml` the same. So that check *is*
  the no-comms check; nothing else compiles the crate without `comms`, and a
  native build without it is not supported (`mcp` and `midas_link` assume it).
- **The world never reaches into the talking half.** Where a world system read
  a comms resource (the dock's switch, the place card's Share, "is the camera
  open?"), the parameter and its uses are `cfg`'d and the web build reads the
  answer as "closed" or "none". The drawing both halves use — a person's disc,
  the round button — moved to `systems::discs`, which `chats` names back.
- **The panel chain is still one total order.** A `cfg` cannot sit on a tuple
  element, so the comms panels are registered in their own statement inside
  the `panel-chain` markers, each run pinned `.after()`/`.before()` the two
  world neighbours it stood between. `every_root_ui_panel_is_in_the_chain`
  reads both. The same goes for every edge from a world system to a comms one:
  it is written on the comms side (`render_login_screen.before(drive_session)`),
  so the world's registration names nothing that may not exist.

## The page signs the frame in

`framed_session.rs` (wasm only) and `crates/atlas/web/src/lib/world-frame.ts`:
the frame posts `{type: "cartopolis.ready"}` to its parent once it listens; the
page answers `{type: "cartopolis.session", token}` and posts it again on every
sign-in and sign-out. The token is the page's own (`$s.signup.token`), the one
the chat worker says hello with, so the world is the same account with no keys
of its own. Both directions stay on the page's origin. The frame never stores
the token (`persist_token: false`) and **never enters a remembered session**
(`lib.rs`), which could be another account than the page's.

## WebGPU is checked before the frame loads

The web build needs WebGPU (`crates/cartopolis/Cargo.toml` has why WebGL2 is no
fallback), so World asks `navigator.gpu.requestAdapter()` first, and without an
adapter shows what it needs and a link to the app instead of the frame.
Community works in any browser.

`crates/atlas/web/world-browser.ts` drives both against a real simulator, on
a build node with `--chromium /usr/bin/chromium`: a browser without WebGPU
gets the message and no frame; one with SwiftShader's adapter gets the frame,
which logs that it took the page's session, and the simulator authenticates the
account twice (the chat worker and the world). First run 2026-11-22: signed in
5 s after the frame was made. The world then loses its device on SwiftShader,
as every canvas does there (`web-device-loss.md`): a node proves the sign-in,
not the picture.

Size, 2026-11-22, `deploy/build/wasm.sh` on vps-1: the module is 100.3 MB
(15.3 MB as trunk's `.br`), against 108.8 MB (16.5 MB brotli as served) for
the build published that morning, which was cut from an older `main` — so
about 8 MB of the module, not a measurement of `comms` alone.

## Not done yet

- Nothing from a chat is drawn in the frame: no chat points, no shared
  locations, no person markers or balloons, no nearby-chat bubbles. The page
  could post them as it posts places; that is the next seam, not this one.
- Proximity voice is still compiled into the web build but has no control
  there (its button was in the chat overlay).
- The TypeScript copies of rules that also exist in Rust (calendar, meetings,
  rooms, posts) still exist twice.
