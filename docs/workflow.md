# The agent workflow

How work gets from an idea onto `main`: in **sessions**. A session is one agent
thread — in Zed, in claude.ai/code, or Claude Code in a terminal — working in a
worktree of its own on a branch of its own, with the maintainer in the
conversation. There is no ticketing system: no issues, no labels, no board. What
a session was for is in its conversation, which is published as a digest, and in
its commit bodies.

Until 2026-10-17 the unit of work was a Forgejo ticket, with `/ticket`,
`/handover`, a kanban board and an issue watcher on the VPS;
[notes/retired/ticket-workflow.md](notes/retired/ticket-workflow.md) has that
document as it stood, and why it went.

## What this is for

One maintainer, several sessions at once, more in flight than one person can
watch. Four things follow, and every mechanism below is one of them:

1. **Sessions are disposable; what they leave is not.** A session dies, is
   killed, or wanders off. What survives it is git — the branch, its commits and
   their bodies — and the published digest, never only the conversation.
2. **Reasoning is published, not reconstructed.** A diff shows what was done, not
   what was considered and rejected, so the next session redoes the rejected
   thing. Digests go up every turn — [Reasoning](#reasoning-the-digest).
3. **Nothing may gate on what the agent controls.** The landing gate is a
   checked-in script, not a sentence in a prompt —
   [notes/agent-verification.md](notes/agent-verification.md).
4. **Standing facts live outside the agent.** What constrains all future work
   goes in `docs/notes/`, what explains one diff goes in the commit body.

## One session, one worktree, one branch

**Never work in the main checkout and never on `main`.** A checkout is shared
state: on 2026-10-17 five sessions were running in one, and between them a
branch was switched under the other four, a landing found the tree full of
somebody else's half-written edits (and had to be gated in a separate worktree
to test the commit rather than the mess), and every build queued on one
`target/` directory's lock.

How a session gets its own worktree depends on where it runs:

| Where | How |
|---|---|
| Zed | One worktree per thread, created by Zed — **check, see below** |
| Claude Code | Ask it to work in a worktree: `.claude/worktrees/<name>`, on branch `worktree-<name>`, cut from `origin/main` |
| claude.ai/code | `--spawn worktree`, on its own branch |

**Zed on macOS has put every thread in the main checkout.** All fourteen threads
on the Mac between its install (2026-10-15) and 2026-10-17 were recorded with the
folder `/Users/jeroen/code/cartopolis` in Zed's own database
(`sidebar_threads.folder_paths` in `db/0-stable/db.sqlite`), and its table of
thread worktrees was empty — while Zed on Linux gives each thread one. The thread's
folder is fixed when Zed creates it, before any prompt is read, so nothing in
this repository can correct it. Before working, look: `git worktree list`, and
the session's working directory.

Separate worktrees do not mean slow builds. Each builds into its own `target/`
and sccache refills it — measured at 85.6 % hits — provided nobody exports
`CARGO_TARGET_DIR`, which sccache hashes into its key
([notes/sccache-parallel-builds.md](notes/sccache-parallel-builds.md)).

## Working

- **Commits** follow the [`commit`](../.claude/skills/commit/SKILL.md) skill:
  focused, Conventional, and a body for every `feat`, `fix` and `perf` — the body
  is where the reasoning of a session outlives it.
- **Builds, captures and deploys** run on a midas node where the work needs one
  (`CLAUDE.md` has which, and when a GPU matters).
- **Picking up another session's branch:** open a worktree on it, then read its
  digest before its diff ([`branch-context`](../.claude/skills/branch-context/SKILL.md)).
  The earliest session usually holds the design decision; the later ones execute
  it.

## Handing a session over

A session that stops before its work is done — out of context, killed, finished
for the day — hands it to the next one rather than leaving it in a worktree:

```bash
.claude/skills/handover/hand-over --note "what works, what does not, what you suspect"
```

It sweeps uncommitted work into a `wip:` commit whose message carries the note,
pushes the branch, confirms the session's digest is published, and detaches the
worktree so the branch is free for the next session's worktree on the same
machine. The next session switches to the branch in a worktree of its own, reads
the digest and the note, and **folds the `wip:` commit into real commits before
landing** — `merge-to-main` refuses a branch that still carries one, because
with no pull request nothing else would stop a sweep reaching `main`.

The note is the part only you can write: steps one to three move bytes, and the
note is the intent that is not already in the digest.
[`handover`](../.claude/skills/handover/SKILL.md) has the rest.

It refuses to sweep the repository's main checkout. Several sessions may be
running in it, and `git add -A` there would hand over their edits as well.

## Landing it: say "merge to main"

You are in the room, so the review has already happened in the conversation.
Saying **"merge to main"** is the instruction to land, and the session does it
rather than handing back a branch:

```bash
.claude/skills/finish-branch/merge-to-main
```

It rebases onto `origin/main`, runs CI's gate **locally**, pushes the branch and
fast-forwards `main` to it. There is no pull request. `--deploy wasm,apk,…`
then publishes the landed commit with `midas deploy`.

**Why the pull request went (2026-08-30).** `ci.yml` has no push trigger on
branches, so opening a PR was the only way to start a run — two full runs of a
15-25 minute workflow per landing, on a runner with capacity 3, for one tree. And
the first was not read: of the eight merges before the change, five went from
"PR opened" to "PR merged" in under six seconds. So the verdict moved to where the
code is:

```bash
.claude/skills/finish-branch/verify-branch
```

runs the same commands out of the same workflow file — `cargo fmt --check`,
`cargo test --locked --workspace --exclude cartopolis_android`, and, when the
diff can change a frame, the two headless smokes and the wasm and android target
checks. It scopes itself with `ci.yml`'s own deny-list and **names on stdout
everything it skipped**. It runs on the midas node the worktree holds, or queues
for a VPS node; with no midas reachable, `CARTO_GATE_NO_SLOT=1` runs it in place.

Four properties are deliberate:

- **The gate is a script, not a sentence.** A session cannot narrow `--workspace`
  to the package it was working in, because it does not choose the argument.
- **A pass is a receipt, keyed on the head sha.** `merge-to-main` refuses a head
  that has none, and committing or rebasing moves the sha, so a pass earned on
  one tree cannot be spent on another.
- **`main` is a fast-forward, never a force.** A rejection means another session
  landed in between; re-run and let the rebase pick it up. No merge commits.
- **It lands only a clean tree, on a named branch, with no handover sweeps.** A
  dirty worktree is refused, because what is verified has to be what lands —
  which is the other reason a session needs its own worktree. A detached HEAD is
  refused because there is no branch to push it under, and a `wip:` commit
  because it is a [handover](#handing-a-session-over)'s sweep, not a change.

### The verdict is still CI's, and it arrives afterwards

CI runs on `main` nightly, and there it runs **more** than a pull request ever
did: the render smokes and both cross-target lanes. What changed is when the
answer lands. That is a real weakening — the gate runs on a machine the agent
is on, and cannot reproduce a clean checkout or a cold cache — and what is kept
of the rule is that the invocations are fixed in a checked-in script, the pass is
recorded against a sha, and the independent run still happens. A red `main` is
fixed forward. Publishing is the gate that protects strangers:
`tools/deploy-main` refuses a commit whose CI never went green, and
`midas deploy` asks for the landing gate's receipt.

## Reasoning: the digest

A `Stop` hook publishes a mechanical digest of each session — user and
assistant prose verbatim, one line per tool call, credentials redacted — to the
**private** `jeroen/cartopolis-agent-context` repository, every turn, keyed on
the session's branch. Private because cartopolis is public and a transcript is
never reviewed before it goes up
([notes/agent-context-refs.md](notes/agent-context-refs.md)). Digests are kept
after a branch lands; [`branch-context`](../.claude/skills/branch-context/SKILL.md)
reads them.

**The hook has to key on the branch now.** Under tickets it published only for a
session bound to one — the marker `.git/cartopolis-ticket` that `/ticket` wrote —
and treated everything else as exploratory. With no `/ticket` there is no
marker, so a hook still keyed on it publishes nothing, silently. It has to
publish for any branch but `main`. The hook is outside this repository, one copy
per machine ([Setup](#setup)); change it there.

## Setup

Per machine, outside the repository:

| What | Where |
|---|---|
| `Stop` hook + jq filter | `~/.claude/hooks/agent-context-push`, `agent-context.jq` |
| Hook registration | `~/.claude/settings.json` → `hooks.Stop` |
| Digest destination | `jeroen/cartopolis-agent-context` over SSH; override with `CARTO_AGENT_CONTEXT_REMOTE` |
| claude.ai/code container | the same hook under `/home/agent/.claude/hooks/` on the `agent_claude` volume |

The copies must stay byte-identical — `md5sum` all of them after editing one.
They drifted once, and the symptom was silent: sessions published their digest
but never pushed their branch.

**The Mac (2026-10-17) had neither the hook nor its registration**, and two
things specific to macOS:

- **Zed starts agents without `/opt/homebrew/bin` on `PATH`**, so `git-lfs` is
  not found: `git status` in a new worktree fails with `git-lfs filter-process:
  command not found`, the LFS `post-*` hooks exit 2, and a worktree checks out
  pointer files in place of the assets. The workaround in force is the repository's
  local `filter.lfs.{process,clean,smudge}` set to the absolute
  `/opt/homebrew/bin/git-lfs`; the fix is to give Zed's agent environment that
  directory.
- **Zed's threads all opened in the main checkout** — [above](#one-session-one-worktree-one-branch).

## When it goes wrong

**`merge-to-main` refuses: uncommitted changes.** You are probably in a checkout
other sessions share. Land from the session's own worktree; failing that, add a
clean worktree at the branch tip, gate it there, and push from it — what the
2026-10-17 landing did.

**`merge-to-main` refuses: wip commits.** The branch was handed over and the
sweep is still on it. Fold it into real commits — `git reset --soft` to the
commit before it and commit again — then re-run.

**The rebase conflicts with `main`.** Another session changed the same lines. If
`main` already landed the same change — it has happened, two sessions fixing one
bug from two ends on one day — take `main`'s side, keep only what it lacks, and
say so in the commit body.

**The gate cannot reach a node.** The midas hub is down or refusing connections.
`CARTO_GATE_NO_SLOT=1` runs the same steps in place; the hub then has no receipt
for the commit, and `midas deploy` will ask for `--unverified`.

**The digest is missing for a branch.** The session was killed before `Stop`
fired, the hook is not installed on that machine, or it is still keyed on the
ticket marker. Nothing recovers it; work from the commits.
