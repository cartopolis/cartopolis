---
topic: chat-across-servers
triggers: [crates/simulator/src/remote, remote/calls.rs, "call/join", "call/signal", "accounts.home", V45__people_elsewhere.sql, "/ap/chat", peer_test_bases, chat_elsewhere.rs, "other servers", "full address", local_name, ADDRESS_MARK]
updated: 2026-11-19
---

# Contacts and private chats between servers

Built 2026-11-16: the first part of stage 5 of `docs/mockups/federation/README.md`.
Somebody on one server can look somebody up on another by full address
(`bob@rug.example`), send a contact request, have it accepted, and talk in a
direct private chat, each connected only to their own server. The apps and the
wire did not change (no protocol revision): every identity was already a full
address (protocol 37, `web-chat.md`), so what changed is that the simulator
forwards instead of answering 501.

## Somebody elsewhere is an account here

The rules (contacts, requests, blocks, chat membership, devices) are all keyed
by account, so a person elsewhere is given one, the first time they are named
(`remote::account`, `accounts.home`, V45): username their full address,
`pw_hash` no PHC string, request policy *anyone*. Nothing else needed changing
to hold them. Each server applies a request or an answer to its own copy, so
each enforces its own person's setting and limits: the sender's home its daily
limit, the receiver's home their policy and blocks.

Two consequences to keep:

- **A username made here can no longer hold `@`** (`http::check_username`), or a
  local name could be somebody elsewhere's address. Accounts from before keep
  theirs; `remote::account` refuses to stand in for an address a local account
  already has.
- **Lists of people leave them out by `home IS NULL`**: the admin list, the
  account counts in the stats and nodeinfo. A new query that counts or lists
  "our people" needs the same.

Rejected: separate tables for remote members and contacts. Every rule would have
needed a second path, and a second path is where a block stops holding.

## A chat lives on its hub

A conversation's id names its hub (`address::group_home`), which orders its log
as before. A member elsewhere's home **follows**:

| Call (`POST /ap/chat/…`) | From → to | What |
|---|---|---|
| `person` | asker's home → their home | the person card, answered by their home |
| `contact-request`, `contact-answer` | home → home | applied on both sides; a decline stays silent |
| `keys` | asker's home → their home | key packages, only with a contact or request between them |
| `submit` | member's home → hub | an `E2eeSend` from a device elsewhere; the hub's answer is passed back |
| `deliver`, `invite` | hub → member's home | what the hub appended, for that home's people |
| `sync` | member's home → hub | a device catching up; blocks are filtered at home |

Calls go under `/ap/`, which Caddy already sends to the simulator for the
fediverse, so a deployment needed no proxy change. They are JSON, signed with
the **instance actor's key** the fediverse already has, and a server is believed
only with its own (`key_id_of`: the key id must be `https://<server>/ap/actor#main-key`,
and the people a call names must end in `@<server>`). Blocked servers and the
fediverse's per-server throttle apply. The whole thing is off where the
fediverse is off (a `localhost` development sim): `Link::enabled`.

Delivery from the hub runs in the background and is not retried: a missed
`deliver` is a gap the device fills by `sync`, as it does for any gap. A missed
**`invite` is not recovered** yet; the inviter's device has to add them again.

## Calls (2026-11-19)

A call in a chat runs on **the forwarder of the chat's hub**, for every member
wherever they live (`remote::calls`): one call, one forwarder, nobody's media
relayed twice, and the keys are the chat's as on one server. A device still
talks only to its home; its home forwards what it says about the call:

| Call (`POST /ap/chat/call/…`) | From → to | What |
|---|---|---|
| `join` | member's home → hub | a `CallJoin`, with the full addresses the person blocks; answered with the forwarder's SDP, "waiting", or the refusal |
| `answer`, `leave` | member's home → hub | answers to the forwarder's offers, in order; leaving, joining another call or the connection ending |
| `signal` | hub → member's home | a call message for one device in the call (offers, `CallJoined` on admission, `CallEnded`, reactions), or a roster for the people named — which is what rings them |

The media goes from the device straight to the hub's forwarder: its address is
the one candidate in the forwarder's SDP, so **the hub needs `calls_listen`
and a reachable `calls_public_addr`; the member's home needs neither.**

- **A home accepts from a hub only call messages, only about chats that hub
  is the hub of**, and a device-addressed one only while that device is in one
  of that hub's calls (`call_message`, `Elsewhere::to_device`).
- **Names are rewritten at the home** (`localise`): a roster says `alice` for
  somebody on the home and `bob@rug.example` for everyone else, as the app
  compares a roster with its own bare username to know whether it is ringing.
- **What the hub signals before the join's answer is back is held** until the
  device has its `CallJoined` (`AwayCall::start`, `Elsewhere::release`): an
  offer for the receiving connection must not overtake it.
- The hub posts its signals one at a time, in order, from one task
  (`spawn_relay`): a slow home delays others' signalling, never media.

Not across servers yet: **declining** (the caller elsewhere rings until the
minute is up), **a closed app** of a member elsewhere is not woken (no push
from the hub, none from the home), a device that **comes online mid-call** at
a home is not told of the running call (`Catchup` knows only devices here),
and **hands, host actions, reactions sent, cameras watched, guests** from a
device elsewhere (`CallControl`, `CallReact`, `CallWatch`, `CallInvite`,
`CallKeyDrop` are not forwarded; the defaults — everybody's cameras — hold).
None of it has run between two real servers on the internet: the test runs two
on one machine (`people_on_two_servers_call_each_other_on_the_chats_hub`).

## Not done yet

- **Communities with members elsewhere**, **adding a person elsewhere's new
  device** to an existing chat (`missing_devices` asks only devices here),
  **removing a contact** across servers, presence and "where" of contacts
  elsewhere, and the rest of calls (above). Each is named in the design's
  stage 5.
- **Contact links** are `https://cartopolis.org/go?contact=…` whatever server
  made them (`place_link::contact_link`), so a link from a self-hosted server
  names the wrong one. Typing the full address works.
- **Server identity and discovery** (stage 2) are still not built: a server is
  `https://<its name>`, and its key is the fediverse's instance actor.

## Testing it

`tests/integration/chat_elsewhere.rs` runs two simulators on one machine, named
`localhost` and `127.0.0.1`, with real MLS devices: a refused request, a card,
a request, an acceptance, a direct chat both ways, and a catch-up after being
away. `peer_test_bases` (tests only, read only with `fediverse_allow_private`)
says where each finds the other, since a server's name drops its port.
