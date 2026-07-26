# ADR-034: Public Data Relay (Standardised Upstream Adapters)

---
**Metadata:**
- **ID**: ADR-034
- **Status**: Proposed
- **Date**: 2026-07-23
- **Tags**: [ops, networking, cache, data-layers, live-data, architecture, crates, upstreams]
- **Decision Type**: architecture_pattern
- **Aligns with**: [G-04, G-02, P-03, P-05]
- **Roadmap tier**: P1
- **Doc resolution**: R2
- **Related**: [ADR-033, ADR-031, ADR-035, ADR-032, ADR-023, ADR-015, ADR-014, ADR-007]
- **Supersedes**: []
- **Superseded By**: []
---

## Context

**Problem**: ADR-033's web target needs some Public-tier upstreams fetched server-side. That is
not a one-off — we keep adding upstreams, and the client already reaches **eleven** of them while
re-implementing the same policy each time, divergently:

| Concern | Current state |
|---|---|
| User-Agent | **five different constants** (`TILE_USER_AGENT`, `NAV_USER_AGENT`, `OVAPI_USER_AGENT` ×2, plus `weather.rs` / `globe_weather.rs`) |
| Cache | per-file disk paths (`tile_cache_path`, `cell_cache_path` ×2), per-file TTLs (`LINES_TTL`, `VEHICLE_TTL`, `REFRESH` ×2) |
| Politeness | `OVERPASS_THROTTLE` sleep in one file, ad-hoc `15u64 << attempts` backoff in another, nothing in the rest |
| Timeouts | hand-set per call site (15 s, 30 s, …) |

So the requirement is "**standardise how a Public-tier upstream is reached**", in a way that also
collapses this duplication.

### CORS is not the driver — one origin is

An earlier framing of this work said we needed to emit CORS headers. That was wrong. If the wasm
app and everything it calls are served from **one origin**, those requests are same-origin and
CORS never applies. Our own services are currently on *sub*domains (`nominatim.garage44.eu`,
`3dbag.garage44.eu`), and a subdomain is a different origin — so the fix is to **path-mount them
under the app's origin** in the existing Caddy, not to sprinkle CORS headers.

CORS survives in exactly two places: consuming a *third party's* headers if we ever call one
directly (L0 below), and local dev (`trunk serve` on another port — use trunk's proxy to mirror
the production paths rather than dev-only CORS headers).

The relay's real justification is therefore **server-side fetch, caching, identification and
transform** — not CORS.

### Findings from the upstream audit

Auditing all eleven upstreams surfaced three constraints that matter more than CORS did:

1. **Secret custody.** `ai_assistant.rs` sends an `ANTHROPIC_API_KEY` as `x-api-key`. A wasm build
   ships to the user's machine, so it **cannot hold a secret**. This one is not a Public-tier
   problem at all — see "What the relay does *not* serve" below.
2. **Mixed content.** OVapi departures (`http://v0.ovapi.nl/tpc/…`) and lines
   (`http://v0.ovapi.nl/line/`) are **plain HTTP**. An HTTPS page cannot fetch them at all —
   blocked as mixed content, independent of CORS. They must be proxied to be reachable at all.
3. **`User-Agent` is a forbidden header in browsers.** `fetch()` cannot set it. Every upstream
   whose usage policy expects identification — OSM, Nominatim, Overpass, met.no, OVapi, i.e. the
   reason those five constants exist — cannot be identified from a browser. Anything carrying a
   real identification obligation therefore wants a relay.

**Requirements**:
- Reach upstreams a browser cannot (mixed content, no CORS, identification obligations).
- Allow server-side **decode/normalise** where the client shouldn't do it (GRIB → grid).
- One shared warm cache, so N viewers are not N upstream hits (P-03, and a condition of the
  OSM/Nominatim usage policies).
- Adding an upstream should be **one registry entry**, matching how layers are already added
  (`LAYER_DEFS`, ADR-023).
- Must not require a login or the simulator: ADR-031's Public tier is anonymous, and ADR-033's
  Phase 1 web Viewer runs with no sim session.
- Keep the deployment simple — ideally one process to run.

## Decision

Add a **`vibe_relay`** crate: a small, stateless, **unauthenticated** HTTP service that fronts
Public-tier upstreams through a **registry of upstream adapters**, served from the **same origin**
as the app.

### Single-origin layout (one Caddy site block)

```
https://viberfox.garage44.eu/            → wasm app + assets
https://viberfox.garage44.eu/tiles/*     → self-hosted vector tiles, PMTiles (ADR-035)
https://viberfox.garage44.eu/nominatim/* → nominatim
https://viberfox.garage44.eu/overpass/*  → overpass
https://viberfox.garage44.eu/3dbag/*     → 3dbag
https://viberfox.garage44.eu/v1/*        → vibe_relay (L2 adapters)
wss://viberfox.garage44.eu/ws            → vibe_sim (ADR-033 Phase 2)
```

Same Caddy that already reverse-proxies these, on one hostname instead of several.

### A three-level ladder (choose the lowest that works)

| Level | When | Cost |
|---|---|---|
| **L0 — direct** | third-party that sends CORS *and* carries no identification obligation | nothing. **Don't proxy what you needn't** (P-03). Currently **no members** — kept as the question to ask first |
| **L1 — path-mount** | our own services; simple passthrough | **Caddy route only, no Rust** |
| **L2 — relay adapter** | needs transform, cache, identification, or is otherwise unreachable (mixed content, no CORS) | one `UPSTREAMS` entry |

### The registry (mirrors `LAYER_DEFS`, ADR-023)

```rust
pub struct UpstreamDef {
    /// Stable key; served at `/v1/{key}`.
    pub key: &'static str,
    /// Build the upstream URL from the request's query params.
    pub url: fn(&Params) -> Result<String, String>,
    /// Upstream bytes → response bytes. Identity for a pure passthrough.
    pub transform: fn(&[u8]) -> Result<Vec<u8>, String>,
    pub content_type: &'static str,
    /// Serve from cache within this window.
    pub ttl: Duration,
    /// Minimum spacing between upstream hits (politeness).
    pub min_interval: Duration,
}

pub const UPSTREAMS: &[UpstreamDef] = &[ /* one entry per upstream */ ];
```

**Every adapter gets these for free** — each replaces something currently duplicated or missing:

1. **TTL cache + single-flight** — concurrent requests collapse to one upstream fetch.
2. **Stale-if-error** — serve the last good response when the upstream fails, instead of a gap.
3. **One project User-Agent** — replaces the five divergent constants, and supplies the
   identification a browser cannot.
4. **Politeness spacing** (`min_interval`) — replaces the ad-hoc sleep/backoff.
5. **Timeout + size cap** — uniform, not per-call-site.
6. **TLS termination** — an `http://` upstream becomes reachable from an HTTPS page.

### Upstream inventory and approach

| # | Source | Endpoint | Approach | Why |
|---|---|---|---|---|
| 1 | OSM vector tiles (MVT) | *(self-hosted — **ADR-035**)* | **L1** `/tiles/*` | OSM discourages proxying their CDN; we serve our own vector tiles from a PMTiles archive (ADR-035). Highest byte volume |
| 2 | Nominatim search | `nominatim.garage44.eu` | **L1** `/nominatim/*` | ours |
| 3 | Nominatim reverse | same, `/reverse` | **L1** `/nominatim/*` | ours |
| 4 | Overpass | `overpass.garage44.eu/api/interpreter` | **L1** `/overpass/*` | ours (client-side throttle can later move to an L2 adapter) |
| 5 | 3DBAG LoD2.2 | `3dbag.garage44.eu/lod22/{z}/{x}/{y}.json` | **L1** `/3dbag/*` | ours; large payloads suit plain HTTP caching |
| 6 | met.no forecast | `api.met.no/weatherapi/locationforecast/2.0/complete` | **L2** `met-forecast` | UA identification required by ToS; shared cache honours their caching guidance |
| 7 | NOAA GFS (GRIB2) | `nomads.ncep.noaa.gov/cgi-bin/filter_gfs_1p00.pl` | **L2** `gfs-tcdc` | transform (GRIB → 360×181 grid) + cycle clock + cache; no CORS |
| 8 | OVapi departures | `http://v0.ovapi.nl/tpc/{codes}` | **L2** `ovapi-departures` | **mixed content** — unreachable otherwise; + UA |
| 9 | OVapi lines | `http://v0.ovapi.nl/line/` | **L2** `ovapi-lines` | **mixed content**; long TTL (24 h today) |
| 10 | OVapi GTFS-RT | `gtfs.ovapi.nl/nl/vehiclePositions.pb` | **L2** `ovapi-vehicles` | 15 s poll × N clients is antisocial; shared cache. Best future candidate for **push over WS** |
| 11 | Anthropic API | `api.anthropic.com/v1/messages` | **not the relay** — see below | holds a secret; admin-only |

### What the relay does *not* serve

The relay stays **Public-tier and unauthenticated**. Anything needing a session or an ACL goes
through `vibe_sim` over `NetMessage`, per the transport rule (Public/cacheable → HTTP;
session-bound/authoritative → WebSocket).

The **AI assistant is the first such case**: the Anthropic key is a server secret, and access is
**admin-only** (ADR-031's `Authorized(cap)` tier — its first concrete use). So the AI request
becomes a `NetMessage` handled by the sim, which checks the capability and calls Anthropic with
its own key. It is not an upstream adapter, and `AiConfig.api_key` leaves the client.

### Where it runs

`vibe_relay` is **lib + thin bin**, the shape ADR-015 already established for `vibe_sim` ("exposes
both a lib and a thin bin, so the server can run standalone *or* embedded"). `vibe_sim` embeds it
behind a default-on **`relay` feature**, so the normal deployment is still **one process**;
splitting it out later is a flag, not a refactor.

It is a **separate HTTP listener** from the game protocol: no auth, no session, no sim state, no
`NetMessage`. That separation is load-bearing — ADR-031's Public tier and ADR-033's Phase 1 must
work with no sim session, and the world-authority process should not carry a public unauthenticated
surface by default (P-04).

### Client side

The client keeps a configurable base URL (same shape as the existing `VIBE_NOMINATIM_URL` env
override, ADR-014) and targets `/tiles/…`, `/nominatim/…`, `/overpass/…`, `/3dbag/…` or
`/v1/{key}`. This is URL construction, not a new abstraction — it composes with ADR-033's
`HttpFetch` provider. **Both targets use the relay**, so there is one code path (the ADR-033
principle) and native gains the shared cache and politeness too.

## Rationale

**Primary Reasoning**:
1. **It consolidates instead of adding.** The relay is where five User-Agents, four cache schemes
   and two throttle strategies become one. The web target is the trigger; the cleanup is worth
   doing regardless.
2. **Browsers structurally cannot satisfy our upstream obligations.** No `User-Agent`, no mixed
   content, no secrets. These aren't preferences to design around — they decide which upstreams
   must be server-side.
3. **A shared warm cache is better citizenship and cheaper** (P-03), and keeps us inside usage
   policies as the viewer count grows.
4. **One registry entry per upstream** matches the pattern contributors already know from
   `LAYER_DEFS` (P-05).
5. **Lib + bin + default-on embed** gives the simple single-process deployment, without welding a
   public endpoint to the authoritative sim.
6. **Keeping auth out of the relay** preserves one clean invariant (Public = anonymous, stateless,
   cacheable) and sends session-bound work where authority already lives.

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| Emit CORS headers everywhere, keep subdomains | No Caddy restructure | Preflights, more config, still cross-origin | Single origin makes CORS moot entirely |
| Module inside `vibe_sim`, no crate | Fewest moving parts | Public tier then requires running the world sim; public unauth surface on the authority process | Breaks ADR-031/033 Phase-1 independence |
| Caddy proxy config only | No Rust code | Can't transform (GRIB→grid), can't single-flight or stale-if-error | Covers L1 only; L2 needs code |
| Relay grows authenticated adapters (for Anthropic) | One service | Muddies the stateless/anonymous invariant; duplicates sim authz | Session-bound work belongs in the sim |
| Proxy OSM's tile CDN | No tile infrastructure | OSM discourages proxying; still can't set UA | ADR-035 self-hosts vector tiles instead |
| Bespoke endpoint per upstream | Simple each time | Rebuilds cache/UA/throttle per endpoint | Doesn't standardise anything |

## Consequences

**Positive**:
- Public-tier upstreams become reachable from the browser — unblocks ADR-033 Phase 1.
- Five User-Agents / four cache schemes / two throttles collapse into one implementation.
- N viewers cost one upstream fetch per TTL, not N (P-03).
- Adding an upstream is one registry entry; adding a *layer* stays one `LAYER_DEFS` entry.
- The Anthropic key leaves the client entirely — a security improvement on **native** too, where
  it currently sits in every user's `.env`.
- A natural home for the rate limits/quotas the capability map lists as a gap.

**Negative**:
- **A new service to run and monitor** (mitigation: default-on embed keeps it one process).
- **A public, unauthenticated HTTP surface** we now own (mitigation: stateless, no sim/session
  access, size caps + timeouts + `min_interval`; rate limiting is the natural next addition).
- **We become a dependency in the data path** — relay down means Public layers degrade
  (mitigation: stale-if-error; client already tolerates fetch failure).
- **We now do the fetching**, so upstream terms (attribution, rate) are our obligation — one place
  to review, which is an improvement, but the obligation moved to us.
- **Subdomain→path migration** for our services (mitigation: keep the subdomains working; the env
  overrides in `geo_nav.rs` / `osm_buildings.rs` already make the base URL configurable).
- **Cache freshness is now a server concern** (mitigation: per-adapter `ttl`, visible in one place).

**Unverified**: whether met.no and the OVapi feeds send permissive CORS headers — the relay is
warranted for them on identification/mixed-content grounds regardless, so this does not change the
approach, only the reasoning recorded per row.

## Related

- [ADR-033](./033-build-targets-and-wasm-packaging.md): The web target that needs this.
- [ADR-035](./035-self-hosted-map-tiles.md): Vector tiles are served by us from a local PMTiles archive, not proxied from OSM.
- [ADR-031](./031-layered-access-model.md): The relay serves the Public tier; the AI assistant is the first `Authorized(cap)` (admin-only) case.
- [ADR-032](./032-global-cloud-cover-data.md): `gribberish` moves from the client to the `gfs-tcdc` adapter.
- [ADR-023](./023-data-layers.md): `UPSTREAMS` mirrors `LAYER_DEFS`; a layer names the upstream it needs.
- [ADR-015](./015-workspace-module-boundaries.md): `vibe_relay` follows the established lib + thin bin, embeddable shape.
- [ADR-014](./014-runtime-configuration-and-operations.md): Base URLs and per-adapter config follow the existing env/CLI layering.
