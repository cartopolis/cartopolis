# ADR-031: Layered Access Model (Anonymous Viewer → Login → ACL Layers)

---
**Metadata:**
- **ID**: ADR-031
- **Status**: Proposed
- **Date**: 2026-07-22
- **Tags**: [architecture, layers, session, auth, authz, acl, viewer, entitlements]
- **Decision Type**: architecture_pattern
- **Aligns with**: [G-04, G-02, G-05, P-04, P-01, P-05]
- **Roadmap tier**: P1
- **Doc resolution**: R2
- **Related**: [ADR-016, ADR-020, ADR-023, ADR-030, ADR-034, ADR-007]
- **Supersedes**: []
- **Superseded By**: []
---

## Context

**Problem**: The client is a **multi-faceted viewer** whose facets have different access
requirements, but today the boot model offers only three all-or-nothing modes (`--connect`,
default `solo`, `--offline` — `main.rs:302`) and **login is effectively mandatory**: even to
browse the map in the default mode you spin up a full `vibe_sim` and authenticate. There is no
first-class **anonymous viewer** that just looks things up, routes, and browses public layers;
and no articulated path for **login** to *add* the simulation, nor for **per-user ACLs** to
add restricted layers later.

The building blocks exist but are ungoverned: the map tier and data layers already run
independent of any sim (ADR-020; layer systems are ungated in `main.rs`), data layers are
already a **declarative registry** (`LAYER_DEFS`/`LayerDef`, ADR-023), and place lookup exists
(self-hosted Nominatim, `geo_nav.rs`). Routing is desired but **not yet built**. What's missing
is a model that says *which facets are available to whom*, and lets that grow monotonically:
anonymous → authenticated → authorized.

This must not be conflated with **platform capability** (ADR-030): *can the runtime do it* is a
different question from *is the user allowed it*.

**Requirements**:
- An **anonymous** facet: place lookup, routing, and public browsable layers — **no login**,
  and the default state of the web build.
- **Login adds** the simulation layer (avatars, prims, editing, chat) on top, as a **runtime
  upgrade**, not a launch-time mode.
- **Future ACL layers**: additional layers unlocked per-user by role/entitlement.
- Restricted layers are **server-enforced**, not merely hidden client-side (P-04, P-01).
- Reuse the existing layer registry (ADR-023) rather than a parallel mechanism (P-05).

## Decision

Model the viewer as a **stack of layers**, each declaring an **access tier**; the session
carries a monotonically growing set of **entitlements** that selects which layers are active.

**Two tiers are primary today — the `Viewer` (no login) and the `Simulation` (login)** — with an
**Authorized** sub-tier for per-user ACL grants. **Avatars and chat live behind the Simulation
tier**: they require a logged-in session.

The Authorized tier has its **first concrete case**: the **AI assistant is admin-only**. It holds
a server-side secret (the Anthropic API key) and spends real money per request, so it is gated on
an `ai` capability granted only to admin users — not merely on being logged in. See ADR-034,
which routes it through `vibe_sim` over `NetMessage` rather than the anonymous data relay.

**Access tiers** (a layer declares exactly one):

| Tier | Product name | Unlocked by | Examples | Enforcement |
|------|--------------|-------------|----------|-------------|
| **Public** | **Viewer** | nothing (anonymous) | map tiles, place lookup, routing, public POI/transit data layers | none — external services hit directly by the client |
| **Authenticated** | **Simulation** | any logged-in session | the sim tier (ADR-020): **avatars, prims, editing, chat** | server: sim only serves/accepts for authenticated sessions |
| **Authorized(cap)** | *(first case: `ai`)* | a specific ACL/role grant | **AI assistant (admin-only)**; later: private worlds, moderator overlays, org data | server: sim withholds data / rejects intents unless the session holds `cap` |

**Session entitlements** grow, never shrink within a session:
`{Public}` (anonymous) → `+Authenticated` on login → `+Authorized(cap)` per granted capability.
Login is a **runtime transition** (a button), not a CLI mode — the app **starts anonymous**
(always, and necessarily so for the wasm build) and *upgrades*.

**Two orthogonal gates.** A layer is live **iff** `platform-supported (ADR-030) ∧ entitled
(this ADR)`. Keep them as separate predicates and separate resources — do **not** fold access
tiers into ADR-030's platform `Capabilities`. One answers "can this build render it," the other
"is this user allowed it."

**Enforcement rule (load-bearing).** For Authenticated/Authorized tiers the **server is the
gate**: `vibe_sim` withholds layer data and rejects mutating intents for sessions lacking the
entitlement (P-01, P-04). Client-side layer toggles are **UX only** (show/hide, grey-out an
unavailable action) and carry no security weight. Public-tier layers are external services
(tiles, Nominatim, routing) the client calls directly and need no entitlement.

**Implementation shape.** Generalize the existing `LayerDef` registry (ADR-023) so every layer
— map tiles, place lookup, routing, POI/transit overlays, the simulation layer — is a registry
entry carrying an `access: Tier`. A `SessionEntitlements` resource holds the current set; a
run-condition activates each layer's systems on `platform_supported(layer) &&
entitlements.grants(layer.access)`. The "simulation layer" is the `Authenticated` entry and
maps onto the existing sim connection path (ADR-016 auth is what flips it on).

## Rationale

**Primary Reasoning**:
1. **Matches the product's real shape.** viberfox is a place-lookup/routing/map viewer *and* a
   virtual world; the tiered stack lets the anonymous viewer be genuinely useful (G-04) while
   login unlocks the world (G-02) and ACLs unlock more (G-05) — one model, three depths.
2. **Web-native default.** "Anonymous, public layers only" is precisely the wasm build's base
   state (ADR-030); login as a runtime upgrade means the web and native flows are identical.
3. **Security lives server-side (P-04, P-01).** Making entitlement a client toggle would be a
   paper wall; the sim-as-gate rule keeps restricted layers actually restricted and slots into
   the authz work the capability map already flags as a gap after ADR-016.
4. **Reuses an existing seam (P-05).** Layers are already a declarative registry (ADR-023);
   adding `access:` extends it instead of forking a new system.

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| Binary Viewer vs Connected mode | Simple | No room for ACL layers; re-couples "logged in" with "can see layer X" | Doesn't express per-layer, per-role access |
| CLI launch modes only (today) | Already built | Login is boot-time, not a runtime upgrade; no anonymous facet | Can't land on a page and browse without login |
| Client-side entitlement checks | Easy to implement | Trivially bypassed | Violates P-04 for restricted layers |
| Fold access into ADR-030 `Capabilities` | One resource | Conflates "can render" with "is allowed"; muddies both | Two orthogonal concerns must stay separate |

## Consequences

**Positive**:
- A useful anonymous viewer (lookup/routing/public layers); login and ACLs are additive depth.
- Web and native share one session flow (start anonymous, upgrade at runtime).
- Restricted layers are enforced where authority lives (`vibe_sim`).
- New layers pick a tier declaratively; no bespoke gating per feature.

**Negative**:
- **Login becomes a runtime transition**, changing today's boot-time mode selection in
  `main.rs` (mitigation: keep CLI flags as shortcuts that pre-seed the transition).
- **Server authz surface**: the sim must key layer data/intents on entitlements — real work
  beyond ADR-016's authn (mitigation: land Public+Authenticated first; `Authorized(cap)` when
  roles exist).
- **Two-gate discipline**: contributors must set both `access:` (tier) and platform support per
  layer (mitigation: both are fields on the registry entry; a missing tier fails closed to the
  most restrictive).
- Routing is a **new** Public service to stand up (mitigation: additive; place lookup already
  exists as the pattern).

## Related

- [ADR-020](./020-map-tier-and-sim-tier-world-model.md): The simulation layer *is* the sim tier; map tier + data layers are the Public base.
- [ADR-023](./023-data-layers.md): The `LayerDef` registry this generalizes with an `access:` tier.
- [ADR-016](./016-authentication-and-session-model-v0.md): Authn is the trigger that adds the Authenticated tier; ACL grants extend it toward authz.
- [ADR-030](./030-platform-capability-model.md): The orthogonal gate — platform capability vs access entitlement; a layer is live iff both allow it.
- [ADR-007](./007-simulation-vs-client-process-model.md): The anonymous viewer needs no sim; the Authenticated tier is where the client/sim split engages.
