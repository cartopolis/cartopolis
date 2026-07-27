# Architecture Decision Records (ADRs)

> **Archived 2026-07-26.** Numbered ADRs were replaced by topic notes in
> [`docs/notes/`](../notes/) plus required commit bodies — see
> [retired/adr-system.md](../notes/retired/adr-system.md) for the reasoning.
> These files are kept for history: read them, don't add to them, and don't
> re-propose the form. Everything below describes the retired process.

This directory contains Architecture Decision Records for Viberfox, documenting architectural decisions and technology choices. ADRs help maintain consistency and provide context for future development.

The **abstract layer** (`docs/abstract/`) defines **goals (`G-*`)**, **principles (`P-*`)**, **[roadmap tiers](../abstract/roadmap.md)**, and **[documentation resolution](../abstract/adr-resolution.md)**. New ADRs should set **Aligns with** in metadata (see `template.md`). Optional: **Roadmap tier** and **Doc resolution** when the ADR is tied to a roadmap row.

## Quick Start for AI Assistants

**Before making architectural decisions:**

1. Read [abstract/vision.md](../abstract/vision.md) and [abstract/principles.md](../abstract/principles.md)
2. Read [abstract/roadmap.md](../abstract/roadmap.md) and [abstract/adr-resolution.md](../abstract/adr-resolution.md)
3. Skim [abstract/capability-map.md](../abstract/capability-map.md) for themes and linked ADRs
4. Search ADRs in `index.md` for relevant decisions
5. Cite ADRs and `G-*` / `P-*` in plans
6. Create new ADRs using `template.md` (**Aligns with** `G-*` / `P-*` or `N/A` with reason)

**After successful implementation:**

- Document significant decisions using `template.md`
- Add new ADRs to `index.md`

## ADR Format

Each ADR contains:

- **Context**: Background and problem statement
- **Decision**: What was decided
- **Rationale**: Why this decision was made
- **Consequences**: Pros, cons, risks, trade-offs
- **Related ADRs**: Links to related decisions

## Location

- **Abstract (vision, roadmap, resolution)**: `docs/abstract/`
- **Index**: `docs/adr/index.md`
- **Template**: `docs/adr/template.md`
- **Individual ADRs**: `docs/adr/XXXX-topic.md`

## Rust ecosystem (implementation hints)

Server and protocol ADRs (**006–016** and later, as added) include **recommended crates** where they help: e.g. **`tokio`** + **`tokio-util`** framing, **`postcard`**/`serde` for payloads, **`refinery`** + **`rusqlite`** for migrations, **`figment`** + **`clap`** for config, **`tracing`** for observability. Prefer **workspace dependency** versions in `[workspace.dependencies]` (ADR-015) instead of duplicating versions per crate.
