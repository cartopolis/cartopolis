# Meta (architecture documentation)

Product intent, planning, and architecture decisions for vibe-rs live under **`docs/meta/`**:

| Path | Contents |
|------|----------|
| **[abstract/](abstract/)** | Vision (`G-*`), principles (`P-*`), roadmap, **ADR resolution** guide, capability map, glossary, discipline |
| **[adr/](adr/)** | Architecture Decision Records (numbered files + `index.md`, `template.md`) |
| **[guides/](guides/)** | Operational how-tos (self-hosting services, deployment) |

## Guides

- [Self-hosted Overpass API (Netherlands)](guides/self-hosted-overpass.md) — run a local Overpass instance on Arch and point the client at it via `VIBE_OVERPASS_URL`

## Read order

1. [abstract/vision.md](abstract/vision.md)
2. [abstract/principles.md](abstract/principles.md)
3. [abstract/roadmap.md](abstract/roadmap.md) — **priorities and sequencing**
4. [abstract/adr-resolution.md](abstract/adr-resolution.md) — **how deep to document** a concept
5. [abstract/capability-map.md](abstract/capability-map.md)
6. [adr/index.md](adr/index.md)

Discipline (when an ADR is required, DoD): [abstract/README.md](abstract/README.md).

**AI assistants:** [AGENTS.md](../../AGENTS.md) in the repo root.
