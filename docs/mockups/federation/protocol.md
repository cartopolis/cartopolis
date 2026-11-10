# Protocol: documents, messages, routes and tables

The detail behind [README.md](README.md). Names are proposals; protocol rows
and migration numbers are claimed when a stage lands (`midas reserve`), never
here. Server-to-server calls are HTTPS through `fetch::Client`, signed the
way the fediverse signs today (HTTP signatures with the server's instance key
in `fedi_keys`), and refused for a server on the block list. Bodies are JSON;
the peer stream is binary.

## 1. The server document

`GET https://<server>/.well-known/cartopolis`, public, cached an hour.

| field | meaning |
|---|---|
| `server` | the DNS name, lower-case |
| `software` | name, version, wire protocol version, peer protocol version |
| `keys` | the instance actor's key the fediverse already has; a second one while rotating |
| `endpoints` | `ws`, `api`, `calls` (the `calls_public_addr`), `federation`, `stream` |
| `layer` | `name`, `about`, `picture`, `data` (the map sources it uses, for information) |
| `peering` | `open`, `listed` or `closed`; `shares_offered` by default (`people`, `things`, `chat`, `voice`); `networks`: the network lists it belongs to |
| `presence` | optional: `updated`, `people`, and `groups`: clusters of people within reach of each other, a count and a centre rounded to 100 m, published only at or above the statistics' `MIN_PEOPLE`, a minute late |

## 2. Network lists

`GET https://<curator>/.well-known/cartopolis-network`, public:

| field | meaning |
|---|---|
| `name`, `about`, `curator` | who keeps it |
| `members` | server names |
| `shares` | what members agree to share with each other |
| `updated` | |

A server imports one by URL (`networks` table: `url`, `name`, `curator`,
`fetched_at`, `members`), re-read daily as block lists are. A `listed` server
peers with the union of its own entries and its networks' members; an
`open` server may import one too, to seed peerings and to show the network's
name beside a peer.

## 3. The servers and peers a server knows

`servers` widens `fedi_servers`: `name`, `software`, `version`, `protocol`,
`peer_protocol`, the raw `document` and `fetched_at`, `key_pem`, `key_id`,
`source` (`fediverse`, `operator`, `network`, `link`), `first_seen`,
`last_seen`, `last_ok`, `listed` (an operator's own entry).

`peers`: `name`, `state` (`asked`, `offered`, `peered`, `ended`), `since`,
`shares_out` and `shares_in` (bit sets of people, things, chat, voice),
`trust` (`full`, `things`, `people`, `none`), `budget_things_near`,
`budget_updates_hz`, `budget_model_bytes`, `rate_hz` (what we asked them to
send at), `lag_ms`, `dropped_until`, `note`.

Blocks stay in `fedi_server_blocks` and apply to peering, forwarding and
discovery alike: one list.

`GET /api/servers?near=lat,lng&radius_km=5` (session required) answers the
known servers with presence near there, with name, picture, whether peered
and its policy. `GET /api/servers` lists all for Admin › Network.

## 4. Peering

| route | called by | does |
|---|---|---|
| `POST /federation/peers` | the asking server | `{ shares_offered, networks }`; answered `peered` with `shares_in` at once by an open server, `asked` by a listed one, 403 by a closed one |
| `POST /federation/peers/{name}/answer` | the asked server, later | `{ state, shares }` after an operator's yes or no |
| `PATCH /federation/peers/{name}` | either | narrow or widen shares; the other side may refuse a widening |
| `DELETE /federation/peers/{name}` | either | ends it; the stream closes; the cached things are kept for a day in case it comes back, then dropped |

Shares are per direction: `shares_out` is what we send, `shares_in` what we
accept, and the stream carries only the intersection.

## 5. The stream

`GET wss://<peer>/federation/stream`, the upgrade request signed; one per
pair, opened by whichever side has something to send and kept by both. Frames
are length-delimited postcard `PeerMessage`s with their own version in the
document (`peer_protocol`), independent of the client wire:

| message | direction | content |
|---|---|---|
| `Interest { areas }` | both, every few seconds | merged circles (centre, radius) where this side's people are; the other side sends only inside them |
| `Avatars { tick_ms, states }` | both, once per tick | `{ address, geo, alt, heading, velocity, emote, expression, seated }` for people inside the other's areas; the look is sent once per person per stream (`Look`) |
| `Things { upserts, removals }` | both | `PrimDto` with `server` and owner address, inside the other's areas; batched, at most `rate_hz` frames a second |
| `Motion { moves }` | both | physical things in motion, at a reduced rate |
| `Say { address, geo, radius, text }` | both | a nearby line said inside the other's areas; delivered under the receiver's blocks and filter |
| `Voice { address, geo, frame }` | both, if shared | proximity voice, the existing codec frame |
| `Touch { address, thing, point }` | both | one of this side's people touched the other's thing; the owner's script runs there |
| `Gone { address }` | both | the person signed out, or was deleted (home to the world) |
| `Rate { hz }` | both | a request to the other side to send `Avatars` at this rate, when behind |

Back-pressure: each side reads its peer's frames on a task of its own into
the spatial index under short locks; the queue is bounded (`PEER_QUEUE`) and
drops the oldest; a peer whose frames arrive older than `PEER_STALE_MS` (1 s)
is asked for a lower `Rate`, and one over `PEER_DROP_MS` (5 s) for longer
than a minute is dropped for `PEER_DROP_FOR` (10 min) and the operator told.
Freshness travels to the client as an age on the avatar state.

## 6. Things and models

- `GET /federation/things?near=lat,lng&radius_m=…` (signed; the asker must
  be a peer with `things` in) answers the things inside, with an `etag`;
  the home keeps them in `peer_things(server, id, dto, seen_at)` and the
  stream keeps them current. A thing a peer stops sending for a day is
  dropped from the cache.
- A model is fetched by the home from the owner's `api + /api/assets/{hash}.glb`
  the first time one of its clients draws a thing wearing it, checked
  against its hash and against the upload limits as if uploaded here, and
  stored under `asset_claims` with `origin` = the server and the uploader's
  address; its licence comes from `GET owner/api/assets/{hash}/claim?by=…`
  and `other` or none makes the thing a placeholder. The home's own
  `/api/assets/{hash}.glb` serves it to its clients from then on, so a peer
  never sees a client.
- Media on a thing (video, sound URLs) is forwarded to clients only from a
  peer at `full` trust; otherwise the thing arrives without it.
- Budgets: things from one peer within one reach of any point are capped at
  `budget_things_near`; past it the furthest are left out; `Things` frames
  past `budget_updates_hz` are coalesced.

## 7. The client's wire

- `PrimDto` gains `server: Option<String>` and its owner becomes a full
  address; the edit path refuses a thing whose `server` is set before it
  asks anybody.
- `AvatarStateDto` gains `server: Option<String>` and `age_ms` for peers'
  avatars; `LookupPerson` answers a full address.
- `SetLayers { hidden: Vec<String> }`: the home stops sending those servers'
  avatars and things to this connection.
- Blocks, contacts and reports carry full addresses. Local ones are
  qualified with `server_name` in the same migration that changes the
  columns (`blocks.blocked_username`, `reports.subject`, `prims.owner`).
- The absolute avatar position and the reach messages, §9.

## 8. Safety across peers

- Both servers enforce a block in their fan-out: the home drops a blocked
  address's avatar, lines and voice before they reach the blocker; the
  peer does the same with what the blocker sends. The block itself stays at
  the blocker's home; the peer learns it as `Block { by, whom }` on the
  stream and keeps it in memory for the peering.
- A report about somebody on a peer is stored here (`reports.subject` = the
  address) and sent `POST home/federation/reports`, which files it with an
  `origin`. A server may ban an address from its layer (`peer_bans`) or
  end the peering.
- Account deletion sends `Gone` to every peer and
  `POST peer/federation/people/{address}/gone`, which anonymises the
  address in reports and history as deletion does locally.

## 9. Interest by reach

In order, each its own protocol row with its measurement:

1. **Absolute position**: `AvatarStateDto` gains `geo` and `alt_m`;
   `ClientIntent.geo` becomes required for an embodied avatar; the sim
   integrates each avatar in a local metre frame about its own position and
   clients convert into their own frame as they do for prims. Here chat and
   voice measure in geo.
2. **Reach**: the server keeps a spatial index of avatars and things by place
   on Earth, home and peers alike, each entry with its server and freshness
   (an R-tree or a sorted geohash, whichever measures fastest; never on the
   wire, never in the database). A connection's reach is a radius about its
   person, `INTEREST_RADIUS_M` (the shout radius, 300 m, plus a margin),
   widened and thinned for a camera high up as the map's own streaming is
   by altitude. The merged reaches of a server's people are the `Interest`
   areas it sends its peers. Nothing is cut into cells.
3. **Per-connection frames**: each avatar's state is encoded once per tick
   (`Bytes`, as today) and a connection's `WorldSnapshot` is assembled from
   the states within its reach, minus hidden layers. A thing that comes
   within reach arrives as a `PrimUpsert`; one that leaves it is forgotten
   with `PrimOutOfReach`. `note_meetings` and the nearby scans ask the same
   index. `max_prims` stays the world's ceiling, joined by a density
   ceiling: at most `MAX_PRIMS_NEAR` within one reach of any point, checked
   at placement against the index.

## 10. Chat across servers

MIMI's shape over the same signed HTTP, independent of peering. A
conversation's hub is the server its id names (`address::group_home`); a
person's home holds their devices and key packages.

| route | on | called by | does |
|---|---|---|---|
| `POST /federation/chat/{group}/submit` | hub | a follower, for one device | what `E2eeSend` and commits do over the wire: order, check membership by address and blocks, append, notify |
| `POST /federation/chat/{group}/sync` | hub | a follower | the log from a sequence, for a device that fell behind |
| `POST /federation/chat/{group}/members` | hub | a follower, for a member device | what `TopicAddPeople` and invitations do; the Welcome is routed to the newcomer's home |
| `POST /federation/keys/{address}` | home | any server | key packages for that person's devices; today's 501 becomes an answer, throttled per calling server |
| `POST /federation/chat/{group}/notify` | home | the hub | entries for the devices here that hold the group, through the `ChatHub` |
| `POST /federation/contacts/request`, `answer` | home | the other home | a contact request and its answer; `RequestPolicy` gains *people on my server* and *people in my network* |
| `POST /federation/presence` | home | a contact's home, batched | online and whereabouts, only for people who show it to that contact |

A follower keeps `remote_groups(group, hub, cursor)`; the hub keeps its
local tables with addresses translated at the edge, as `net/e2ee.rs` does.
The "missing device" and "fell behind" repairs run on the hub, which asks a
holding device through its home's notify.

**Calls**: a `CallJoin` on a conversation hosted elsewhere is forwarded by
home to `POST hub/federation/calls/{group}/join`; the answer comes back the
same way; media goes from the client to the hub's `calls` address. Keys are
the group's, unchanged.

**Communities**: the community's server is the hub of every topic and its
member list the participant list (`CommunityDto.home` already names it). A
member elsewhere joins through their home,
`POST hub/federation/communities/{id}/join`, under the same join rules.
