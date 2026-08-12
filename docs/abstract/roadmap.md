# Roadmap (priorities and ADR sequencing)

This file ranks **capabilities** and ties them to **documentation depth** (see [adr-resolution.md](adr-resolution.md)). **ADR numbers are not pre-allocated here** — you add an ADR when a tier + resolution calls for a recorded decision, then link it from [capability-map.md](capability-map.md).

## Priority tiers

| Tier | Meaning |
|------|---------|
| **P0** | **Must** land for a credible **shared, authored, visitable** world with **safe edits** |
| **P1** | **Important** soon: operations, safety for multi-user, **accelerated authoring** (incl. LLM) once shape is known |
| **P2** | **Valuable** but **after** core loop; stay at **R0** or thin **R1** ADRs until promoted |

## P0 — core loop

Capabilities that already have decision records list **ADR** ids. Others say **when to add ADRs** (tier + target resolution); create **new numbered ADRs** only when you start that work.

| Capability | Recorded in ADRs | When adding ADRs |
|------------|------------------|------------------|
| World coordinates & world model | 019, 020, 025 | **006 is superseded** by these; 021 (fixed region grid) is retired. Extend or supersede as implementation evolves (**R2+**) |
| Replication & interest | 011, 012 | Same |
| Transport & protocol envelope | 008, 009 | Same |
| Authoritative avatar | 010 | Same; **036** (input-sequenced prediction) is the open follow-up |
| **Prim authoring & editing** | 017, 018 | Core authoring loop is built. **029** (geo-anchored prim storage) is **proposed** — accept or supersede before prims spread past the current anchor model |
| Persistence & migrations | 002, 013 | Same |
| Sim vs client, workspace | 007, 015 | Same |
| **Asset storage & delivery** | *none* | **Before** general mesh/texture upload: add **R2** (or **R3** if split) ADR(s) for identity, fetch path, upload + integrity. ADR-021 also defers a **capacity-bounded asset ADR** that is still unwritten. Prims + the texture catalog (017/018) cover authoring today; **upload** is the gap |
| **Authentication** | 016 | v0 accepted and implemented (`cartopolis_simulator/src/auth.rs`). Extend **016** when adding OAuth/OIDC, MFA, or federation; **R2+** as features harden |
| **Authorization / world edits** | 016 (implicit v0), 031 (tiers) | **R2** ADR for **roles/capabilities** when the mutating surface needs more than one trust level; until then ADR-016’s “authenticated = full v0 mutator” applies, split by the Viewer/Simulation tiers of **031** |

## P1 — scale, safety, fast iteration

| Capability | Recorded in ADRs | When adding ADRs |
|------------|------------------|------------------|
| Runtime config & ops | 014 | Extend as needed |
| **Platform portability & web target** | 030, 033 | Both **proposed** while the wasm32 build lands. Accept once the browser path is real; add child ADRs per capability that needs a substitutable provider |
| **Layered access (Viewer → login → ACL)** | 031 | **Proposed.** Promote to accepted when the server enforces the tier split; the ACL layer is the **R2** authorization ADR above |
| **Map tile hosting & data relay** | 034, 035 | 035 accepted (tiles live at `tiles.cartopolis.org`); **034 proposed** — accept when `cartopolis_relay` exists, or fold into 014 if it stays a config concern |
| **Buildings & data layers** | 022, 023, 024, 026, 027 | Extend at **R1**; promote to **R2** if the import/refresh path or an upstream dependency becomes load-bearing |
| **Routing & navigation** | 037 | Extend as the on-tile router grows; **R2** already |
| **Terrain elevation** | 038 | **Proposed.** Touches geometry *and* collision — accept before building on top of a flat ground plane |
| **Client prediction & latency** | 036 | **Proposed.** Accept alongside the next avatar-authority change, not before |
| **LLM-constrained world ops** | *none* — **overdue** | The spike **already shipped** (`cartopolis/src/systems/ai_assistant.rs`: prim tool-calls, in-client panel). Write the **R1** boundary ADR now — action schema, validation, permission scope, oversight — then split **R2+** children as the surface grows |
| **Rate limits & quotas** | *none* | **Before** wide exposure: **R2** ADR. The web target (033) brings that exposure closer |
| **Observability & abuse signals** | *none* | **Alongside** limits: **R2** ADR |
| **Scripting / integrations** | *none* | **R1** until demand is clear; **R2** when embedding and API surface stabilize |

## P2 — social & world extensions

| Capability | Recorded in ADRs | When adding ADRs |
|------------|------------------|------------------|
| Text chat | 028 | Built (Local/Global/Rooms). Geo-derived channels still pending; deepen to **R2** if the protocol or abuse story grows |
| Weather & sky realism | 005, 032 | Extend at **R1** |
| WebRTC / media | *none* | **R1** roadmap-only until auth + observability basics exist; then dedicated ADR(s) |
| Companion / GNSS | *none* | **R1** first; **R2** after privacy/threat model |
| Rendering stack | 001, 003, 004, 005 | Evolve with accepted pattern; 004 (raster region textures) is largely displaced by the vector-tile path in 035 — supersede it when the raster path is removed |

## Sequencing hints

1. Do **not** block **assets** on **chat** or **WebRTC**.
2. **Authentication → authorization**: auth (016) is in; the **roles/ACL** ADR is the next gate before asset **upload** or a wider mutating surface.
3. **LLM**: the spike is done and shipping — the **R1 boundary ADR is now the blocker**, not the spike. **Split** by concern when promoting to **R2+**.
4. **Web exposure raises the floor**: 033 puts the client in front of anonymous users, which pulls **rate limits**, **observability**, and **031**’s tier enforcement forward.
5. When a theme moves **P2 → P1** (or **P1 → P0**), update this file and [capability-map.md](capability-map.md), then add or deepen ADRs per [adr-resolution.md](adr-resolution.md).
6. **Shipped without an ADR is a bug in the docs.** If a capability lands in code with no decision record, it belongs here with an explicit target resolution until one exists.

## Related

- [vision.md](vision.md) — `G-*` goals
- [adr-resolution.md](adr-resolution.md) — R0–R3 and **workflow**
- [capability-map.md](capability-map.md) — link new ADRs here
- [../adr/index.md](../adr/index.md) — decision log
