# ADR-033: Build Targets — Standalone (Native) and Web (wasm32)

---
**Metadata:**
- **ID**: ADR-033
- **Status**: Proposed
- **Date**: 2026-07-23
- **Tags**: [architecture, platform, wasm, web, build, packaging, ops, cors, dependency]
- **Decision Type**: architecture_pattern
- **Aligns with**: [G-02, G-04, P-05, P-03]
- **Roadmap tier**: P1
- **Doc resolution**: R2
- **Related**: [ADR-030, ADR-031, ADR-034, ADR-035, ADR-008, ADR-016, ADR-032, ADR-023, ADR-007]
- **Supersedes**: []
- **Superseded By**: []
---

## Context

**Problem**: We want cartopolis reachable **in a browser** without foreclosing native-only
capability (ADR-030) and without the browser's limits dictating the architecture. ADR-030 set
the *pattern* (substitutable providers + additive plugins) and ADR-031 the *access model*
(anonymous Viewer → login → ACL). Neither says **what we actually build and ship**. This ADR
defines the target matrix, the dependency/feature topology that produces both from one crate,
the server-side work the web target implies, and the delivery order.

**Current state** (measured, `crates/cartopolis`):

| Native-only primitive | Sites | Status |
|---|---|---|
| `rusqlite` (client SQLite) | **0** | **Gone** — dropped in `7b44042`; no `db/`, no `database.rs` |
| `ureq::` blocking HTTP | 14 across **10 files** | one uniform pattern (see below) |
| `std::thread::spawn` | 15 | mostly the HTTP workers |
| `std::fs::` | 30 | disk caches, `user.json`, login TOML |
| `tokio::net` / `TcpStream` | 3 | the sim transport |

No wasm scaffolding exists yet. Two facts shape the design:

1. **The HTTP surface is uniform.** All 10 fetch files follow one shape — `std::thread::spawn`
   → blocking `ureq::get` → `mpsc` back to a Bevy system. One `HttpFetch` provider replaces all
   of them without touching their parsing/meshing logic. The uniformity is the leverage.
2. **Client SQLite is already gone**, so the only remaining "embedded authority" is the `solo`
   in-process `cartopolis_simulator` — already a Cargo feature, already off-able.

**New constraints since ADR-030**:
- ~~**A C dependency entered the client.**~~ **Resolved before this ADR lands.** ADR-032's GFS
  cloud cover pulled `gribberish` with default features → `openjpeg-sys` → a cmake-built C
  library that does not build for `wasm32-unknown-unknown`. Measured: NOMADS packs the
  `TCDC:entire atmosphere` field we request with **DRS template 5.3** (complex packing +
  spatial differencing), which `gribberish` decodes with *no* optional feature; the `jpeg`
  (openjpeg-sys) and `png` features only cover templates 5.40/5.41, which this field never
  uses. `default-features = false` drops the C dependency with the decode unchanged, and
  leaves `gribberish` pure-Rust and wasm-buildable. **The remaining web blocker is CORS,
  not the decoder** — see the GRIB endpoint below, which is now justified on origin policy
  and payload alone.
- **CORS and origin policy.** Native `ureq` calls any host; a browser cannot. Every Public-tier
  (ADR-031) service must send permissive CORS headers or be proxied through our origin. Our
  self-hosted services (Nominatim, Overpass, 3DBAG) already sit behind the garage44 Caddy and
  can set headers; third-party origins (NOMADS/GFS, met.no, OVapi, tile hosts) cannot be
  assumed to.

**Requirements**:
- One `cartopolis` crate produces both targets; feature logic is not duplicated (P-05).
- The web build ships **useful value without the sim** (ADR-031 Viewer tier).
- Native keeps every capability it has today, plus room for native-only additions (ADR-030).
- Web/native share one session flow: start anonymous, upgrade at login (ADR-031).

## Decision

Ship **two build targets from one crate**, with providers selected by `cfg(target_arch)` and
additive capabilities selected by Cargo feature.

### Target matrix

| Concern | **Standalone** (native desktop) | **Web** (`wasm32-unknown-unknown`) |
|---|---|---|
| Sim | `solo` embedded sim, `--connect`, or none | **remote only**, via WebSocket |
| Session start | any (CLI may pre-seed) | **always anonymous Viewer** (ADR-031) |
| Transport | `TcpStream` + `LengthDelimitedCodec` | `WebSocket` (binary message = one app frame) |
| HTTP | `ureq` on `AsyncComputeTaskPool` | `fetch` |
| Storage | `std::fs` cache dir + `user.json` | IndexedDB / OPFS |
| Clipboard | `arboard` | `web-sys` |
| Config | `clap` CLI + login TOML | URL query params (`?lat=&lng=&alt=`) |
| GRIB decode | server endpoint (see below) | server endpoint |
| `--shot` harness | yes | compiled out |
| Native-only extras | in-world browser etc. (ADR-030) | declared fallbacks (ADR-030) |

### Dependency topology

**Providers by target** (`cfg(target_arch = "wasm32")` — no feature flag; the target implies
the impl). Four seams, matching ADR-030: `Transport`, `HttpFetch`, `KvStore`, `Clipboard`.

**Additive capabilities become optional deps**, off for wasm:
- `solo` → `dep:cartopolis_simulator` (already a feature; wasm builds `--no-default-features`).
- `tokio` / `tokio-util` / `ureq` / `arboard` / `clap` / `dotenvy` / `toml` → optional,
  native-only, `#[cfg]`-excluded on wasm.
- `gribberish` (pure Rust, no default features) is wasm-buildable, so it needs no `#[cfg]`;
  it leaves the client anyway once the GRIB endpoint lands — see below.

**Known config points** (small but must be handled, not discovered): `getrandom` needs its `js`
backend on wasm (reached via `uuid` v4 and Bevy); `jiff` needs a bundled tzdb (no system tzdb in
a browser); `tzf-rs` is pure Rust but embeds timezone polygons — verify it builds for wasm and
weigh the binary-size cost, or put local-time behind a provider. Bevy needs a wasm feature
profile: drop `multi_threaded`, `wayland`, `x11`; target WebGPU with WebGL2 fallback. Logging
needs a wasm subscriber (console) instead of the env-filter writer.

### Server-side work the web target implies

1. **WebSocket acceptor in `cartopolis_simulator`**, alongside the existing TCP listener. Same `NetMessage`
   frames (ADR-008/009 unchanged); WebSocket is already message-framed, so one `encode_app_frame`
   per binary message and length-delimiting is skipped on that path.
2. **Serve everything from one origin — no CORS.** An earlier draft called for a "CORS edge";
   that was wrong. Path-mount the app, our services and the relay under a single hostname
   (`/tiles/*`, `/nominatim/*`, `/overpass/*`, `/3dbag/*`, `/v1/*`, `wss://…/ws`) in the existing
   Caddy and same-origin requests need no CORS headers at all. Our services are on *sub*domains
   today, which are separate origins — the migration is Caddy routes, not headers. See
   [ADR-034](./034-public-data-relay.md) for the layout.
3. **A Public-tier data relay — see [ADR-034](./034-public-data-relay.md).** A registry of
   upstream adapters (`cartopolis_relay`, lib + thin bin, embedded in `cartopolis_simulator` by default) on a
   three-level ladder: **L0** direct (third-party that sends CORS and carries no identification
   obligation — currently empty), **L1** path-mount (our services; **Caddy config, no Rust**),
   **L2** relay adapter (transform, cache, identification, or otherwise unreachable). Every
   adapter inherits a TTL cache with single-flight, stale-if-error, one project User-Agent,
   politeness spacing, timeouts and TLS termination. The audit behind it found three constraints
   that outweigh CORS: **secrets can't ship to a browser** (the Anthropic key), **mixed content**
   (OVapi is plain `http://`), and **`User-Agent` is a forbidden header** in `fetch()`.
4. **Self-host *vector* map tiles — see [ADR-035](./035-self-hosted-map-tiles.md).** OSM
   discourages proxying their CDN and expects heavy users to self-host, and the browser cannot
   send our identifying User-Agent. Tiles are our largest byte volume, so this is on Phase 1's
   critical path: OSM vector tiles (MVT) built with planetiler, stored as PMTiles, served at
   `/tiles/*`; global low zoom plus regional extracts to ~z14 with client overzoom. Decode is
   pure Rust — no C dependency on the wasm path. The client-side vector rendering path is being
   built in a separate workstream.
5. **Move GRIB decode server-side (ADR-032)** — the first **L2** adapter (`gfs-tcdc`), returning
   the decoded 360×181 TCDC grid (65 KB of `R8`, ~8 KB gzipped) instead of the ~70 KB GRIB2
   record. NOMADS sends no permissive headers, so a browser cannot fetch it directly whatever the
   decode costs (which is now pure Rust — see Context). Secondary wins: the client stops
   re-deriving the GFS cycle clock, and one warm cache serves every viewer. **Native uses the same
   endpoint**, at which point `gribberish` leaves the client for the relay.
6. **The AI assistant moves into `cartopolis_simulator`, admin-only.** `ai_assistant.rs` sends an
   `ANTHROPIC_API_KEY` from the client — impossible in a browser, and undesirable on native. It
   becomes a `NetMessage` handled by the sim, gated on ADR-031's `Authorized(ai)` capability
   (**admin users only**, since it spends real money). It is *not* a relay adapter: the relay
   stays anonymous and stateless.

### Delivery phases (mapped to ADR-031 tiers)

- **Phase 1 — Web Viewer (Public tier). No `cartopolis_simulator` changes at all.** Land `HttpFetch` +
  `KvStore` providers, the wasm Bevy profile, and the optional-dep split; build with
  `--no-default-features`. The whole Public tier is client→external-service, so this ships the
  anonymous map/lookup/layers viewer on the web **before** any transport or protocol work.
  Prerequisites from the list above: the single-origin layout (2), the relay with its first
  adapters (3, 5), and **self-hosted tiles (4) — the long pole**, since a compliant base map is
  what makes a public web viewer possible at all.
- **Phase 2 — Web Simulation (Authenticated tier).** `Transport` provider (client WebSocket) +
  `cartopolis_simulator` WS acceptor. Login becomes the ADR-031 runtime upgrade; avatars, prims, editing and
  chat come with it.
- **Phase 3 — Native-only extras.** In-world browsers and friends as ADR-030 additive plugins,
  each shipping its web fallback in the same change.

**CI**: add `cargo check --target wasm32-unknown-unknown -p cartopolis --no-default-features` to
the existing Forgejo Actions fast-lane, so the web target cannot rot silently. Standalone
release binaries keep the existing tag-driven Linux release workflow.

## Rationale

**Primary Reasoning**:
1. **Phase 1 is decoupled from the hard work.** Because ADR-031 made the Viewer tier
   sim-independent, the web build delivers real value with zero protocol/server changes. The
   access model *is* the wasm de-risking strategy.
2. **The uniform fetch pattern makes the big number small.** 14 call sites across 10 files look
   daunting until you notice they are one shape; a single `HttpFetch` seam converts them all and
   improves native too (task pool instead of 15 ad-hoc OS threads).
3. **Client SQLite is already gone**, so the remaining "native authority" is one Cargo feature
   (`solo`) — the topology is far closer to buildable than it was.
4. **Server-side GRIB decode is better on both targets** (P-03): it solves CORS (the hard
   blocker), collapses N client fetches of NOMADS into one cached one, and lets the client
   drop a decoder it only needs for one 65 KB grid. The C dependency it was also going to
   remove is already gone (`default-features = false`), so this is now a simplification with
   a deadline set by web, not a prerequisite.
5. **`cfg(target_arch)` for providers, features for capabilities** keeps the two axes from
   tangling: platform impl is implied by the target, while capability presence stays a
   deliberate build choice (and matches ADR-030's split).

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| Separate `cartopolis-web` crate | Clean isolation | Duplicate feature logic; drift | Violates P-05; the whole point is one codebase |
| Web build waits for the WS transport | One big launch | Delays all web value behind server work | Phase 1 needs no server change; shipping earlier de-risks |
| Keep GRIB decode client-side, `gribberish` `default-features = false` | No server work; drops the C dependency today; decode verified unchanged (template 5.3) | NOMADS CORS still blocks the browser; every client re-fetches | **Adopted for native now**, but it does not unblock web — the endpoint still lands for CORS |
| Hand-roll a template 5.3 decoder to drop `gribberish` too | Zero GRIB dependency | ~150 lines of bit-level packing + spatial differencing to own; breaks silently if NCEP repacks | `gribberish` minus its C features is already pure Rust and wasm-clean — no benefit left to buy |
| Switch to the NOMADS OPeNDAP endpoint (plain float arrays, no GRIB) | No GRIB decode at all | Same CORS wall; 260 KB float32 vs 70 KB packed; NOMADS rate-limits DODS harder | Trades a solved problem for a bigger payload |
| Cargo feature (`web`) instead of `cfg(target_arch)` for providers | Explicit | Redundant with the target; easy to build an incoherent combo | Target already determines the impl |
| Proxy *everything* through our edge | Uniform | Bandwidth and operational cost for tiles we don't need to proxy | Proxy only what CORS forces (P-03) |

## Consequences

**Positive**:
- A browser-reachable anonymous viewer (G-02 reach, G-04 geo) with no protocol change.
- Native sheds a C dependency and 15 ad-hoc threads; one fetch path on both targets.
- One session flow across targets (ADR-031); one crate, one set of feature logic.
- Web and native diverge only at four provider seams plus additive plugins — a small, reviewable
  surface.

**Negative**:
- **New ops surface**: a CORS/proxy edge and a GRIB decode endpoint to run and monitor
  (mitigation: both extend the existing garage44 Caddy + self-hosted-service pattern).
- **Provider refactor touches 10 fetch files** (mitigation: mechanical and uniform; do it once,
  behind `HttpFetch`, with native impl first so nothing changes behaviourally).
- **Bevy wasm profile is a second render configuration** to keep working — WebGPU/WebGL2 paths
  can diverge from the native Vulkan path (mitigation: CI check; treat visual parity as
  best-effort, not a gate).
- **Binary/asset size** on web (tzf-rs data, avatar glTFs in LFS, LUTs) needs a streaming asset
  story (mitigation: Phase 1 is map-only — avatars belong to the Phase 2 Simulation tier).
- `--shot` and the CLI are native-only, so scripted capture cannot verify the web build
  (mitigation: the web build renders the same scene graph; verify visuals natively).

## Related

- [ADR-030](./030-platform-capability-model.md): The pattern this applies — provider seams and additive plugins with fallbacks.
- [ADR-031](./031-layered-access-model.md): Supplies the phase boundaries; the Viewer tier is what makes Phase 1 server-free.
- [ADR-008](./008-network-transport-layer.md): Gains a WebSocket variant; the envelope (ADR-009) is unchanged.
- [ADR-016](./016-authentication-and-session-model-v0.md): Login as the runtime upgrade that enters Phase 2's Simulation tier.
- [ADR-032](./032-global-cloud-cover-data.md): Its `openjpeg-sys` C dependency is dropped
  outright (`gribberish` `default-features = false`, template 5.3 needs no optional feature);
  the remaining pure-Rust decode moves server-side with the GRIB endpoint.
- [ADR-023](./023-data-layers.md): Public-tier layers are the web build's Phase 1 payload.
