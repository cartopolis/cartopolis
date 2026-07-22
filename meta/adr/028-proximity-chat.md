# ADR-028: Proximity Chat with Place-Derived Channels

---
**Metadata:**
- **ID**: ADR-028
- **Status**: Proposed
- **Date**: 2026-07-21
- **Tags**: [chat, social, geo, places, protocol, networking, privacy, persistence]
- **Decision Type**: architecture_pattern
- **Aligns with**: [G-03, G-04, P-01, P-02, P-07]
- **Roadmap tier**: P2
- **Doc resolution**: R1
- **Related**: [ADR-009, ADR-012, ADR-013, ADR-014, ADR-015, ADR-016, ADR-023, ADR-029, ADR-031]
- **Supersedes**: []
- **Superseded By**: []
---

> **Tier (ADR-031).** Chat is a **Simulation-tier** feature: it requires a logged-in
> session, alongside avatars and prims. The anonymous **Viewer** tier has no chat.
>
> **Implemented (protocol v11):** Local (proximity say/shout), Global, and Rooms —
> `ChatSend`/`ChatMessage`/`ChatSubscribe` (kinds 17–19), `ChatHub` routing, per-scope
> rate limits, SQLite scrollback for durable channels (migration V6), and the egui chat
> panel. **Deferred to `vibe_geo`:** the geo tiers (country/province/municipality) and
> avatar geo-authority on the sim — they slot in as extra channels behind the same
> `resolve()` with no protocol change.

## Context

**Problem**: The sim has no chat. The obvious model — named rooms users join — is a
poor fit for a world that is already *situated*: every avatar has a real-world
lat/lng, and the interesting question is "who is near me / in my city" rather than
"who joined `#general`". We want chat scopes derived from geography, without
turning every message into a geocoding request.

**Requirements**:
- Scopes from immediate proximity up to global, derived from position, not from
  user-managed room membership.
- Resolution must be **server-side and authoritative** (P-01) — a client cannot
  talk its way into a place it is not standing in.
- Resolution must be **fast and infallible**: no network call in the path. The
  self-hosted Nominatim and Overpass instances already carry throttles and long
  backoff ladders (`osm_buildings.rs`); chat membership must not inherit them.
- **Degrade automatically** where data is thin: missing city → province →
  country → global, with no per-country branching in protocol or UI.
- **Extensible to other countries and other data sources**, all normalised into
  one internal model.
- An explicit privacy position (P-07), since chat is a location-adjacent channel.

**Non-goals** (deliberately deferred, see *Consequences*): multi-simulator chat
routing, cross-sim presence, a boundary-rendering data layer, polygon-accurate
boundaries, and **voice/video media** — this ADR is text only; real-time audio/video
is a separate transport deferred behind auth (see *Consequences*).

## Decision

**Chat channels are a pure function of position, resolved server-side against a
sparse place hierarchy, with a proximity tier below the administrative ones.**

Five scopes, coarse to fine:

| Scope | Channel key | Membership |
|-------|-------------|------------|
| Global | `geo:` | everyone |
| Country | `geo:iso1:NL` | position |
| Province | `geo:iso1:NL/iso2:NL-GR` | position |
| Municipality | `geo:iso1:NL/iso2:NL-GR/cbs:GM0014` | position |
| Local (tier 0) | *(none — computed per send)* | radius around the sender |
| Room | `room:<slug>` | explicit opt-in, non-geographic |

Tier 0 exists because the smallest administrative tier is still an entire city;
proximity chat is where co-presence actually reads as social.

**Approach**:

### Place model (`vibe_geo`, new crate)

A resolved location is a **sparse hierarchy**, so degradation is "the list is
shorter" rather than a special case:

```rust
pub struct PlacePath { levels: Vec<PlaceRef> }      // coarse → fine, may skip
pub struct PlaceRef {
    level: PlaceLevel,   // Country | Province | Municipality (extensible)
    code:  String,       // namespaced: iso1:NL, iso2:NL-GR, cbs:GM0014, osm:r47826
    name:  String,       // "Groningen"
    label: String,       // "Provincie" / "Bundesland" / "State"
}
```

Global is the implicit root and never appears in the vec. Mid-ocean yields an
empty vec; Portugal yields one entry; Groningen yields three.

Codes are **namespaced by source** so two providers cannot mint the same key.
These strings are channel identity and scrollback keys — a collision across a
border would silently merge two cities' conversations.

`level` is semantic, never the raw OSM `admin_level`: level 4 is a *provincie* in
NL, a *Bundesland* in DE, a *région* in FR, and absent in some countries. `label`
carries the per-country display term as data.

### Sources plug in behind a provider trait

```rust
pub trait PlaceProvider {
    fn precision(&self) -> u8;                        // centroid stub < polygons
    fn levels(&self) -> &[PlaceLevel];
    fn covers(&self, lat: f64, lng: f64) -> bool;
    fn resolve(&self, lat: f64, lng: f64) -> Vec<PlaceRef>;
}
```

`PlaceRegistry` queries every covering provider and merges per level, highest
`precision` winning. Adding a country, swapping a data source, or running mixed
precision (polygons for NL, countries-only elsewhere) is provider registration —
no change to the model, protocol, or UI.

**v0 ships two providers:**
- **NL municipalities + provinces** — a small table of centroids, resolved
  nearest-centroid. A Voronoi approximation of the real boundaries: correct in the
  interior, wrong in a band near irregular borders.
- **World countries** — Natural Earth `admin_0`, point-in-polygon (~258 polygons,
  R-tree over bounding boxes + winding number).

The upgrade to polygon-accurate municipalities is a higher-`precision` provider
behind the same `resolve()` signature — a data change with zero protocol impact.

### Coverage: "no data" is not "nowhere"

```rust
enum LevelResult { Resolved(PlaceRef), Outside, NoData }
```

`NoData` (no provider offers this level here — municipality in Portugal) hides the
tab. `Outside` (covered, but inside no unit — the North Sea) shows it disabled.
Without the distinction both look like a bug to the user.

### Membership, hysteresis, linger

Membership is recomputed only when an avatar has moved >50 m since its last
resolve — at walking speed, roughly once per avatar per 6 s, each costing
microseconds. The client never asserts membership; it is told.

- **Hysteresis**: leaving a place requires crossing its boundary by ~100 m, so
  pacing a border does not flap channels.
- **Linger**: on a real exit the previous channel stays subscribed read-only for
  ~60 s, so a conversation does not vanish mid-sentence.

Positions are geo-anchored (ADR-029): prims carry an absolute `f64` lat/lng, and
avatar positions resolve to lat/lng the same way. The sim passes that coordinate
straight to `resolve()` — no tangent-frame conversion, since the sim holds geo
authority and has no anchor of its own.

### Protocol (adds kinds 17–19, bumps `PROTOCOL_VERSION`; ADR-009 envelope unchanged)

```rust
enum ChatScope { Local { shout: bool }, Place(PlaceLevel), Global, Room(String) }

ChatSend     { request_id: u32, scope: ChatScope, body: String }         // kind 17
ChatMessage  { channel: String, sender_id: u64, sender_name: String,
               sent_at_ms: u64, body: String, distance_m: Option<f32> }  // kind 18
ChatChannels { channels: Vec<ChannelInfo> }                              // kind 19
```

`ChatSend` carries a **scope**, not a channel string — the client says "province",
the server decides which province. `ChatChannels` is pushed whenever the resolved
set changes and drives the tab strip.

**Coarsening rule**: a scope resolves to the nearest *available* level, coarsening.
Asking for `Province` where none exists sends to `Country`. A message always has
somewhere to land, and the client needs no per-country knowledge.

### Routing, persistence, limits

- A `ChatBus` trait (`publish` / `subscribe`) with a single in-process
  implementation over `tokio::sync::broadcast`, one sender per active channel,
  created on first subscriber and dropped at zero. Frames are encoded once per
  channel, not per recipient. The trait exists so multi-sim routing is additive
  later; **no peer transport is specified or built here.**
- Scrollback in SQLite (`chat_messages`, indexed on `(channel, sent_at_ms)`),
  pruned to the last N per channel, replayed on subscribe. Writes go through the
  single server writer (ADR-013). The local tier is ephemeral and never stored.
- Per-scope rate limits, since reach differs by orders of magnitude: local ~2/s,
  municipality ~1/5 s, province ~1/15 s, country and global slow-mode ~1/60 s plus
  a length cap. Enforced alongside the existing `MIN_INTENT_INTERVAL` limiter.

### Threat model (P-07)

- Messages above the local tier carry **no coordinates** — only a place code and a
  coarse label. Chat must not become a position oracle for anyone who can hear you.
- The local tier exposes `distance_m` only, and only to avatars already within
  radius — who can already see the sender via avatar replication. No new leak.
- Resolution is server-side; a client cannot claim membership.
- Stored scrollback holds sender ids and bodies under a bounded retention window.
  Per-user deletion and export are **not** addressed in v0 and are flagged below.

## Rationale

**Primary Reasoning**:
1. **Derived membership removes the entire room-management surface.** No join,
   leave, room list, or invite paths — and no way to be in a channel you are not
   standing in, which satisfies P-01 by construction rather than by validation.
2. **A local lookup is the only way to make membership reliable.** Reverse
   geocoding puts channel membership behind a service that already needs an 800 ms
   throttle and a 15 s → 4 m backoff ladder; a wobble would flap every user's
   channels. A local table cannot fail, and lets client and server compute
   identical answers from identical data — so tab labels can never disagree with
   authority near a border.
3. **The sparse hierarchy plus provider registry is cheap flexibility at the
   interface**, not expensive flexibility in infrastructure: roughly a trait and
   two enums. It absorbs new countries, new sources, and the centroid → polygon
   upgrade without touching the wire format.
4. **Worldwide baseline, NL enhancement** — the same shape ADR-027 used for 3DBAG.
   Global and country work anywhere; province works anywhere via Natural Earth
   `admin_1` when added; only municipality is NL-only today. This keeps ADR-023's
   objection (a NL-only source baking NL assumptions into the registry's shape)
   from applying: the *tier structure* is worldwide, only the finest tier degrades.

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| Named rooms users join | Trivial; familiar | Ignores that every avatar already has a real position | The world is situated; geography is the interesting axis |
| Nominatim reverse geocode per resolve | No bundled data | 10–100 ms, async, fails; client/server can disagree at borders | Membership must not inherit a throttled service's failure modes |
| Overpass `is_in` per resolve | Genuine PIP service, already hosted | Same latency and throttle problems | Same; useful as a *build-time verification* oracle instead |
| Polygon boundaries in v0 | Accurate at borders | Overpass extraction, ring assembly, simplification tolerance, pack format | Deferrable behind `resolve()`; ship the stub, upgrade invisibly |
| Raw OSM `admin_level` as the tier key | Direct from source | Level 4 means different things per country; not portable | Semantic level + display label is the same cost and correct |
| Distance-only tiers (radius rings) | No geo data at all | "Everyone within 10 km" is not a community; ignores real boundaries | Places are what users actually identify with |
| Multi-sim peer mesh in this ADR | Future-proof | Trusted transport + directory service before auth exists | Inverts the roadmap's auth-first sequencing; `ChatBus` is the seam |

## Consequences

**Positive**:
- Chat scopes that mean something socially, with no room-management UI or state.
- Resolution is local, microsecond-scale, and cannot fail — no new runtime
  dependency on Nominatim or Overpass.
- Client and server compute identical membership from identical data.
- Adding a country or upgrading NL to polygon accuracy is provider registration
  plus a data file; protocol, persistence, and UI are untouched.
- `vibe_geo` gives the eventual BAG/BRT work a home, without committing to a
  generic pipeline before there is a second consumer to shape it.

**Negative / risks**:
- **Centroid approximation is wrong near irregular borders** — roughly 5–15% of
  area, where the nearest centroid is not the containing municipality. Mitigated by
  the ~100 m hysteresis being meaningless at that error scale (so it is a *wrong
  answer*, not a *flapping* one), and by the polygon upgrade path being invisible
  above `resolve()`. Ship only if a mis-assigned gemeente near a town edge is
  tolerable for v0; otherwise do polygons first.
- **Sequencing against the roadmap**: `roadmap.md` prefers authentication →
  authorization *before* chat send paths, and auth remains a gap (ADR-016 is
  Proposed; capability map lists it open). This ADR does not resolve that. Chat
  send inherits whatever identity `ClientHello`'s `client_token` provides, which is
  weak. **Either ADR-016 lands first, or chat ships operator-gated** (a config flag
  defaulting off) until it does. This is the sharpest open risk here.
- **No moderation story**: muting, blocking, reporting, and operator takedown are
  unspecified. Rate limits bound spam volume, not abuse. Global and country tiers
  are the exposed surface; consider launching with only local and municipality
  enabled.
- **Boundary data goes stale**: Dutch municipalities merge regularly. The
  namespaced-code identity scheme survives renames; a merge still orphans the old
  channel's scrollback. Needs a refresh cadence and a merge/redirect story.
- **Attribution**: any OSM-derived boundary data is ODbL, and a redistributed
  derived database carries share-alike. Natural Earth is public domain. Provenance
  and attribution ship with the data file, per the ADR-023/027 precedent.
- **Single simulator only.** Geographic channels mean "everyone in Groningen *on
  this process*". Correct today; silently wrong the moment a second sim exists.
  `ChatBus` is the seam, but the limitation must be documented and revisited before
  multi-sim, since clients will have come to depend on channel completeness.
- **New crate** (`vibe_geo`) touches ADR-015's crate boundaries; it is placed
  outside `vibe_core` so `rusqlite` and the geometry stack stay out of the
  protocol crate.
- **Scrollback growth and data-subject rights** (deletion, export) are unaddressed
  in v0 beyond a retention bound.
- **Voice/video is out of scope here and deferred behind auth.** The same
  place-derived scopes are the obvious channel model for spatial audio later, but
  real-time media is a different transport (WebRTC: ICE/DTLS/SRTP + an SFU), not a
  `ChatBus` publish. The direction we are leaning is **str0m** (a sans-IO WebRTC
  crate, sans-IO the same way `rtc` is): because it imposes no runtime or clock of
  its own, the SFU state machine can be **embedded in `vibe_sim`** and driven from
  the existing tick loop, keeping media in-process rather than a separate service.
  SDP offer/answer can ride the existing ADR-009 envelope as new message kinds
  rather than adding a signaling server. Sequencing: this waits until **ADR-016
  (auth) lands** — an unauthenticated media path is a worse position oracle and
  abuse surface than text, so it inherits the same auth-first gate as the text send
  path above, and is picked up only after. Open costs a media ADR must own: TURN for
  restrictive NATs, congestion control, simulcast, and the SFU forwarding layer
  (str0m gives transport, not routing). A dedicated ADR supersedes this note when
  the work starts.

## Related

- [ADR-009](./009-application-protocol-envelope-v0.md): Envelope reused unchanged; this adds message kinds 17–19 and bumps `PROTOCOL_VERSION` (currently 10).
- [ADR-012](./012-interest-management-and-osm-tiles.md): Tier 0 needs the same entity proximity query as observer AOI; one index should serve both.
- [ADR-013](./013-sqlite-migrations-and-server-writer.md): Scrollback writes go through the single server writer; this is also why a shared-SQLite chat bus was rejected.
- [ADR-015](./015-workspace-module-boundaries.md): Adds the `vibe_geo` crate.
- [ADR-016](./016-authentication-and-session-model-v0.md): Prerequisite for an ungated chat send path.
- [ADR-029](./029-geo-anchored-prim-storage.md): Geo-anchored positions (`f64` lat/lng) that `resolve()` consumes directly — no tangent-frame conversion.
- [ADR-031](./031-layered-access-model.md): Chat is a Simulation-tier feature (login required).
- [ADR-023](./023-data-layers.md): Same self-hosted-geo and attribution shape; also the source of the "layers must work worldwide" constraint this addresses.
- [ADR-027](./027-3dbag-buildings.md): Precedent for worldwide baseline plus NL enhancement.
