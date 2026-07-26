# Abstract (strategic foundation)

This directory holds **slow-moving** product and engineering intent. **[`docs/adr/`](../adr/)** holds **point-in-time** decisions. Together: **abstract constrains and prioritizes; ADRs record what we chose.**

**Canonical layout:** [`docs/`](../README.md) → **`abstract/`** (here) + **`adr/`** (decision log).

## Read order (humans and agents)

1. [vision.md](vision.md) — mission, goals (`G-*`), non-goals
2. [principles.md](principles.md) — engineering principles (`P-*`)
3. [roadmap.md](roadmap.md) — **P0 / P1 / P2** and ADR sequencing
4. [adr-resolution.md](adr-resolution.md) — **R0–R3** depth; which concepts get thin vs full ADRs
5. [capability-map.md](capability-map.md) — themes, status, ADR links
6. [glossary.md](glossary.md) — shared vocabulary (optional)
7. [adr/index.md](../adr/index.md) — full decision log

## Discipline

### When an ADR is required

| Change | ADR? |
|--------|------|
| Wire protocol, authority model, persistence schema, or security boundary | Yes — new or superseding ADR |
| New major dependency or process model (sim vs client, new crate boundary) | Yes |
| Behavior already covered by an existing ADR | No — code only; extend ADR if semantics shift |
| Bug fix, perf tweak, refactor inside established ADR scope | No |
| Routine dependency bumps within the same stack | No |

If unsure, prefer a **short ADR** or an explicit addendum to the nearest ADR over silent drift. **New ADRs** follow **[roadmap.md](roadmap.md)** then **[adr-resolution.md](adr-resolution.md)** — tier and resolution decide **if** and **how deep**; see *From roadmap to ADR* there.

### Definition of done (ADR-worthy PRs)

1. **`Aligns with`** in ADR metadata lists relevant `G-*` / `P-*` from [vision.md](vision.md) and [principles.md](principles.md), or states **`N/A`** with one line why (e.g. local refactor under ADR-NNN).
2. **`adr/index.md`** updated in the same change set as a new ADR file.
3. If work **materially advances** a capability row, update [capability-map.md](capability-map.md); if **tier** changes, update [roadmap.md](roadmap.md).

### Conflicts with principles

If a decision **contradicts** a `P-*` principle, either:

- Update **principles.md** with rationale and date, or
- Document a **time-boxed exception** in the ADR (principle ID + why).

## Files

| File | Purpose |
|------|---------|
| [vision.md](vision.md) | Mission, lineage, `G-*` goals, non-goals |
| [principles.md](principles.md) | `P-*` durable rules |
| [roadmap.md](roadmap.md) | Priority tiers, sequencing, ADR focus |
| [adr-resolution.md](adr-resolution.md) | R0–R3 documentation depth |
| [capability-map.md](capability-map.md) | Themes ↔ ADRs ↔ status |
| [glossary.md](glossary.md) | Domain terms |

## See also

- [Meta overview](../README.md)
- [ADR template](../adr/template.md) — includes **Aligns with**
