# Instructions for AI assistants

**Primary guide: [`CLAUDE.md`](CLAUDE.md)** — build/test commands, architecture, and the rules that hold everywhere. Read it first, whichever assistant you are.

## Where the knowledge is

Most of what this codebase knows is in [`docs/notes/`](docs/notes/), one topic per file: measurements (with method), rejected options (with the reason they lost), external-world constraints, traps that fail silently. Each note's `triggers:` front-matter names the files it is about. Claude Code is shown the matching notes automatically by a hook (`.claude/hooks/note-triggers`); **any other assistant must look them up itself before changing a file**:

```bash
grep -l 'routing.rs' docs/notes/*.md        # notes about a file
grep -l 'triggers:.*routing' docs/notes/*.md # notes about a topic
```

- [`docs/guides/development.md`](docs/guides/development.md) — contributor guide: dev loop, tests, code map, conventions
- [`.claude/skills/`](.claude/skills/) — the procedures: `commit`, `shots` (headless captures and driving a running client), `finish-branch` (landing on `main`), `handover` (passing a session's branch to the next one); the hook is `.claude/hooks/note-triggers`, registered in `.claude/settings.json`
- `docs/adr/`, `docs/abstract/` — **frozen archive** of the numbered decision log, retired 2026-07-26 ([why](docs/notes/retired/adr-system.md)). Read for history; **never add to them, and don't re-propose numbered ADRs**

**Where a fact goes:** explains one diff → the commit body (required for `feat`/`fix`/`perf` and any 5+-file commit — see [`.claude/skills/commit`](.claude/skills/commit/SKILL.md)); constrains all future work → a note; falsified by a code change → a comment next to the code.

## Build scope

Scope builds to the crate you changed — but on a midas build node prefer the workspace-scoped commands it is already warm for. `CLAUDE.md` has the complete command list.
