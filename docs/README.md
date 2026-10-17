# Documentation

| Path | Contents |
|------|----------|
| **[guides/development.md](guides/development.md)** | **Contributor guide** — dev loop, tests, code map, conventions, config, CI |
| **[workflow.md](workflow.md)** | **Agent workflow** — sessions: one worktree and one branch each, landed on `main` through the local gate, with their reasoning in the commit bodies |
| **[manual.md](manual.md)** | User manual — controls, getting around, authoring objects, developer panel |
| **[notes/](notes/)** | Standing facts about the codebase: measurements, rejected options, external constraints |
| **[guides/](guides/)** | Operational how-tos (self-hosting services, deployment) |
| **[img/](img/)** | Screenshots for the docs and the top-level README (`shots.toml` drives `cargo shots`) |
| [adr/](adr/), [abstract/](abstract/) | **Frozen archive** — the numbered decision log and its abstract layer, retired 2026-07-26 ([why](notes/retired/adr-system.md)) |

## Guides

- [Development](guides/development.md) — start here as a new contributor
- [Self-hosted geo services (Overpass + Nominatim + 3DBAG)](guides/server.md) — run local Overpass/Nominatim instances seeded with the Netherlands extract, serve the 3DBAG building tiers as static tiles, and point the client at them via `CARTO_OVERPASS_URL` / `CARTO_NOMINATIM_URL`

## Where a fact belongs

- Explains **one diff** → the commit body (required for `feat`/`fix`/`perf` and any 5+-file commit).
- Constrains **all future work** → a topic note in [notes/](notes/), edited in place.
- Falsified by a code change → a comment next to the thing.

Nothing new goes in `adr/` or `abstract/`.

**AI assistants:** [AGENTS.md](../AGENTS.md) in the repo root.
