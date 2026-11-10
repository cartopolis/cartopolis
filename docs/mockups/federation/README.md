---
topic: federation
triggers: [federation, hypergrid, "home server", peering, "network of servers", address.rs, server_name, "crates/simulator/src/fediverse", chat.rs, ChatHub, MIMI, well-known, layers, "interest management", "other servers", "multi-sim"]
updated: 2026-11-11
---

# A network of servers: design

A design, not code. Nothing below exists in the app unless it says *built*.
[research.md](research.md) is what was read first: OpenSimulator's Hypergrid
and what broke in it, how one world has been scaled elsewhere, and the
federated protocols this borrows from. [protocol.md](protocol.md) has the
documents, messages, routes and tables in detail.

## In one paragraph

Every Cartopolis server is a **home** for its people and a **layer** on the
one Earth: its people, its things, its crowd. Servers that **peer** mix their
layers: a person, connected only to their own server, sees at any place the
things and the people of every server in their network, and where their
avatar stands plays no part in which servers those are. The home is the
**gateway**: it fetches and filters what its peers share, under budgets and
trust levels of its own, so a slow or careless server can only degrade its
own layer, never the home's. **Networks** are made by peering policy: open
(any server not blocked), listed (only these) or closed, and a shared list
makes a named network a server can join. Inside a server each person
receives what is **within reach**, from one spatial index that holds home
and peer entities alike, so nothing is cut into cells and no border is ever
crossed.

![The model](v1-model.png)

## What is kept from the Hypergrid, and where this differs

Kept: a home that vouches for its people; home services that stay home; a
server's right to decide whom it mixes with, in both directions; and the
rule that what crosses between servers is limited, so a bad neighbour can
only harm what it was given.

Different, each because of a failure recorded in [research.md](research.md),
or because the person steering asked for it:

| Hypergrid | Here | Why |
|---|---|---|
| To see another grid's content you teleport there; two grids' content never shows in one view | **Layers mix in one view.** You see the network's things and people wherever you are, without leaving your server | Switching servers to see more was rejected as the base |
| A hyperlink is a region at coordinates on a global map; grids are linked by hand; the 4096 bug | A place is a place. Servers link by **peering**, and a link's host names a server, not a destination to travel to | There are no regions |
| The visited grid holds the visitor's session id, which works against home services | **No server but home ever speaks to the client.** Peers speak to the home over a signed stream | The broken trust model |
| Assets are protected by an unguessable id and copied into the foreign asset store | A model is fetched by the home from its owner's server, under a licence that allows it, checked against its hash, cached once, with **provenance** | Leakage and laundering |
| Friendships made abroad are confirmed when you get home | Contact requests go **home to home**, signed, like any other federated message | The dance was a workaround for a missing channel |
| Teleports and region crossings between simulators | **Nothing to cross.** The network is in view everywhere; moving between servers is not a thing a person does | Seams are where worlds break |

## The model

### A server is a home and a layer

| The **home** keeps | The **layer** is |
|---|---|
| the account, its devices and keys, tokens, settings | its people's avatars, wherever they stand |
| private chats and communities (as hub or follower) | its things: prims, models, scripts and their effects |
| contacts, blocks, reports made by its people | its Here and Everyone chat, its proximity voice |
| inventory and uploaded models | its looks |
| push addresses, the calendar, plans | what it shares with each peer, and what it takes |
| public posts (ActivityPub, *built*) | |

### One Earth, many layers, one view

Nobody owns a place. Two servers that peer show each other's layers to
their people: on the Grote Markt a person on `cartopolis.org` sees the
market their own server holds, the student stand `rug.example` put there,
and the students standing at it, all at once. The avatar is always on its
home server; where it stands decides what is within reach, never which
server it is on. A person may hide any layer but their own, as they hide a
map layer today.

What two layers do at the same spot: both are drawn. Things from two servers
may overlap, as two people's prims on one server may today; there is no
arbitration, and a server that finds a peer's layer in the way lowers its
trust or hides it. One authority per thing: a thing is simulated and
scripted only by the server it lives on, and avatars from other servers are
seen there, not felt by its physics.

### Peering makes a network

Peering is two servers agreeing to mix, with what each shares in each
direction: **people** (avatars, looks), **things** (prims, their changes),
**nearby chat**, **voice**. It is asked and accepted, as a follow is, and
either side ends it. Each server's **peering policy** is:

- **open**: it peers with any server that asks and is not on its block
  list, and asks any server its people come across (a place link, a
  community, a contact elsewhere). The fediverse's default, and
  `cartopolis.org`'s.
- **listed**: only servers on its list, in or out. A municipality and its
  schools; a consortium.
- **closed**: no peers. It still publishes its document and still federates
  chat and posts if it wants to; its layer is its own.

A **network list** is a document a curator publishes (`/.well-known/cartopolis-network`:
a name, the members, the shares they agree on) and a server imports, as
block lists are imported today; a listed server's list can be one or more of
these plus its own entries. That is how a network is made inclusive or
exclusive: an open server is in the inclusive network by default, a listed
server is in exactly the networks its lists name, and a curator decides who
is on theirs.

Shares default by policy and are set per peer by the operator (Admin ›
Network): open peering shares people, things and nearby chat and not voice;
a listed peer whatever the list says; and every share can be narrowed to a
**trust level** of `full`, `things only`, `people only` or `none` without
ending the peering.

### The home is the gateway

The answer to "content and latency we do not control" is that the home
stands between its people and every other server:

- **Only the home talks to the client.** One connection, as today. A peer
  never learns a client's address and never sends it a byte.
- **Peer traffic never touches the tick.** Each peer stream is read by a
  task of its own into the spatial index under short locks; its queue is
  bounded and drops the oldest; the tick reads what is there. A peer that
  falls behind is told to send at a lower rate, and its avatars carry a
  freshness: older than a second they stand still, older than five they
  fade, and the client shows why.
- **Budgets per peer**: things within one reach, updates a second, model
  bytes on disk, voice on or off. A peer over budget is thinned, not
  waited for; a peer far over is dropped for a while and the operator told.
- **Content passes the home's own limits.** A peer's model is fetched once
  by the home from its owner, checked against its hash and the upload
  limits as if uploaded here, and served to the home's clients from the
  home's cache. Media URLs (video on a prim, sounds) come only from peers at
  `full` trust. Scripts never run at the home; their effects arrive as
  thing updates, rate-limited like any other.
- **Safety keeps its rule**: the server refuses, the client only draws. A
  block names a full address and both servers enforce it in their fan-out;
  a report about somebody elsewhere is stored here and forwarded to their
  home; the word filter runs on every nearby line the home delivers,
  whichever server it came from; a whole layer can be dropped at once.

## What two peers exchange

![Peering](v1-peering.png)

1. **The handshake.** A asks B to peer (`POST B/federation/peers`, signed
   with A's instance key, A's shares offered). B answers by its policy: at
   once when open, after an operator's yes when listed, never when closed.
   Both record the peering and the shares each way.
2. **One stream per pair** (`wss://B/federation/stream`, signed at the
   upgrade), carrying small binary frames both ways.
3. **Interest areas.** Every few seconds each side sends the merged circles
   where its people are, each a reach wide. The other side sends only what
   falls inside them: the avatar states of its people there, batched once
   per tick; changes to its things there; nearby chat said there; voice if
   shared; and its people's touches on the other's things.
4. **Things come once.** When an interest area opens, the things already
   inside it are fetched (`GET B/federation/things?near=…`), cached, and
   kept current by the stream. Models are fetched by hash when first drawn,
   under the licence gate, and cached for good.
5. **Interactions cross with an address.** Touching a peer's thing sends
   `Touch` to its server with the toucher's full address; the owner's script
   runs there; what it changes comes back as updates. A nearby line is
   forwarded to the peers whose people stand within its radius, and each
   server delivers it under its own blocks and filter.
6. **Nothing is owned twice.** A thing lives on one server; an avatar lives
   on one server; a peer's copy is a view with a freshness, never a second
   authority.

![The home composes the view](v1-compose.png)

## The client

One connection, as today, and little else changes on the wire:

- A thing says which server it lives on (`PrimDto::server`, `None` at home)
  and who owns it as a full address; the thing card shows both, and the
  edit controls appear only for your own.
- Looking up an avatar answers a full address, shown bare for your own
  server and in full for others (`address::display`, *built*).
- The layers panel gains a row per peer server, with its name and picture
  from its document, to hide or show; hidden layers are not sent
  (`SetLayers { hidden }`), not merely not drawn.
- A faded avatar or thing says *rug.example is slow*; a dropped layer says
  so in the panel.
- A place link whose host is a peer opens the place with that layer shown;
  one whose host is not says *not in your network*, with the server's name
  and picture from its document, and an operator's way to ask for peering.
  Community and contact links name the server the request goes to.

Nothing moves the avatar between servers, so the status line, the sign-in
card and the session code stay as they are.

## Chat, contacts and calls across servers

What `docs/notes/web-chat.md` fixed in place becomes a protocol, shaped by
MIMI and spoken over the same signed server-to-server HTTP:

- **A conversation's hub is the server its id names** (`server/…`). The hub
  orders the log, checks membership by address, enforces blocks and fans
  out. A member's home **follows**: it forwards what its devices send to the
  hub and delivers what the hub sends to its devices, so a device never
  connects anywhere but home.
- **A person's home holds their key packages.** Claiming them for someone
  elsewhere is a signed `POST home/federation/keys/{address}`; the 501 the
  simulator answers today becomes an answer.
- **A community's topics are conversations with the community's server as
  hub**; the member list the server already holds is the participant list.
  Public posts stay ActivityPub, unchanged.
- **Contact requests go home to home**; "near you" now counts people from
  peers too, because the home sees them.
- **Presence of contacts** (online, and whereabouts for those who share it)
  is asked home to home, with today's `show_online` consent.
- **Calls**: the SFU is the conversation's hub; the client sends its media to
  the hub's calls address, published in its document. Keys are unchanged:
  the chat's MLS group.

Chat federation does not need peering: two closed servers' people can still
message each other if both allow chat federation, as two Mastodon servers
exchange posts without sharing a timeline.

## Content across servers

A thing lives where its maker's account is. Building happens on your own
server, in your own layer, and everyone peered sees it; there is no building
on somebody else's server, and no copying of ownership. The licence asked at
upload decides whether a model is shown to peers at all (`own`, `cc0`,
`cc-by` are; `other` is drawn as a placeholder elsewhere), so no new
permission is added. The operator's `/api/content/export` and `import`
remain the way to move content between servers, provenance kept.

## Finding other servers

No directory service. Each server publishes `https://<server>/.well-known/cartopolis`:
its name, software, key, endpoints, policy, the networks it belongs to, a
description and picture, and, if it chooses, where its people are, as groups
of people within reach of each other (a count and a centre rounded to a
hundred metres), published only above the minimum the usage statistics
apply and a minute late. Each server keeps a `servers` table (the
fediverse's `fedi_servers`, widened) of every server it has met: through the
fediverse, typed by an operator, named in a network list, or met through a
link. The client asks home `GET /api/servers?near=lat,lng` and the macro
view shows chips like *12 people on rug.example here*, which for a peer is
already in view and for a stranger is a reason to peer.

## One world, more people

Federation adds layers; it does not make one server bigger. Two steps do,
each a landing on its own, and both are owed whatever happens to federation.
Neither cuts the world into anything: there are no regions, no cells and no
squares in this design, on the wire or in the database.

1. **The avatar's authoritative position becomes a place on Earth.** Today it
   is metres in its own client's map frame, and Here chat compares frames
   that differ (`research.md`, §4). The client already sends `geo` with every
   intent; the server holds lat, lng, altitude and heading, and each client
   converts into its own frame, as it does for prims.
2. **Each person receives what is within reach.** The server keeps a spatial
   index of avatars and things by their place on Earth (an R-tree or a
   sorted geohash: a server detail, never on the wire), and once peering
   exists the peers' avatars and things sit in the same index with their
   server and freshness on them. Each tick, each avatar's state is encoded
   once, as today, and a connection's snapshot is assembled from the states
   within a radius of its person, the shout radius and a margin, widened
   and thinned for a camera high up the way the map already streams by
   altitude. A thing that comes within reach arrives; one that leaves it is
   forgotten. Here chat, voice and "who was near whom" ask the same index
   instead of scanning everyone. The quadratic cost DSG measured becomes
   linear in what a person can see, and reaches overlap freely because no
   reach owns anything.

   ![Interest by reach](v1-interest.png)

Splitting one world over several machines is **not part of this design**.
The hosted server runs one process with room to spare
(`docs/notes/simulator-tick-budget.md`), and should it ever outgrow one, the
split is by load and by what is near what, decided then and measured then,
not by squares decided now.

## What changes where

| Where | What |
|---|---|
| Wire (`PROTOCOL_HISTORY`, one row each) | `PrimDto::server` and owner as address; lookups answer addresses; `SetLayers`; a thing leaving reach; freshness on peer avatars; blocks, contacts and reports by full address; the absolute avatar position |
| HTTP, player-facing (Caddy entries needed) | `GET /api/servers`; `GET /api/assets/{hash}.glb` serving cached peer models |
| HTTP, server-to-server (signed, new `/federation/*`) | `/.well-known/cartopolis`, `/.well-known/cartopolis-network`; peers (ask, answer, end); the stream; things; reports; keys, submit, notify (chat); contacts; presence |
| Schema (migrations) | `servers`, `peers`, `peer_things` (the cache), `networks`; `blocks`, `reports` and `prims.owner` by address; `asset_claims.origin` |
| Client | layers panel rows per peer; thing card with server and owner; faded and dropped layers; link hosts honoured |
| Web UI | Admin › Network: policy, lists and networks, peers with their shares, trust and budgets, lag per peer |
| Config | `federation.peering` (`open`, `listed`, `closed`), lists, network list URLs, default shares, budgets, `peer_rate_hz` |

## Stages

Each lands alone and is useful alone; the order is by what each needs.

1. **Absolute avatar positions and interest by reach** on one server. No
   federation yet; fixes Here chat's frames and removes the broadcast.
2. **Server identity and discovery**: the two well-known documents, the
   `servers` table, signed server-to-server calls on the fediverse's signing
   code, `/api/servers`, the chips on the map. Still nothing mixes.
3. **Peering, things first**: the handshake, the stream, interest areas,
   things fetched and kept current, models through the home's cache, the
   layers panel, budgets and freshness. Two servers' content in one view.
4. **People**: avatars and looks over the stream, nearby chat forwarded,
   touches with an address, blocks and reports by address, "near you"
   across peers.
5. **Chat across servers**: key packages, hub and follower forwarding,
   communities with members elsewhere, contacts and presence home to home,
   calls at the hub.
6. **Voice across peers**, if wanted after measuring stage 4's bandwidth.

## Rejected

| Instead of this | Considered | Why not |
|---|---|---|
| Layers mixed in one view | **Visiting** (Hypergrid): co-presence per server, a signed ticket to be a guest elsewhere, a home link and a world link | You had to switch servers to see more; rejected by the person steering as the base. The first draft of this design was this |
| Nobody owns a place | **Territory**: each server claims areas, a directory maps place to server, walking crosses servers | One owner per real place is a land grab and contradicts many communities in one city; borders are where SL and OpenSim break; most of Earth would be unclaimed |
| Peers speak to the home | **Clients connect to peers directly** (Second Life's child agents, one root and many children) | Every peer learns every client's address and may send it anything; no place for the home's budgets and filters; the Hypergrid review's "uncontrolled host" problem on every client |
| Peering by consent, lists for networks | **A grid service** (OpenSim's ROBUST, Overte's metaverse server): one directory and account store | One operator again; the fediverse shows it is not needed, and the project's goal is that anyone can run one |
| Home vouches with its signature | **Portable identity** (AT Protocol: DIDs and signed repositories) | Heavy, and the one thing it buys, moving an account, is rare; MLS credentials already pin the address, so a move re-keys chats either way |
| A hub per conversation (MIMI) | **Replicated state** (Matrix rooms with state resolution) | Every server stores and resolves every room; the simulator is one SQLite file on a shared VPS |
| One authority per thing, peers hold a view | **Shared authority** (DSG's scene synchronised between simulators) | Measured by its authors to cost seconds per hundred objects moved; two servers disagreeing about one ball is the failure a single authority exists to prevent |
| Interest by reach | **Zones**: a 0.01° grid as the unit of interest, and later of authority between processes | Regions under another name: cells on the wire, ceilings per cell, borders to hand over. Rejected by the person steering |

## Decisions to take

1. **Open peering by default for `cartopolis.org`**, with the fediverse
   block list: the inclusive network is the default, as in the fediverse.
2. **Default shares for an open peering**: people, things and nearby chat
   on, voice off until measured.
3. **Visiting is out.** A server that wants a person in admits their server
   or gives them an account; there is no guest sign-in on a server outside
   your network.
4. **Whether a person may hide their own server's layer**: no, in this
   design; their own server is the one they are on.
