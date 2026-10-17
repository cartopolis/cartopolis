---
topic: web-chat
triggers: [cartopolis_chat, cartopolis_web_chat, web_chat, address.rs, server_name, federation, MIMI, "full address", group_id, chat-worker.ts, "src/worker", SharedWorker, "Web Lock", "chat in the web UI", embodied, embodied_position, build-chat-core.ts, chat-e2e.ts, crates/web_chat, crates/chat]
updated: 2026-10-17
---

# Private chats in the web UI run the app's engine, once per browser

The web UI (`crates/atlas/web`) chats through the same engine as the app:
`cartopolis_chat::Engine`, compiled to WebAssembly as `cartopolis_web_chat`
and driven by a worker. Designs: `docs/mockups/workspace/`. Keys and the wire
are `encrypted-chat.md`'s; this note is about the browser half.

The engine was split out of the client's `systems::private_chat` for exactly
this (the client re-exports it), and `cartopolis_web_chat` gives it a
byte-shaped surface: frames in, frames out, events as JSON, state as one blob.
It is built by `build-chat-core.ts`, **not threaded** — unlike the viewer — so it
imports no shared memory and loads in any worker on stable Rust, and the
`wasm-bindgen` on the PATH must be the exact version `Cargo.lock` pins (a
mismatch fails at load, not at build).

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

## Federation: what was fixed in place before it exists

Decided 2026-10-09: channels and direct chats are all end-to-end encrypted,
and the simulator may one day be one node of several. Encryption makes that
easier, because a server only ever relays sealed bytes. The model to grow into
is the IETF's MIMI (chat between providers, on MLS): every conversation has
one **home server** that orders its log, every person has one home server that
holds their key packages and delivers to their devices, and servers forward to
each other. Nothing forwards yet. What could not wait is everything written
into something that outlives a change of mind (protocol 37,
`cartopolis_core::address`):

- **A person is a full address everywhere**: `name@server`, lower-cased. It is
  in each device's MLS credential, which lasts as long as the device, so a bare
  username there could never later be told apart from the same name elsewhere.
  `Device::save`'s format went to version 2, so older devices are refused and
  replaced, and V28 cleared every private chat on the simulator (the one live
  chat was a test, and the user said not to keep it).
- **A conversation's id begins with its home server**: `server/` and sixteen
  random bytes. The simulator registers only ids that name it.
- **Key packages are claimed by full address.** The simulator answers for its
  own people and refuses anybody else's address in words (501), never with the
  empty answer that means "they have no devices".
- **A server's name is the host of its public URL** (`SimConfig::server_name`,
  `cartopolis.org` live, `localhost` for a development sim), sent in
  `ServerHelloAck::server_name`. It must not change once chats exist.

Storage on the simulator is still keyed by local account; addresses are
translated at the wire's edge (`net/e2ee.rs`, `local_name` and `full`). The
clients show a person on their own server by bare name
(`address::display`), so nothing in the app's contact or chat screens changed.

The split with the fediverse stands: ActivityPub is for public posts about
places, and private chats and channels never go over it.

## Communities (protocol 39)

Workspaces and channels (protocol 38) were replaced by communities and their
topics on 2026-10-10: `communities.md` has what they are and how a member is
added to a topic by a device already in it. The page shows them as the app
does: one list, communities with their topics folded under them, then people
and groups, and the community's own pages (find, preview, start, run, edit) in
the middle.

## A message's id is its place in the log

Replies, reactions, edits and deletes (four payloads, appended after
`Picture`) each name another message by its `E2eeDeliver` seq. That is the one
id every member already agrees on, because the server orders the log;
anything the sender minted would need agreeing separately. A device's own
message gets its seq back as `Event::Sent`, matched by the request id the send
answered with.

The engine keeps no history, so it cannot check that an edit or a delete
comes from the person who said the message; the history does
(`worker/history.ts`), and drops one that does not.

**The readable history exists only on the device.** The server keeps sealed
bytes and replays from where the device left off, so the worker keeps every
chat it has read in IndexedDB (`history` store, beside `keys`), up to
`MAX_KEPT` messages per chat. Clearing site data loses it as surely as it
loses the keys.

## Measured

2026-10-09, `web-release` profile, no `wasm-opt`: the module is **2.5 MB,
735 KB gzipped**, and the bundled worker script 15 KB. It is loaded once per
browser session by the worker, not by every page. Neither size optimisation
(`opt-level = "z"`, `wasm-opt`) has been tried yet.

**Connecting overlaps loading** (2026-10-17): `ChatConnection` opens its
socket at once and makes its engine when the module's `ready` resolves,
sending the hello only then, so the WebSocket's handshake and the module's
fetch and compile run together instead of one after the other. A second
`sign_in` for the token already signing in is answered with the state, not a
second Web Lock request (which read as another tab holding the keys).

**Still on the connect path** (measured from the VPS 2026-10-17, so with no
real round trips): the engine makes `KEY_PACKAGE_STOCK` (20) fresh key packages
on *every* connect (`Engine::on_connected`), and openmls keeps every one it
made in the device's saved state, so that state grows by twenty on every page
load and is loaded and written again on every step. Only the difference to
what the server holds is needed (it answers a registration with its count),
but knowing that across a reload means a new field in `SavedEngine`, which is
postcard and not self-describing: a format change to the one blob whose loss
orphans every chat. Not done yet.

`chat-e2e.ts` is the test that the pieces fit: two `ChatConnection`s in Bun,
the real module, a real simulator over its WebSocket, a direct chat both ways,
and one side coming back from its saved state to read what it missed.

`chat-browser.ts` drives the page itself in two headless Chromium profiles
(two people): sign-in through the form, a community started with a topic and
the other person in it, added by the first browser, a line each way with a mention and a reaction, and a reload
that reads the history back out of IndexedDB. It is what exercises the
`SharedWorker`, the Web Lock and the storage, which nothing else does.

```bash
bun run build:chat-core --dev && bun run build   # in crates/atlas/web, in a build slot
bun chat-e2e.ts --sim "$CARGO_TARGET_DIR/debug/simulator"
bun chat-browser.ts --sim "$CARGO_TARGET_DIR/debug/simulator" --shots /tmp/chat
```

`deploy-web.yml` builds the module with nightly (the CI image carries the
wasm32 target only there) and fails the deploy if it cannot: the page is
nothing without it.

## Not done yet

- **Joining with nobody online**: a newcomer's history comes from a device
  that is (`docs/notes/communities.md`, "History for people added later").
- **A recovery key.** A person who loses every device loses every chat.
- **World.** The mockups' Chat | World switch with the viewer embedded in the
  page; the viewer would then have to use this worker's connection, or it is
  a second device in the same browser.
- **Search** (on the device, over its own history), **pictures and files**, and
  **notifications when no tab is open** (Web Push).
- **The app knows nothing of workspaces**: a channel it is in shows as an
  ordinary group chat, and replies show as plain lines.
- **The chat is drawn in the site's component library and its tokens**
  (`@garage44/mithril/components`), so it follows the light/dark switch like
  every other page. The halfway palette of `docs/mockups/workspace/` was
  dropped with it (2026-10-10); if the site's colours move towards the app's,
  that is a change to the library's tokens, not to this page.
