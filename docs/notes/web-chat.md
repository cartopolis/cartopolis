---
topic: web-chat
triggers: [cartopolis_chat, cartopolis_web_chat, web_chat, chat-worker.ts, "src/worker", SharedWorker, "Web Lock", "chat in the web UI", embodied, embodied_position, build-chat-core.ts, chat-e2e.ts]
updated: 2026-10-09
---

# Private chats in the web UI run the app's engine, once per browser

The web UI (`crates/atlas/web`) chats through the same engine as the app:
`cartopolis_chat::Engine`, compiled to WebAssembly as `cartopolis_web_chat`
and driven by a worker. Designs: `docs/mockups/workspace/`. Keys and the wire
are `encrypted-chat.md`'s; this note is about the browser half.

## One owner per browser, enforced by a lock

A device's state (its private keys and every chat's ratchet position) is one
blob. Two copies of it advancing separately and saving over each other break
every group they are in, with nothing to say so. So:

- **The engine runs in a `SharedWorker`**, so every tab talks to one instance,
  and **in a dedicated `Worker` where there is none** (Chrome on Android).
- **It takes the Web Lock `cartopolis-chat-keys` before opening the keys.** The
  lock is the actual guarantee; the SharedWorker is only the common case of it.
  A deploy changes the worker's URL, and a tab still running the old script and
  a tab opened since would otherwise be two shared workers holding one device.
- **The state is written before the frames a step produced are sent**
  (`connection.ts`). If a commit reached the server and the new state never
  reached storage, the next load is an epoch behind everybody, which cannot be
  repaired.
- **A saved blob that will not load is an error, never a fresh device.**
  Starting over silently would orphan every chat the blob was in.

The keys are in IndexedDB database `cartopolis-chat`, key
`web-chat/<host>/<user>` (`keystore.ts`). That is deliberately not the viewer's
`e2ee/…` key: until the page and the embedded viewer share one connection they
are **two devices**, and two devices must never share one state. The page asks
`navigator.storage.persist()`; a worker cannot, the method exists only on a
window.

## A connection that only chats has no body

Every connection gets an avatar at the handshake. One the web UI opens never
moves, and before 2026-10-09 it would have stood at the frame origin in every
client's snapshot and heard every nearby line said there. Now an avatar is
**embodied** by its client's first intent or teleport
(`AvatarSim::embodied`): until then it is in no snapshot, and
`embodied_position` (what nearby chat, voice and prim speech deliver by)
returns nothing for it. The app sends its first intent on its first online
frame, so for the app this lasts a frame. This was chosen over a hello flag
because it needs no protocol revision: the version check is exact, and a bump
means the sim, the APK and the web viewer all deploy together. The worker also
sends `SetPresence { active: false }`, so it is not sent world snapshots.

## Measured

2026-10-09, `web-release` profile, no `wasm-opt`: the module is **2.5 MB,
735 KB gzipped**, and the bundled worker script 15 KB. It is loaded once per
browser session by the worker, not by every page. Neither size optimisation
(`opt-level = "z"`, `wasm-opt`) has been tried yet.

`chat-e2e.ts` is the test that the pieces fit: two `ChatConnection`s in Bun,
the real module, a real simulator over its WebSocket, a direct chat both ways,
and one side coming back from its saved state to read what it missed.

```bash
bun run build:chat-core --dev        # in crates/atlas/web, inside a build slot
bun chat-e2e.ts --sim "$CARGO_TARGET_DIR/debug/simulator"
```

## Not done yet

- **No page uses it.** `lib/chat.ts` is the client a page would hold; the chat
  screens in the mockups are the next step.
- **Not run in a browser.** The engine, the wire and the persistence order are
  exercised from Bun; the `SharedWorker`, the Web Lock and IndexedDB have only
  been type-checked.
- **The deploy may skip it.** `deploy-web.yml` builds the module with
  `continue-on-error`, because it depends on the CI image's `wasm-bindgen`
  matching `Cargo.lock` exactly; make the step required when a page needs it.
- **Calls.** The browser's call worker (`crates/cartopolis/web/call-worker.js`)
  needs the chat's call key per epoch (`Device::call_epoch`), which this module
  does not export yet.
