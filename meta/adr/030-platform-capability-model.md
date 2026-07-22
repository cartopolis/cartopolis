# ADR-029: Platform Capability Model (Substitutable Providers + Additive Plugins)

---
**Metadata:**
- **ID**: ADR-029
- **Status**: Proposed
- **Date**: 2026-07-22
- **Tags**: [architecture, platform, wasm, web, capabilities, portability, plugins]
- **Decision Type**: architecture_pattern
- **Aligns with**: [G-02, G-03, P-05, P-02]
- **Roadmap tier**: P1
- **Doc resolution**: R2
- **Related**: [ADR-007, ADR-008, ADR-015, ADR-016]
- **Supersedes**: []
- **Superseded By**: []
---

## Context

**Problem**: We want a browser (`wasm32`) build without foreclosing native-only features
(in-world browsers, local file import, the embedded solo sim), and without letting the
browser's limits dictate the whole architecture. Today the client assumes native primitives
throughout — raw TCP (`systems/network.rs`), an in-process `vibe_sim` over loopback
(`solo.rs`, ADR-016), blocking `ureq` on `std::thread::spawn` (tile/transit/overpass/lod22
loaders), `std::fs` caches, and `rusqlite` for offline mode. None of these exist in a browser
sandbox. The naive fixes pull in opposite directions: design for the intersection of both
platforms and native-only features become impossible; design for native and the web build
won't compile.

The framing trap is treating native as a **superset** of web. It isn't — opening a real link
in a new tab or compositing an `<iframe>` is trivial on web and hard on native. Each platform
has a **capability set**; neither contains the other.

**Requirements**:
- A web build that compiles and runs, online-only, without `#[cfg(wasm)]` smeared through
  gameplay/feature code.
- Native-only features (starting with in-world browsers) add cleanly and never break the web
  build — they degrade to a declared fallback.
- The **data model and wire protocol** (ADR-002, ADR-008/009, ADR-015) stay platform-neutral:
  a feature's shared state is portable even where its live rendering is not.
- UI degrades **visibly** (offer the fallback affordance), never silently no-ops.
- Both targets stay green in CI so platform splits don't rot silently (cf. the egui test-rot
  war story in CLAUDE.md).

## Decision

Adopt a two-tier capability model. Every platform-varying concern is classified as one of:

- **Substitutable provider** — both platforms do it, differently. Define a **trait** (the
  seam), select the impl at **build time**, and feature code programs against the trait with
  **no platform branching**. First instance: the network transport (ADR-008) becomes a
  `Transport` provider — native `TcpStream`, wasm `WebSocket`/WebTransport — behind the same
  `NetMessage` codec, which does not change. Other providers: `HttpFetch` (native
  `ureq`+task-pool vs wasm `fetch`), `KvStore` (native `std::fs` vs wasm IndexedDB/OPFS),
  `Clipboard`.

- **Additive capability** — only some platforms have it at all (in-world browsers, embedded
  solo sim, local file import, mic capture). Modeled as **portable data** + a
  **capability-gated Bevy plugin** + a **mandatory fallback**. The native-only dependency
  (e.g. `wry`/CEF) lives *only* inside that plugin, which the wasm `App` never adds. The
  fallback plugin ships in the **same change** as the capability — the web build is never
  broken, only lighter.

Supporting rules:

- **Data in `vibe_core`, capability in the client (ADR-015).** An in-world browser is, in the
  data model, a prim property: a `MediaSurface { url, nav_state, poster, … }` DTO that
  serializes over the wire and into SQLite. The sim and *both* client builds understand a prim
  can carry media; only the code that turns that data into pixels is platform-specific. The
  protocol never learns the word "CEF." Shared state stays server-authoritative (P-01), which
  keeps the door open to the heavy variant (native peer/server streaming live frames to web)
  with **no** protocol change.
- **Runtime `Capabilities` resource.** Build-time cfg chooses which plugins load; a
  `Capabilities` Bevy resource lets UI/systems query presence so they degrade visibly —
  the context menu shows "Add in-world browser" on native and "Open media ↗" on web, rather
  than offering an action that silently does nothing.
- **Splits live at the edges.** `#[cfg(target_arch = "wasm32")]` belongs in a handful of
  provider modules and plugin registration, not inside gameplay systems. A `#[cfg]` reaching
  into a feature system is the signal that concern wanted to be a trait or a plugin.

Web build is necessarily **online-only**: `solo` (ADR-016), `--offline`, and all `rusqlite`
usage are additive capabilities the wasm `App` omits; it connects to a remote `vibe_sim` via
the `WebSocket` transport provider.

## Rationale

**Primary Reasoning**:
1. **No lowest-common-denominator tax.** Additive capabilities let native keep full-fat
   features; the fallback contract keeps web shipping. We design for the *union*, gated by
   capability — not the intersection.
2. **Extends work already sanctioned.** The transport abstraction is needed for web regardless
   (ADR-008); making it the first `Transport` provider costs nothing extra and sets the
   pattern. `solo` (ADR-016) is already a build-time capability gate — this generalizes it.
3. **Protocol/data neutrality is preserved (P-02, P-05).** Portable DTOs mean a web user sees
   *what* a surface shows even when it can't render live, and the wire format is untouched.
4. **Graceful degradation is a UI contract**, not an accident — the `Capabilities` resource
   makes "what can I offer here" a first-class query.

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| Design for the intersection only | Simplest; one code path | Native-only features become impossible | Directly causes the limitation the ADR exists to prevent |
| Native-first, bolt on wasm later | Fastest short-term | Web build won't compile; retrofit touches everything | Platform assumptions calcify into core loop |
| `#[cfg]` inline per call site | No new abstraction | Rots silently; smears platform logic through features | The egui test-rot failure mode, codebase-wide |
| Separate web fork/crate | Clean isolation | Duplicate feature logic; drift | Violates single-source feature code (P-05) |

## Consequences

**Positive**:
- Web target reachable online-only without gutting native features.
- New native-only features have a known shape: portable DTO + gated plugin + fallback.
- Transport, HTTP, and storage seams make the client testable with mock providers.
- Server-authoritative shared state keeps richer variants (live-frame streaming to web) open.

**Negative**:
- Up-front seam cost: `Transport`, `HttpFetch`, `KvStore` traits + two impls each
  (mitigation: introduce lazily, transport first as it's already required).
- Split-rot risk (mitigation: CI runs `cargo check --target wasm32-unknown-unknown`; keep
  splits at provider edges so the surface is small).
- Fallback discipline: every additive capability owes a fallback in the same PR (mitigation:
  reviewer checklist item; `Capabilities`-driven UI makes a missing fallback visible).
- A separate wasm Bevy feature profile is needed (no `multi_threaded`/`wayland`/`x11`).

## Related

- [ADR-008](./008-network-transport-layer.md): Transport becomes the first substitutable provider; codec (ADR-009) unchanged
- [ADR-015](./015-workspace-module-boundaries.md): Portable data in `vibe_core`, capability impls in `viberfox`
- [ADR-016](./016-authentication-and-session-model-v0.md): `solo` feature is the precedent build-time capability gate; embedded sim is additive/native-only
- [ADR-007](./007-simulation-vs-client-process-model.md): Web client keeps the real client/sim split (remote sim)
