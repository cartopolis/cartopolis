# Capability map

Themes connect **vision goals** to **ADRs**. Rows **without** ADRs are **intentional**: add an ADR only when [roadmap.md](roadmap.md) tier and [adr-resolution.md](adr-resolution.md) say so, then paste the **ADR id** here.

**Priority:** P0 / P1 / P2 from roadmap.

**Resolution (target):** R0–R3 — see [adr-resolution.md](adr-resolution.md).

**ADRs:** `—` means no decision record yet for that theme.

**Status:** `partial` = built and still evolving · `proposed` = ADR written, not built yet · `gap` = neither · **`shipped, undocumented`** = built **without** a decision record, so an ADR is owed.

| Theme | Goals | Priority | Resolution (target) | Status | ADRs | Notes |
|-------|-------|----------|---------------------|--------|------|-------|
| World model & geo | G-04, G-02 | P0 | R2+ | partial | 019, 020, 025, 004 | Floating origin + map/sim tiers + globe; **supersedes 006**. 021 (fixed region grid) retired |
| Prim authoring & editing | G-01, G-02, P-01 | P0 | R2+ | partial | 017, 018, 029 | Shapes + path-cut/hollow/twist/taper/slice, gizmo, edit dialog, texture catalog; **029 proposed** (geo-anchored storage) |
| Interest & replication | G-02 | P0 | R2+ | partial | 012, 011 | |
| Networking & envelope | G-02, P-02 | P0 | R2+ | partial | 008, 009 | |
| Sim vs client | G-02, P-05 | P0 | R2 | partial | 007 | |
| Avatars & authority | G-02, G-03, P-01 | P0 | R2+ | partial | 010, 036 | Simulation tier (ADR-031) — requires login. **036 proposed**: input-sequenced prediction + reconciliation |
| Persistence & migrations | G-01, G-02 | P0 | R2+ | partial | 002, 013 | |
| Runtime & ops | P-03 | P1 | R2 | partial | 014 | |
| Workspace boundaries | P-05 | P0 | R2 | partial | 015 | |
| Platform portability & web target | G-09, G-02, P-05 | P1 | R2 | proposed | 030, 033 | 030 = provider/plugin pattern; 033 = standalone + wasm32 targets, phased (Viewer first, no server change) |
| Public data relay & upstream policy | G-04, P-03, P-05 | P1 | R2 | proposed | 034 | `cartopolis_relay` upstream registry (L0 direct / L1 path-mount / L2 adapter); single origin, shared cache, one UA |
| Map tile hosting | G-04, P-03 | P1 | R2 | partial | 035 | Self-hosted **vector** tiles (Shortbread MVT); **server live** at `tiles.cartopolis.org` (mirror of VersaTiles' 62 GB planet, `versatiles` server in the geo compose stack); client-side styling responds to day/night + weather; overzoom past ~z14 |
| Layered access & entitlements | G-09, G-05, G-02, P-04 | P1 | R2 | proposed | 031 | Two tiers: **Viewer** (no login) / **Simulation** (login: avatars, prims, editing, chat); + future ACL; server-enforced |
| Buildings from map data | G-04, P-03 | P1 | R1 | partial | 022, 027 | Overpass extruded footprints; 3DBAG LoD2.2 where covered |
| Data layers & live feeds | G-04, P-03 | P1 | R1 | partial | 023, 024, 026 | Togglable POI overlays; OVapi departure boards and GTFS-RT vehicles |
| Weather & sky realism | G-04, P-03 | P2 | R1 | partial | 005, 032 | met.no local weather; global GFS cloud cover driving the procedural layer |
| Routing & navigation | G-04, P-01, P-03 | P1 | R2 | partial | 037 | Routing computed on the same vector tiles the map renders — no routing service |
| Terrain elevation | G-04, G-02, P-03 | P1 | R2 | proposed | 038 | Pluggable height-field providers, AHN first; affects geometry **and** collision |
| Rendering & client UX | G-01, G-03 | P2 | R2 | partial | 001, 003, 005 | |
| Asset pipeline & storage | G-01, G-05 | P0 | R2+ | gap | — | Add ADR(s) per roadmap before rich assets; ADR-021 also defers a **capacity-bounded asset ADR** that is still unwritten |
| AuthN | G-05, P-04 | P0 | R2+ | partial | 016 | Session / identity; authorization in a later ADR |
| AuthZ / world edits | G-05, P-01, P-04 | P0 | R2+ | partial | 016 | v0: any authenticated principal = same mutation rights (ADR-016); roles/ACLs later |
| Chat & social presence | G-03 | P2 | R1 | partial | 028 | Simulation tier (ADR-031); Local/Global/Rooms built (v11), geo tiers pending cartopolis_geo |
| Voice / WebRTC / video | G-03, P-07 | P2 | R1 | gap | — | After auth + ops basics |
| LLM-assisted generation | G-06, P-06 | P1 | R1→R2+ | **shipped, undocumented** | — | In-client copilot exists (`cartopolis/src/systems/ai_assistant.rs`: `create_prim` / `update_prim` / `delete_prim` / `list_prims` tool calls). The **R1 boundary ADR is overdue** — write it, then split R2+ children for action schema, model boundary, review UX |
| Companion / GNSS | G-07, P-07 | P2 | R1 | gap | — | Privacy before R2 |
| Scripting & integrations | G-08, P-04 | P1 | R1→R2 | gap | — | |
| Observability & abuse signals | P-03, P-04 | P1 | R2 | gap | — | With rate limits |
| Protocol rate limits & quotas | P-03, P-04 | P1 | R2 | gap | — | Before wide exposure |

## Related

- [roadmap.md](roadmap.md)
- [adr-resolution.md](adr-resolution.md)
- [vision.md](vision.md)
- [principles.md](principles.md)
- [../adr/index.md](../adr/index.md)
