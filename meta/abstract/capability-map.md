# Capability map

Themes connect **vision goals** to **ADRs**. Rows **without** ADRs are **intentional**: add an ADR only when [roadmap.md](roadmap.md) tier and [adr-resolution.md](adr-resolution.md) say so, then paste the **ADR id** here.

**Priority:** P0 / P1 / P2 from roadmap.

**Resolution (target):** R0–R3 — see [adr-resolution.md](adr-resolution.md).

**ADRs:** `—` means no decision record yet for that theme.

| Theme | Goals | Priority | Resolution (target) | Status | ADRs | Notes |
|-------|-------|----------|---------------------|--------|------|-------|
| World model & geo | G-04, G-02 | P0 | R2+ | partial | 006, 004 | |
| Interest & replication | G-02 | P0 | R2+ | partial | 012, 011 | |
| Networking & envelope | G-02, P-02 | P0 | R2+ | partial | 008, 009 | |
| Sim vs client | G-02, P-05 | P0 | R2 | partial | 007 | |
| Avatars & authority | G-02, G-03, P-01 | P0 | R2+ | partial | 010 | |
| Persistence & migrations | G-01, G-02 | P0 | R2+ | partial | 002, 013 | |
| Runtime & ops | P-03 | P1 | R2 | partial | 014 | |
| Workspace boundaries | P-05 | P0 | R2 | partial | 015 | |
| Platform portability & web target | G-02, G-03, P-05 | P1 | R2 | proposed | 030 | Substitutable providers + additive plugins; wasm online-only |
| Layered access & entitlements | G-04, G-02, G-05, P-04 | P1 | R2 | proposed | 031 | Anonymous viewer → login (sim layer) → ACL layers; server-enforced |
| Rendering & client UX | G-01, G-03 | P2 | R2 | partial | 001, 003, 005 | |
| Asset pipeline & storage | G-01, G-05 | P0 | R2+ | gap | — | Add ADR(s) per roadmap before rich assets |
| AuthN | G-05, P-04 | P0 | R2+ | partial | 016 | Session / identity; authorization in a later ADR |
| AuthZ / world edits | G-05, P-01, P-04 | P0 | R2+ | partial | 016 | v0: any authenticated principal = same mutation rights (ADR-016); roles/ACLs later |
| Chat & social presence | G-03 | P2 | R1 | gap | — | After core loop |
| Voice / WebRTC / video | G-03, P-07 | P2 | R1 | gap | — | After auth + ops basics |
| LLM-assisted generation | G-06, P-06 | P1 | R1→R2+ | gap | — | Thin ADR first; split after spikes |
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
