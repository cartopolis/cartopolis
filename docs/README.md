# Documentation

User docs, product intent, planning, and architecture decisions for Viberfox live
under **`docs/`**:

| Path | Contents |
|------|----------|
| **[manual.md](manual.md)** | User manual — controls, getting around, authoring objects, developer panel |
| **[abstract/](abstract/)** | Vision (`G-*`), principles (`P-*`), roadmap, **ADR resolution** guide, capability map, glossary, discipline |
| **[adr/](adr/)** | Architecture Decision Records (numbered files + `index.md`, `template.md`) |
| **[guides/](guides/)** | Operational how-tos (self-hosting services, deployment) |
| **[img/](img/)** | Screenshots used by the docs and the top-level README (`shots.toml` drives `cargo shots`) |

## Guides

- [Self-hosted geo services (Overpass + Nominatim)](guides/server.md) — run local Overpass/Nominatim instances seeded with the Netherlands extract and point the client at them via `VIBE_OVERPASS_URL` / `VIBE_NOMINATIM_URL`

## Read order

1. [abstract/vision.md](abstract/vision.md)
2. [abstract/principles.md](abstract/principles.md)
3. [abstract/roadmap.md](abstract/roadmap.md) — **priorities and sequencing**
4. [abstract/adr-resolution.md](abstract/adr-resolution.md) — **how deep to document** a concept
5. [abstract/capability-map.md](abstract/capability-map.md)
6. [adr/index.md](adr/index.md)

Discipline (when an ADR is required, DoD): [abstract/README.md](abstract/README.md).

**AI assistants:** [AGENTS.md](../AGENTS.md) in the repo root.
