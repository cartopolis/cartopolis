# Instructions for AI assistants

**Primary guide: [`CLAUDE.md`](CLAUDE.md)** — build/test commands, architecture, the render + `--shot` screenshot harness, and known-issue workarounds. Read it first, whichever assistant you are.

## Architecture docs

Intent and standing constraints live under `docs/`:

- [`docs/guides/development.md`](docs/guides/development.md) — contributor guide: dev loop, tests, code map, conventions
- [`docs/notes/`](docs/notes/) — **standing facts**: measurements (with method), rejected options (with the reason they lost), external-world constraints, directions abandoned without a diff. One file per topic, retrieved by `triggers:`, edited in place
- `docs/adr/`, `docs/abstract/` — **frozen archive** of the numbered decision log, retired 2026-07-26 ([why](docs/notes/retired/adr-system.md)). Read for history; **never add to them, and don't re-propose numbered ADRs**

**Where a fact goes:** explains one diff → the commit body (required for `feat`/`fix`/`perf` and any 5+-file commit — see [`.claude/skills/commit`](.claude/skills/commit/SKILL.md)); constrains all future work → a note; falsified by a code change → a comment next to the code.

## Build scope

Prefer scoped `cargo check -p <crate>` over full-workspace builds. `CLAUDE.md` has the complete command list.
