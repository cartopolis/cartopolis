# Research: how other worlds connect servers, and what Cartopolis already has

Read before [README.md](README.md), which is the design. Everything here was
checked on 2026-11-10: the OpenSimulator wiki, the Hypergrid 2.0 writing,
the 2016 security review, Intel's scaling paper, the Matrix, MIMI and AT
Protocol specifications, Overte's and Resonite's architecture pages, Third
Room's README, Photon's write-up, and this repository's code and notes.

## 1. OpenSimulator's Hypergrid

**What it is.** OpenSim worlds are grids of 256 m regions. A grid has central
services (the ROBUST suite: user accounts, inventory, assets, friends, grid
map). The Hypergrid, written by Crista Lopes from 2008, removes two central
assumptions: the map is no longer one grid's property (an operator *links*
foreign regions into their own map at coordinates of their choosing), and an
agent may be a *foreign user*, whose home services live on another grid. A
"virtual world hyperlink" is a region on the map, named `host:port:Region`.

**The services** ([Hypergrid Protocol](http://opensimulator.org/wiki/Hypergrid_Protocol),
[Implementation](http://opensimulator.org/wiki/Hypergrid_Implementation)):

- *Gatekeeper*, on the destination grid: answers which regions may be
  entered, admits an agent, and verifies it with the source grid.
- *User Agent Service* (UAS), on the home grid: `VerifyAgent`/`VerifyClient`
  for the gatekeeper, the travelling agent record, `GetHomeRegion`, logout,
  and status notifications to friends.
- *HG inventory, asset, friends and instant-message services*, on the home
  grid, called by the foreign region on the visitor's behalf.

**A teleport**: the source region asks the destination gatekeeper for the
region; the gatekeeper asks the home UAS to verify the agent; the destination
region waits for the viewer's connection, verifies it again with the home UAS,
then receives the visitor's attachments. The viewer speaks to the regions
only, never to grid services. The home UAS must be reachable from the
destination, or the visit is refused.

**What a foreign region gets**: a non-persistent user record naming the
visitor's home services (user, inventory, asset). From then on inventory
access is "wrapped": the foreign region fetches assets from the home asset
server and *copies them into its own asset service*; anything the visitor
takes is posted back to the home server.

**Hypergrid 2.0** (OpenSim 0.7.5, 2012;
[towards HG 2.0](http://www.metaverseink.com/blog/hypergrid/towards-hg-2-0/)):
the *suitcase*, one inventory folder that is the only one a foreign grid may
reach, plus appearance items; per-grid switches for whether foreign agents
are admitted (`ForeignAgentsAllowed`, allow/deny lists by source grid) and
whether local users at a given level may travel
(`ForeignTripsAllowed_Level_N`); and an *export* permission on assets, with
grid owners able to forbid rezzing what came home from elsewhere. The export
permission was contested enough that it shipped later and partially.

**Friends across grids** take a confirmation dance: the friendship is stored
on the foreign grid, forwarded to the home friends service as a request, and
confirmed by the traveller *when they are next home*
([Hypergrid Friends](http://opensimulator.org/wiki/Hypergrid_Friends)).

**Identity** is `First Last @grid.example:8002`: a name plus the home grid's
URL. A UUID per account, meaningful only on its grid.

**The 4096 bug**: viewers failed to render a destination more than 4096
regions away on the global map, so hyperlinked grids had to be placed near
each other. Fixed in viewers and OpenSim around 2013
([hypergridbusiness](https://www.hypergridbusiness.com/2013/09/much-hated-4096-bug-fixed/)).
It is a symptom of the deeper choice: one global region grid, into which every
other world has to be mapped.

**What broke, in the words of its critics**
([Heilmann and Maxwell, 2016](https://www.hypergridbusiness.com/wp-content/uploads/2016/09/Securing-the-Hypergrid-Michael-Heilmann-and-Douglas-Maxwell.pdf);
[Hypergrid Security](http://opensimulator.org/wiki/Hypergrid_Security)):

1. *The visited grid holds the visitor's session id*, and that id works
   against the home services. A malicious host "can request your inventory
   items on your behalf and store copies", even wipe the inventory. The
   suitcase narrows what can be reached; it does not change who holds the
   credential.
2. *Assets are protected by nothing but an unguessable UUID*, served over
   plain HTTP; once an asset is rendered anywhere its id is on the wire and
   `curl` fetches it from the grid's asset service.
3. *Every asset a visitor rezzes is copied into the foreign asset store* and
   stays after they leave, so malicious scripts are injected with a worn hat.
4. *Trust is by domain allow-lists*, which a script's own HTTP calls walk
   around (an LSL script on grid A calling grid B's services from A's host).
5. *No local vetting of foreign accounts*: anyone can make accounts on any
   home grid faster than a destination can ban them.
6. *Archives (OAR, IAR) strip all provenance*, so content laundering is a
   file copy away.
7. The reviewers' conclusion: "better encryption cannot improve hypergrid,
   because the hypergrid design is flawed. Hypergrid has a broken trust
   model."

**What held up, and is worth keeping**: the split between a home that vouches
and a destination that decides; home services staying home; the four policy
switches (admit visitors, from whom; let residents travel, where to); the
"gateway region" idea (a grid may admit visitors only at chosen places); and
the frank observation on its own security page that malicious *hosts* are the
new threat open worlds add, with "protecting you from yourself" (limit what
travels with you) as the answer.

## 2. Scaling one world

**Intel's Distributed Scene Graph** (Liu, Bowman, Adams, Hurliman, Lake,
[WSC 2010](https://www.informs-sim.org/wsc10papers/070.pdf); DSG was folded
into OpenSim core in 2011): measured on OpenSim, both CPU and bandwidth grow
*quadratically* with users in one space, because every update reaches every
client. Their diagnosis of the "simulator-centric" architecture: regions
collapse past ~100 users; load balancing by moving a region's objects is
prohibitive (seconds per hundred prims, "thousands of seconds to migrate 100K
prims"); and physics, scripts and client managers want different partitions.
Their answer: the *Scene* (authoritative state) separated from *actors*
(physics, scripts, client managers), each subscribing to a part of the scene,
with an eventually-consistent timestamped sync; DSG-M added microcell
partitions. Thousands of avatars in one region were demonstrated.

**Second Life region crossings**
([Region crossing](https://wiki.secondlife.com/wiki/Region_crossing)): an
avatar near a border has a *child agent* in the neighbouring region, which
becomes the root agent as it crosses; objects near the edge have "ghost"
representations on both sides. Crossings are the least reliable moment in the
platform, twenty years on.

**Photon's seamless MMO backend**
([2025 write-up](https://blog.photonengine.com/building-a-seamless-mmo-backend-across-machines/)):
the world is split into nodes by load, not by a fixed grid; a *gateway* holds
each player's connection for the whole session and does per-player interest
management; neighbouring nodes keep an overlap zone where they predict
entities they do not own. **SpatialOS** did the same with "areas of
authority" per worker and an authority handover between them; its dynamic
load balancer stayed experimental.

The common lesson: the client's connection and interest set belong to one
stable gateway; authority over space moves behind it; and partitions are
cheap only when the state they move is small and the scene lives outside the
simulator process.

## 3. Federated protocols

**ActivityPub** (what Cartopolis already speaks, `docs/notes/fediverse.md`):
an object lives on its author's server; servers push activities to inboxes
with HTTP signatures; identity is `name@server` resolved by WebFinger; there
is no shared state and no portability. Right for public posts about places,
wrong for anything real-time or with shared state.

**Matrix**: a room's full state is replicated to every participating
homeserver; events form a signed DAG; a deterministic *state resolution*
algorithm makes every server converge. Powerful and expensive: every server
in a room stores and resolves everything. Third Room (2022–23) put 3D worlds
on it: a world is a room, its scene a glTF in the room, avatars networked
over WebRTC data channels set up through Matrix (MSC3401), identity and
friends carried between worlds by the Matrix account. Rooms as worlds is a
usable idea; replicating a live world's state Matrix-style is not.

**MIMI** (IETF, More Instant Messaging Interoperability,
[architecture](https://datatracker.ietf.org/doc/html/draft-ietf-mimi-arch),
[protocol](https://datatracker.ietf.org/doc/html/draft-ietf-mimi-protocol)):
one *hub* provider per room orders and fans out; *follower* providers forward
their users' messages to the hub and deliver locally; MLS is the end-to-end
layer and servers are external senders, never members; key material is
fetched across providers (`POST /keyMaterial/{user}`); provider-to-provider
authentication is mutual TLS with `From: mimi@domain`; consent is explicit.
This is the model `docs/notes/web-chat.md` already chose for chats, and
`cartopolis_core::address` already encodes it (a person is `name@server`, a
conversation id starts with its hub's name).

**AT Protocol** (Bluesky,
[overview](https://atproto.com/guides/overview)): identity is a DID whose
document names the current host and keys; data lives in a signed repository
the user can move between hosts; relays and app views aggregate. The one thing
it does that nothing else does is *account portability*. Its critics
([Rescorla](https://educatedguesswork.org/posts/atproto-firstlook/)) note the
DNS handle is the weak link, access control was unspecified, and the whole is
heavy for what it buys.

**Overte / Vircadia** (High Fidelity's descendants): a *domain server* hosts
one world with assignment clients (avatar mixer, audio mixer, entity server);
a central *metaverse server* holds accounts, a directory of places and
connections, and users travel between domains with that identity. A grid
with a central directory, in other words. **Resonite** likewise: identity,
inventory and assets are central cloud services, sessions are hosted by
users or headless servers.

**Open Metaverse Interoperability Group** and the Metaverse Standards Forum
work on avatar and asset formats (glTF extensions); nothing there defines
how a person moves between servers.

## 4. What Cartopolis already has, and what it does not

Facts from the code on 2026-11-10 (file paths in the design where they
matter):

- **One server, one world, no partitions.** `SimWorld` holds every prim in
  a `Vec` and every avatar in a map; one broadcast channel sends every
  avatar to every client every tick; the area-of-interest filter was removed
  with regions (`docs/notes/retired/regions.md`: "a real AoI filter has to
  be computed per connection, which means the snapshot stops being a
  broadcast"). Ceilings: 1,000 prims and 200 scripts per world, 64 physical
  prims, 4,096 sessions. Measured 2026-09-15: the full prim list in every
  snapshot cost 6 % of the tick and 3.2 MB/s per client, which is why prims
  now go out once at the handshake (`docs/notes/simulator-tick-budget.md`).
- **Avatar positions are not comparable.** An avatar's sim position is
  metres in *its own client's* map frame; only `ClientIntent.geo` (the
  client's own report) is an absolute place. Here chat and voice compare the
  per-client frames (`crates/simulator/src/chat.rs`, `within_radius`).
  Prims, by contrast, carry an absolute `GeoAnchor` since ADR-029.
- **Physics already cuts the world into 0.01° zones** (about 1.1 × 0.67 km
  at Groningen), each a Rapier world, existing only while a physical prim is
  in it (`docs/notes/physical-prims.md`).
- **Identity is per server and already server-qualified where it had to
  be.** `accounts.username` is unique per server; the wire carries bare
  usernames for blocks, contacts and reports, but every MLS credential and
  every private-chat member is `name@server`, and every conversation id is
  `server/…` (protocol 37, `crates/core/src/address.rs`). The simulator
  refuses key-package claims for other servers with a 501, in words.
  `SimConfig::server_name` is the host of `public_base_url` and must not
  change.
- **Tokens are opaque, local and unsigned**: 256 random bits, hashed in
  `sessions` and `access_tokens`, resolved by `Sessions::resolve_access`,
  re-checked by every open connection every 5 s. There is no device key in
  the handshake; the only device identity is the E2EE device UUID the client
  minted itself.
- **Server-to-server already exists for the fediverse**: draft-cavage HTTP
  signatures with per-actor RSA keys in `fedi_keys`, an inbox that verifies
  against cached actor keys, `fetch::Client` refusing private, loopback and
  link-local addresses and anything but https on 443, a daily shared block
  list plus operator blocks, and a `fedi_servers` table that remembers what
  software each server runs.
- **Roles are a closed list of permissions** with a built-in Player role
  every account holds; a non-elevated access token is Player minus the
  elevated permissions. A Visitor role is one row away.
- **Atlas planned the token split** that federation needs: "atlas
  authenticates and issues a short-lived signed token, and the simulator
  verifies it with a public key" (`docs/notes/atlas.md`), not built.
- **Links already name a server and ignore it.** `https://cartopolis.org/go?place=…`,
  `?community=CODE` and `?contact=name~code` all carry a host, and
  `place_link.rs` deliberately reads none of them.
- **Map data is not the simulator's.** Tiles, terrain, cells, Overpass and
  Nominatim come from `*.cartopolis.org` hosts fixed at build time, with
  `CARTO_*` overrides; the handshake's `osm_tile_url_template` is the one
  source a server can redirect. The simulator "holds no map data and is not
  going to".
- **Models are content-addressed** (`data/assets/<blake2s>.glb`), served
  `immutable` at `/api/assets/{hash}.glb` behind a session token of *that*
  server; `asset_claims` records each uploader's licence (`own`, `cc0`,
  `cc-by`, `other`), and `other` may never be shared.
- **Calls**: the simulator you are connected to is always the SFU; keys come
  from the chat's MLS group; signalling rides the wire.
- **Earlier directions, retired**: ADR-021's flat clusters with an S2
  index and sim-to-sim frame handoff ("superseded direction"); ADR-012's
  radius filter; ADR-028 rejected a "multi-sim peer mesh" as premature and
  left `ChatHub` as the seam ("a trait can wrap it when multi-sim lands").
