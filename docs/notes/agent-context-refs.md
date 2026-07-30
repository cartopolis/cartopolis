---
topic: agent-context-refs
triggers: [agent-context, "refs/agent-context", post-receive, prune, gc, "chat history", handoff, Stop hook, forgejo hooks]
updated: 2026-07-30
---

# Carrying agent chat context on a git ref

Work on this repo happens partly through `claude.ai/code`, which — because
`viberfox-agent` on the VPS runs `claude remote-control --spawn worktree` — is a
thin client over a container here, not an Anthropic-hosted sandbox. The
transcripts are therefore ordinary files on our own volume
(`agent_claude` → `/home/agent/.claude/projects/<slug>/*.jsonl`), and every
transcript line carries a `gitBranch` field, so branch → session mapping is
exact rather than guessed.

The reasoning behind an unfinished branch is published as a digest on
**`refs/agent-context/<branch>`**. It is public while the branch is open and
must be *unretrievable* once the branch is merged — that requirement is what
drove every choice below.

## Why a ref outside `refs/heads/*`

Measured 2026-07-30 with scratch bare repos:

| operation | picks up the namespace? |
|---|---|
| `git clone` over `upload-pack` | no |
| plain `git fetch` on an existing clone | no |
| explicit `git fetch origin '+refs/agent-context/*:…'` | yes |

So no clone accumulates chat history by accident; only an agent that asks gets
it. `uploadpack.hideRefs` was tried and **rejected**: it does stop the
advertisement, but it also makes the explicit fetch impossible, which is the one
access path we need.

**Measurement trap:** `git clone /path/to/repo` hardlinks the whole objects
directory and never speaks `upload-pack`, so it reports objects present
regardless of ref advertisement. An early run of this test "proved" the opposite
of the table above. Any advertisement or isolation test must pass `--no-local`.

## Why deletion needs a prune, and what actually prunes

Also measured 2026-07-30, in order of increasing force:

| step | result |
|---|---|
| delete the file in a new commit | content still fully readable |
| delete the ref | object intact, merely unreachable |
| `git gc` | **still present** — `gc.pruneExpire` defaults to two weeks |
| `git gc --prune=now` | gone |

Forgejo sets `core.logAllRefUpdates = true` and `gc.reflogExpire = 90` globally
for its repos (`/var/lib/forgejo/data/home/.gitconfig`), which looks like it
would pin the objects for 30 days via `gc.reflogExpireUnreachable`. It does not:
deleting a ref deletes that ref's reflog with it, so a `--prune=now` still
collects the objects. Verified for a ref under `refs/heads/` too — the namespace
choice above is about fetch isolation, *not* about reflog retention. Don't
re-derive the reflog worry from first principles; it was tested.

Because the digest never touches `main`, removing it is a ref deletion plus a
prune — **not** a history rewrite. No SHA on `main` changes and nobody re-clones.

## Where the prune is triggered from

`hooks/post-receive.d/prune-agent-context` in the *server* repo. Forgejo's
global `core.hooksPath` means a repo's own `hooks/post-receive` is never run by
git, but the generated dispatcher in the shared hooks dir loops over
`./hooks/post-receive.d/*` relative to the repo (skipping the entry named
`gitea`), which is the sanctioned per-repo extension point. The hook fires on an
all-zero new object id for an `refs/agent-context/*` ref and detaches
`git gc --prune=now`, so a push never blocks on gc. Measured end-to-end: object
gone within 2 s of the ref deletion.

`--prune=now` is a flag on that one invocation rather than `gc.pruneExpire` in
the repo config, so ordinary and automatic gc keep git's two-week safety net for
force-push recovery.

**Rejected: an admin API token.** `POST /api/v1/admin/cron/git_gc_repos` exists
on our instance and would trigger the prune over HTTPS with no shell, and
`forgejo admin user generate-access-token` can mint the token without the web
UI. But the endpoint sits under `/api/v1/admin/*`, so the token needs admin
scope — it can create users, hence own the instance. That is a *heavier*
credential than the SSH key the whole design was meant to avoid. The hook needs
no credential at all.

## The digest is mechanical

A jq filter (`agent-context.jq` on the container volume) keeps user prose,
assistant prose, subagent reports and one line per tool call; it drops tool
results, thinking and slash-command plumbing. Measured on four real transcripts:
**2% of the input size** (1169 KB → 33 KB; all four branches together ≈ 55 KB).

That it needs no model is the point — a `Stop` hook can do the whole job, so the
handoff cannot be forgotten, and hooks receive `transcript_path` on stdin so the
script never has to guess which file is its own.

Commit messages on these refs are deliberately dull (`agent context`). Push
webhooks and Forgejo's activity feed retain commit *metadata*, and that is the
one place content could outlive the prune; file contents are not carried there.

## What is still not covered

Backups taken before a deletion still hold the content — true of any store, git
or database. And a clone that explicitly fetched the namespace keeps its objects
until its own prune, which is why `finish-branch` prunes locally as well as
letting the server hook run.
