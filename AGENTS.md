# Instructions for AI assistants

**Primary guide: [`CLAUDE.md`](CLAUDE.md)** — build/test commands, architecture, the render + `--shot` screenshot harness, and known-issue workarounds. Read it first, whichever assistant you are.

## Architecture docs

Intent and decisions live under `meta/` (not `docs/`):

- `meta/abstract/` — **why**: vision (`G-*`), principles (`P-*`), roadmap (P0–P2), ADR resolution (R0–R3), capability map, glossary
- `meta/adr/` — **what we chose**: one file per decision; [`meta/adr/index.md`](meta/adr/index.md) lists them all

Before large or cross-cutting work, read in order: [`vision.md`](meta/abstract/vision.md) → [`principles.md`](meta/abstract/principles.md) → [`roadmap.md`](meta/abstract/roadmap.md) → [`adr-resolution.md`](meta/abstract/adr-resolution.md) → [`meta/adr/index.md`](meta/adr/index.md). Full ADR discipline (when one is required, definition of done): [`meta/abstract/README.md`](meta/abstract/README.md).

**Create or supersede an ADR** for wire-protocol, authority, persistence-schema, security-boundary, major-dependency, or sim/client process-boundary changes; skip it for bug fixes and refactors inside an existing ADR's scope. Copy [`meta/adr/template.md`](meta/adr/template.md) and add a row to [`meta/adr/index.md`](meta/adr/index.md) in the same change set.

## Build scope

Prefer scoped `cargo check -p <crate>` over full-workspace builds. `CLAUDE.md` has the complete command list.
