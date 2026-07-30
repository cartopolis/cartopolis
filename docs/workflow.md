# The agent workflow

How work gets from an idea to a branch, across a desktop editor and a headless
agent on the VPS, with a Forgejo kanban board as the shared view.

Everything below is driven by **Forgejo labels**. The board column is a
*projection* of them, never the source — see [Why the board cannot be the state
store](#why-the-board-cannot-be-the-state-store).

- [The two lanes](#the-two-lanes)
- [Local lane](#local-lane-zed-on-the-workstation)
- [Remote lane](#remote-lane-the-vps-agent)
- [Handover](#handover)
- [States](#states)
- [Setup](#setup)
- [When it goes wrong](#when-it-goes-wrong)

## The two lanes

| | **Local** | **Remote** |
|---|---|---|
| Runs in | Zed on the workstation (`claude-acp`) | `viberfox-issue-watcher` on garage44.eu |
| Started by | you, in a session | a new issue, or a label |
| Label | `local` | none, or `agent` |
| GPU | yes — `cargo shots` works | **no** |
| Correcting it | a sentence, immediately | a comment, next poll |

**Local is the default for real work**, and the reason is the cost of being
wrong. A spec written in Zed is reviewed as it is written; the user is in the
room, the code is in context, and a wrong direction costs one sentence to fix.
The same ticket handed to the remote agent raw has to be *inferred* from
whatever the issue says, and a confidently wrong inference costs a 45-minute
session and a branch someone has to read to discover it went sideways.

The remote lane is for what the local one cannot absorb: work that should happen
while you are not at the machine, tickets filed from a phone, and anything you
would rather queue than do.

**Anything touching camera, shaders, egui layout or map draw order must stay
local.** The container is deliberately GPU-less, so `--shot` and `cargo shots`
cannot run there and the change cannot be verified. The remote agent is told to
say so rather than fake it, but the cheaper fix is not to send it.

## Local lane (Zed on the workstation)

### File the ticket

Use the [`/ticket`](../.claude/skills/ticket/SKILL.md) skill. It writes a spec —
problem, approach, acceptance criteria, verification, out of scope — shows it to
you before filing, then creates the issue with the `local` label.

```bash
.claude/skills/ticket/file-issue --title "..." < /tmp/spec.md
```

The card appears in **Backlog** within 30 s. The workstation does not place it
itself; the watcher does, so the bot's password never has to live on your
laptop.

`--branch` defaults to the branch currently checked out. That line in the issue
body is not decoration — see [Handover](#handover).

### Work

Ordinary sessions on an ordinary branch. Two things happen automatically when a
session **ends normally**, both from the `Stop` hook in
`~/.claude/hooks/agent-context-push`:

1. a mechanical digest of the session is published to
   `refs/agent-context/<branch>`
2. the branch is pushed, if it has commits

The hook is scoped by **remote URL**, not by where it is installed: it lives in
the global `~/.claude/settings.json` and exits silently in any repo whose
`origin` is not viberfox. Without that guard it would publish chat digests into
every other project on the workstation.

> **Commit before you stop.** Uncommitted work does not survive a handover. The
> digest header records the file count (`uncommitted at stop: 7 file(s)`) so the
> next agent knows something existed, but it cannot recover it.

## Remote lane (the VPS agent)

A new issue with no `local` label is **refined** before anything is built.

### Refinement

Read-only: a detached worktree, no branch, no commits. The session reads the
code and rewrites the issue description as a spec. Two rules make it useful
rather than confident:

- **Every claim about current behaviour needs a `file.rs:line`.** A spec that
  cannot cite is a spec that guessed.
- **A criterion it would have to guess at is a question, not a criterion.**

It ends with one of two statuses:

- `agent:refined` → card to **To Do**, and the watcher picks it up for
  implementation on the next poll. The auto-chain.
- `agent:needs-input` → card stays in **Backlog** with the open questions in the
  description. **Nothing is built.**

A refinement that returns no status line at all is treated as `needs-input`. An
unclear signal must not start a build.

### Implementation

A worktree on the issue's branch, cut from `origin/main` unless it is a
[takeover](#handover). Verify with `cargo check -p <crate>` and
`cargo test -p viberfox --bin viberfox`, commit in Conventional Commits style,
push the branch. `main` is protected; the bot cannot push to it.

`agent:done` means **a branch is waiting for review**, not shipped.

### Talking to it

- `@agent <instruction>` — resume that issue's session with a new instruction.
  On a `local` issue this doubles as the handover.
- `@agent refine <what is wrong>` — throw the spec away and write it again.
- `agent:skip` — hands off, permanently.

Only users in `AGENT_ALLOWED_USERS` (currently `jeroen`) can start work by
comment. Labelling is already restricted to users with write access, so the
label triggers need no allowlist.

## Handover

The local lane stalls; the remote agent picks it up. **Remove the `local`
label** (or comment `@agent`).

Three things have to line up, and each one used to fail silently:

**The branch.** The watcher reads `` Branch: `feat/x` `` from the issue body. The
two lanes name branches differently — local uses `feat/…`, the watcher used to
hardcode `issue-<n>` — so without that line a takeover would cut a *fresh*
branch from main and ignore every local commit. `main` in that line is ignored.

**The work.** A branch already on `origin` means a takeover: the worktree is
checked out where the branch is, and `freshenWorktree` is skipped. It resets to
`origin/main` when a tree is clean and has no commits of its own, which is right
for a retry and catastrophic here — a fresh worktree also has no commits of its
own.

**The reasoning.** `refs/agent-context/<branch>` carries a digest of the
session(s) that built the branch: what was decided, what was rejected, what was
left unfinished. The watcher fetches it into the worktree and the takeover
prompt points at it; the [`branch-context`](../.claude/skills/branch-context/SKILL.md)
skill documents what to pull out. **Read every session file, oldest first** — the
earliest usually holds the design decision and the later ones only execute it.

If no digest exists, the agent is told to say so rather than invent intent.

> The digest is readable by anyone who can read the repo, for as long as the
> branch is open. It keeps user and assistant prose and one line per tool call;
> it drops tool *results*, thinking, and slash-command plumbing. Merging the
> branch destroys it — see
> [notes/agent-context-refs.md](notes/agent-context-refs.md) and the
> [`finish-branch`](../.claude/skills/finish-branch/SKILL.md) skill.

## States

The labels are the state machine. The column follows.

| Label | Column | Meaning |
|---|---|---|
| `local` | Backlog¹ | The workstation owns it. The agent never touches it. |
| `agent:skip` | Backlog¹ | Nobody acts on it. |
| *(none)* | Backlog | New. Refinement starts on the next poll. |
| `agent:refining` | Backlog | Reading the code, writing a spec. Read-only. |
| `agent:needs-input` | Backlog | Refined, but blocking questions. **Parked.** |
| `agent:refined` | To Do | Spec agreed. Queued for implementation. |
| `agent` | To Do | Explicit "implement this now" — skips refinement. |
| `agent:running` | In Progress | A session is running **right now**. |
| `agent:done` | Done | Branch pushed, waiting for review. |
| `agent:failed` | To Do | Needs a human. |

¹ Seeded once, then never moved again. A card the agent did not place is one you
dragged, and it stays where you put it.

`In Progress` means a session is live, which is what makes a crashed one
visible: a session killed mid-run leaves the issue stuck on `agent:running` **on
purpose**. Silently retrying an unknown half-finished state is worse than a
visible stall.

### Why the board cannot be the state store

Forgejo 16.0.1 has **no REST API for projects**. Nothing matching `project` or
`column` appears in `swagger.v1.json`, and neither `Issue` nor `EditIssueOption`
carries a project field. The board is reachable only through two *web* routes,
and those use session authentication — an API token is rejected outright.

So `board.ts` logs in with the bot's password and keeps the cookie. That works
because Forgejo 16 removed CSRF tokens entirely in favour of Go's header-based
`CrossOriginProtection`, which passes any request with no `Origin` and no
`Sec-Fetch-Site` — every non-browser client. Three things about it are
load-bearing:

- **Login must go over `https://`.** The session cookie is `Secure`, so a POST
  to the internal `http://172.17.0.1:3000` returns a perfectly good `303` and
  sets a cookie the client must discard. You get a successful login followed by
  404s on every authenticated route.
- **Adding a card is a batch route** — `POST /{owner}/{repo}/issues/projects`
  with `issue_ids`, not a per-issue `/issues/{index}/projects`. The latter 404s
  in a way that reads exactly like a permissions problem.
- **Both routes take the issue's internal `id`, never its `#index`.** They agree
  on a young repo and diverge silently later.

Because that path is a password session against un-versioned web routes, a
Forgejo upgrade can break it. Every board call is therefore failure-tolerant:
it logs a warning and the work proceeds. Labels remain correct; only the card
position goes stale.

## Setup

**Workstation**

| What | Where |
|---|---|
| `Stop` hook + jq filter | `~/.claude/hooks/agent-context-push`, `agent-context.jq` |
| Hook registration | `~/.claude/settings.json` → `hooks.Stop` |
| Forgejo token (`write:issue`) | `~/.config/viberfox/forgejo.env`, `0600` |

**VPS** (`/data/agent/`, docs in `README.md` there)

| What | Where |
|---|---|
| Watcher + board driver | `issue-watcher/watcher.ts`, `issue-watcher/board.ts` |
| Token, bot password | `watcher.env`, `0600` |
| Modes, labels, column ids | `docker-compose.yml` → `environment:` |
| Same `Stop` hook | `/home/agent/.claude/hooks/` on the `agent_claude` volume |

The hook exists in two places on purpose — the workstation and the container are
separate environments — but they are the same script. Keep them in step.

`AGENT_NEW_ISSUE_MODE` chooses what a new issue gets: `refine` (default),
`implement`, or `off`.

> `env_file` is read when a container is **created**, not started. Adding a
> variable needs `docker compose up -d --force-recreate`; a `restart` keeps the
> old environment, which reads as "the new code does nothing".

## When it goes wrong

**A card is in the wrong column.** Cosmetic. Check the labels — they are the
truth. `docker logs viberfox-issue-watcher` will have a `[board]` line saying
what failed.

**Cards stopped moving entirely.** The bot's session or password. The watcher
logs `board: DISABLED` at startup if `FORGEJO_BOT_PASSWORD` is unset — which is
what a `restart` instead of a `--force-recreate` looks like.

**An issue is stuck on `agent:running`.** A session died. This is deliberate,
not a bug. Read `/data/agent/state/logs/issue-<n>.log`, then remove the label and
apply `agent` to requeue.

**A takeover ignored the local work.** The issue body had no `` Branch: `` line,
or the branch was never pushed. Check `git ls-remote origin <branch>`.

**A refinement built the wrong thing anyway.** `@agent refine` with the
correction, or take it local. If this happens more than occasionally, the issue
arrived too raw — file it from `/ticket` instead.

**The digest is missing for a branch.** The session was killed before `Stop`
fired, or it predates the hook. Nothing recovers it; work from the commits.
