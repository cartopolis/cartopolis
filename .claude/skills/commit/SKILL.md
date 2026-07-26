---
name: commit
description: Stage and create git commits using Conventional Commits / commitizen style (type(scope): subject, imperative mood, focused per-change, path-scoped, no co-author trailer). Use whenever the user wants to commit the current changes.
---

# commit

Create one or more git commits for the current changes, following the
**Conventional Commits / commitizen** standard.

## Format

`type(scope): subject`

- **type** — one of: `feat`, `fix`, `chore`, `refactor`, `docs`, `test`, `ci`,
  `perf`, `style`, `build`
- **scope** — the affected area (e.g. `map`, `chat`, `sim`, `ui`, `routing`);
  omit only when the change is genuinely cross-cutting
- **subject** — imperative mood, lower-case, no trailing period
  ("add", not "added" / "adds")
- **body** — wrap at ~72 cols and explain the *why*, not the *what*.
  **Required** for `feat`, `fix` and `perf`, and for any commit touching **5+
  files**. Optional only for mechanical `chore` / `docs` / `style` / `build`.

## The body is where the reasoning is kept

An empty body on a large landing is a **bug**, not a style choice. Measured on
this repo: 71 of 113 commits have no body, and 32 of those touch 10+ files —
the big feature landings are exactly the ones that explain nothing. The routing
work landed as `c21d9ee feat: routing, screenshots, vector tiles` with an empty
body, and the finding that made it work (the road graph must be noded, or
ordinary routes silently return `NoPath`) survives nowhere in git.

Before writing the body, ask four questions. Any **yes** must reach the body:

1. **Did I measure something?** Numbers, and how they were obtained.
2. **Did I reject something?** The alternative, and the reason it lost.
3. **Did I hit a constraint from outside the codebase?** Upstream policy,
   licence, a browser/platform limit, a format quirk.
4. **Did something bite me silently?** A failure that produced a plausible
   wrong answer instead of an error.

Four noes → a one-line subject is the honest message. Don't pad.

### Commit or note?

Ask: *will this still be true after the next ten commits?*

- **No — it explains this diff** → commit body. Frozen with the code it describes.
- **Yes — it constrains all future work** → also add it to `docs/notes/`, which
  is edited in place and retrieved by topic. A commit message cannot be updated
  when it stops being true, and nobody greps `git log` before editing a file.

Standing constraints, external-world facts, and directions abandoned *without*
a diff are the note's job — the last of these has no commit to attach to at all.

## Rules

- Prefer **focused commits per logical change** — if the working tree mixes
  unrelated changes, split them into several commits rather than one.
- A subject that **lists several things** ("routing, screenshots, vector tiles",
  anything joined by a comma or "and") is the signal to split, not to write a
  longer subject. Each part then gets its own body.
- Commit **path-scoped** (`git commit -- <path> ...`) so a parallel agent's
  staged files aren't swept into this commit.
- **Never** add a `Co-Authored-By:` or any attribution / co-author trailer.
- Don't commit on the default branch if the change warrants a branch — ask first.
- Don't push unless explicitly asked.

## Steps

1. Run `git status` and `git diff` (and `git diff --staged`) to see all changes.
2. Group them into logical commits.
3. For each group: stage its paths, then `git commit -- <paths>` with a
   compliant message. If the group is `feat`/`fix`/`perf` or spans 5+ files,
   answer the four questions above and write the body before committing.
4. Show the resulting `git log --oneline` for the new commits so the user can
   confirm, and flag any commit that ended up bodiless despite qualifying.

If the user passed extra guidance as arguments, honor it (e.g. how to split the
commits, a specific scope, or an explicit message).
