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
- **body** (optional) — wrap at ~72 cols and explain the *why*, not the *what*

## Rules

- Prefer **focused commits per logical change** — if the working tree mixes
  unrelated changes, split them into several commits rather than one.
- Commit **path-scoped** (`git commit -- <path> ...`) so a parallel agent's
  staged files aren't swept into this commit.
- **Never** add a `Co-Authored-By:` or any attribution / co-author trailer.
- Don't commit on the default branch if the change warrants a branch — ask first.
- Don't push unless explicitly asked.

## Steps

1. Run `git status` and `git diff` (and `git diff --staged`) to see all changes.
2. Group them into logical commits.
3. For each group: stage its paths, then `git commit -- <paths>` with a
   compliant message.
4. Show the resulting `git log --oneline` for the new commits so the user can
   confirm.

If the user passed extra guidance as arguments, honor it (e.g. how to split the
commits, a specific scope, or an explicit message).
