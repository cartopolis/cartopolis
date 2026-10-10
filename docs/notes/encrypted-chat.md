---
topic: encrypted-chat
triggers: [e2ee, cartopolis_e2ee, private_chat.rs, "src/e2ee.rs", openmls, MLS, "E2ee", "private chat", "direct message", "key package", "recovery key", V20__private_chats.sql]
updated: 2026-10-10
---

# Private chats are end-to-end encrypted, and the server is only a relay

Direct messages and private groups are sealed with **MLS** (RFC 9420) on the
devices. The simulator stores and delivers the sealed bytes and cannot open
them: it does not link `cartopolis_e2ee`, the only crate that could (the
simulator's *tests* link it, to drive real devices). Design and screens:
`docs/mockups/chat-world/`. The web UI runs the same engine in a worker:
`web-chat.md`.

## What the server can and cannot see

It **cannot** see anything a private chat says: text, and later points, their
pictures and shared real locations. It **can** see who has which devices, who
is in which chat, and when each entry was written and by whom (`e2ee_*` tables,
V20). That metadata is visible to the operator and is not hidden.

Here (nearby) and Global are **not** encrypted (public rooms were removed with
protocol 39, `communities.md`) and cannot be: the
server picks who hears a nearby line by where people stand. The UI must say
which kind a chat is: every private chat's label starts with a padlock
(`private_chat::private_label`, `icons::LOCK`), and nothing else carries one.

## Why openmls with the pure-Rust provider

Measured 2026-09-26: `openmls` 0.9 + `openmls_rust_crypto` builds for
`aarch64-linux-android` (NDK clang only) and for `wasm32-unknown-unknown` with
the web build's flags, with no `cc`-built sources on either. The same constraint
chose rustls over native-tls and ADPCM over Opus. Rejected: libsignal (not on
crates.io, heavy native dependencies, and Signal's protocol has no standard
group layer; MLS does). The web build needs getrandom 0.2's `js` feature for
`rand_core` under the RustCrypto curves (`crates/e2ee/Cargo.toml` says so);
without it the wasm build stops at `getrandom`.

## The rules the whole thing rests on

- **The server orders commits by epoch.** A membership change is accepted only
  if made in the chat's current epoch; the loser of two simultaneous changes
  retries after catching up. Everything else about agreement is MLS's job.
- **A device never reads its own entries back.** openmls does not return one's
  own application message as a message, so clients skip log entries whose
  sender device is themselves and show their own line from their own copy.
  (An early version reported them as "membership changed", which hid this.)
- **Entries are applied strictly in order.** A gap means a line was dropped (the
  text backlog is bounded, `chat::TEXT_BACKLOG`); the client asks again from its
  cursor instead of skipping, because a skipped *commit* breaks the chat for good.
- **Blocks withhold messages, never commits**, for the same reason.
- **A key package opens one invitation.** The server deletes each one it hands
  out; devices keep a stock of 20 and top up on connect or when told they are
  below 5. Claims are throttled per connection so nobody's stock can be drained.

## Keys, backup, trust (decided 2026-09-26)

- **Keys live only on the device**, in the Data namespace of `platform::storage`
  (`e2ee/<server>/<user>.bin`), preloaded on the web because IndexedDB has no
  synchronous read. Losing that blob loses every private chat on the device.
- **History on a new device: encrypted backup unlocked by a recovery key.** Not
  built yet. Until it is, a new device is added to its chats (below) but reads
  only what is said after it was added.
- **Web client: encrypted like the apps**, with a note that a browser runs
  whatever code the server sends it.
- **Identity proofs are out of scope for now.** The server vouches for each
  person's full address (`name@server`, protocol 37, `web-chat.md`) at sign-in
  and nothing checks further; a key-change warning is the only
  defence against a swapped key until proofs or in-person verification exist.

## Every device of every member (protocol 39)

A chat is joined by *devices*, and until 2026-10-10 only the devices that had a
key package when the chat started were ever in it. A phone signed in after a
chat was started on a browser was in none of them: it heard nothing, and
starting the chat again found the existing one and said so. The server cannot
fix that itself, since only a device in the chat can commit somebody in, so it
finds who is missing and asks one that is.

- **The server tracks which devices hold which chat** (`e2ee_group_devices`,
  V30), from what it already sees: the device that registered the chat, the
  devices an invitation names, a device that writes to it, and every cursor a
  device syncs with. The last is what backfilled chats from before the table.
- **After every sync and every accepted commit** it looks for member devices
  not holding the chat (`e2ee::missing_devices`), claims one key package each,
  and sends `E2eeAddDevices` to a device that does hold it: the asking one if
  it can, otherwise any that is connected. Nobody holding it online means it
  waits for the next one to sync.
- **An ask is not repeated for two minutes** (`ASK_AGAIN_MS`), because each
  spends one of the missing device's key packages. Devices not seen for 30
  days are not added: a browser whose storage was cleared would otherwise
  collect a member slot in every chat for ever.
- **A device adds only devices of people already in the chat**
  (`Device::add_member_devices` drops any other key package, and any device
  already in it). The request comes from the server, and this is what keeps it
  from being a way to add a stranger. The server could always hand out a key
  package of its choosing when a chat starts, so this is no weaker than that.
- **A device that joins this way is labelled by who else is in the chat**, not
  by who invited it: when your own laptop adds your phone, the inviter is you.
  A group's name is sent again after the add, as for a newcomer.

## Moderation in encrypted chats

The server-side word filter (`net.rs`'s `ChatSend` arm) and reports that keep
the reported line server-side (`user-safety.md`) cannot work on sealed text.

- **The filter runs on the receiving device** (`private_chat::PrivateChats::filter`,
  `cartopolis_core::Filter::seeded`). It uses the **built-in** word list only: an
  operator's additions are server config and never reach a client.
- **Block and account deletion work** as for the rest of chat (blocks are
  enforced by the server, which sees the sender).
- **A report carries the reported private line from the reporter's device**:
  the name menu on a private line opens the ordinary report with that line
  attached, as it does for nearby chat. The server cannot check it was really
  said. Message franking would make it checkable; not built.

## Points and real-location sharing ride inside the same sealed messages

`cartopolis_e2ee::Payload` is append-only (postcard numbers variants by
position): `Text`, then `Object` (an ActivityStreams JSON object, today always
a point, `cartopolis_core::point`), then `Location` / `LocationStopped`. An
older build meets a newer variant as "a message this version can't display".

- **A point is an ActivityStreams Note with a Place** from day one, so the
  same object can be posted publicly over ActivityPub later without
  converting encrypted points on every phone.
- **A shared real location ends on the devices.** The server cannot read it,
  so it cannot end it: the sender stops at `until_ms`, and every receiver
  drops the position at `until_ms` whatever it last heard. The sender sends on
  the first fix, then at most every 10 s and only after moving 15 m, and at
  least once a minute (`private_chat::share_due`).
- **A share is never on without a sign of it.** While any is live, a chip at
  the top of the map names who can see the phone and has Stop, drawn even in
  driver mode (`place_pins::sharing_chip`).
- **Colours mean one thing each:** a chat's points are the brand orange, a
  person's real location is `theme::REAL_LOCATION` blue.
- **Desktop has no location provider**, so sharing from it says so and stops.

## Groups

A group is created with its members and its name in one go: the creator asks
for every member's key packages at once, creates the chat, adds everybody who
has a device in one commit, and then sends the name as the chat's first sealed
message (`Payload::GroupName`), so the server never learns what a group is
called. Members with no device are named back to the creator as "not added".

Key-package claims are a bucket (`net::CLAIM_BURST`, refilling every
`MIN_CLAIM_INTERVAL`) rather than a fixed gap, because a group claims for every
member at once; past the bucket the server answers `429` with a reason, never
silence, since an empty answer reads as "they have no app that can chat
privately". Adding or removing people after a group starts is not built.
