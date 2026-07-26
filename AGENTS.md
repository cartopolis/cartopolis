# Instructions for AI assistants

**Primary guide: [`CLAUDE.md`](CLAUDE.md)** — build/test commands, architecture, the render + `--shot` screenshot harness, and known-issue workarounds. Read it first, whichever assistant you are.

## Architecture docs

Intent and decisions live under `docs/`:

- `docs/abstract/` — **why**: vision (`G-*`), principles (`P-*`), roadmap (P0–P2), ADR resolution (R0–R3), capability map, glossary
- `docs/adr/` — **what we chose**: one file per decision; [`docs/adr/index.md`](docs/adr/index.md) lists them all

Before large or cross-cutting work, read in order: [`vision.md`](docs/abstract/vision.md) → [`principles.md`](docs/abstract/principles.md) → [`roadmap.md`](docs/abstract/roadmap.md) → [`adr-resolution.md`](docs/abstract/adr-resolution.md) → [`docs/adr/index.md`](docs/adr/index.md). Full ADR discipline (when one is required, definition of done): [`docs/abstract/README.md`](docs/abstract/README.md).

**Create or supersede an ADR** for wire-protocol, authority, persistence-schema, security-boundary, major-dependency, or sim/client process-boundary changes; skip it for bug fixes and refactors inside an existing ADR's scope. Copy [`docs/adr/template.md`](docs/adr/template.md) and add a row to [`docs/adr/index.md`](docs/adr/index.md) in the same change set.

## Build scope

Prefer scoped `cargo check -p <crate>` over full-workspace builds. `CLAUDE.md` has the complete command list.
