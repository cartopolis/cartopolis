# ADR resolution (how deep to document)

Not every concept deserves the **same documentation depth**. **Resolution** is how fine-grained we split intent into ADRs and how much detail each ADR carries. **Roadmap tier (P0–P2)** and **resolution (R0–R3)** together decide **whether** an ADR exists and **how big** it is — not the other way around.

## From roadmap to ADR

1. Pick a **theme** in [roadmap.md](roadmap.md) and [capability-map.md](capability-map.md).
2. Note its **priority tier** and **target resolution** (capability map column).
3. **R0** → no new ADR; update roadmap/capability text only if intent changes.
4. **R1+** → add **one** new ADR at the next free number in [index.md](../adr/index.md); use [template](../adr/template.md) depth matching **R1** vs **R2** vs **R3** (split = multiple new files).
5. Set metadata **Roadmap tier** and **Doc resolution** on that ADR; fill **Aligns with** `G-*` / `P-*`.
6. Link the ADR id in the capability map row and adjust **status** (partial / done).

**Do not** create a batch of ADRs for “completeness” ahead of priority — add them as work or risk **forces** recorded decisions.

## Resolution levels

| Level | Name | Use when | Typical output |
|-------|------|----------|----------------|
| **R0** | Abstract only | Idea is **real** but **unscheduled** or **blocked on unknowns** | Bullet in [roadmap.md](roadmap.md) or capability map; **no** dedicated ADR yet |
| **R1** | Thin ADR | Direction is agreed; **details will change** with spikes | Short ADR: problem, **one** decision, explicit **non-decisions**, link to roadmap tier |
| **R2** | Standard ADR | Trade-offs matter; implementers need **alternatives and consequences** | Full [template](../adr/template.md) sections |
| **R3** | Split / family | Subsystem is **essential** or **high risk**; one file would hide decisions | Multiple ADRs (e.g. transport vs envelope) or ADR + linked appendix |

**Rule:** **Raise** resolution (R0→R1→R2→R3) when **implementation starts** or **risk** (security, perf, data loss) **forces** a recorded choice. **Lower** or **merge** when ADRs duplicate the same decision or stay forever “Proposed” with no unique content.

## How to choose resolution

1. **Check [roadmap.md](roadmap.md) tier** (P0 / P1 / P2). P0 topics trending to **R2+** before serious code. P2 topics can stay **R0–R1** until promoted.
2. **Essential path to “visit a shared world with authored content”** (assets + auth + replication + protocol) → prefer **R2 or R3** on the codebase hot path.
3. **Social / media** that are **explicitly later** → **R0** or thin **R1** until tier rises.
4. **LLM in the product (G-06)** → important but often **unknown shape** early: first ADR at **R1**; add **R2+** ADRs only after spikes define **action schema**, **model boundary**, and **operator UX** (see roadmap).
5. **Capability map** **Priority** and **Resolution** columns define urgency and doc depth — many themes can stay **gap** until you deliberately add an ADR.

## Related

- [roadmap.md](roadmap.md) — priorities and sequencing
- [capability-map.md](capability-map.md) — themes ↔ ADRs
- [README.md](README.md) — when any ADR is required (discipline)
