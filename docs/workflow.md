# The agent workflow

How work gets from an idea to a branch, across a desktop editor and a headless
agent on the VPS, with a Forgejo kanban board as the shared view.

Everything below is driven by **Forgejo labels**. The board column is a
*projection* of them, never the source — see [Why the board cannot be the state
store](#why-the-board-cannot-be-the-state-store).

- [Two lanes, three places](#two-lanes-three-places)
- [Driven lane](#driven-lane-zed-and-claudeaicode)
- [Autonomous lane](#autonomous-lane-the-issue-watcher)
- [Handover](#handover)
- [States](#states)
- [Setup](#setup)
- [When it goes wrong](#when-it-goes-wrong)

## Two lanes, three places

Work is split by **who is driving**, not by where it runs. Those are separate
axes and conflating them gets the GPU rule wrong.

| | Zed | claude.ai/code | issue watcher |
|---|---|---|---|
| Session | `claude-acp` on the workstation | Remote Control into `viberfox-agent` | headless `claude -p` |
| **Lane** | **driven** | **driven** | **autonomous** |
| Driving | you, live | you, live | a new issue or a label |
| Label | `local` | `local` | none, or `agent` |
| Host | workstation | VPS container | VPS container |
| GPU | **yes** | no | no |
| Correcting it | a sentence | a sentence | a comment, next poll |

**The driven lane is the default for real work**, and the reason is the cost of
being wrong. A spec written with you in the room is reviewed as it is written,
and a wrong direction costs one sentence to fix. The same ticket handed to the
watcher raw has to be *inferred* from whatever the issue says, and a confidently
wrong inference costs a 45-minute session and a branch someone has to read to
discover it went sideways.

`claude.ai/code` is the same lane as Zed, not a variant of the watcher: a person
is steering it turn by turn. It is a full design/spec session that happens to be
reachable from a phone. Everything below about the driven lane — `/ticket`, the
`local` label, the `Stop` hook, the handover — applies to it unchanged.

The autonomous lane is for what the driven one cannot absorb: work that should
happen while you are not at a keyboard, tickets filed in passing, and anything
you would rather queue than do.

> **`local` names the lane, not the machine.** It means "a person is driving
> this ticket; the watcher must not touch it" — true of a claude.ai/code session
> on the VPS just as much as a Zed session on the workstation.

### The GPU rule is about the host, not the lane

**Anything touching camera, shaders, egui layout or map draw order must run on
the workstation.** Both VPS contexts are deliberately GPU-less, so `--shot` and
`cargo shots` cannot run there and the change cannot be verified — that applies
to an interactive claude.ai/code session exactly as much as to the watcher. Both
are told to say so rather than fake it, but the cheaper fix is not to send it.

A claude.ai/code session is therefore excellent for the *spec* of a rendering
change and unable to sign it off. Filing the ticket there and doing the work in
Zed is the intended shape.

## Driven lane (Zed, and claude.ai/code)

Identical in both places. The only difference is the GPU.

### File the ticket

Use the [`/ticket`](../.claude/skills/ticket/SKILL.md) skill. It writes a spec —
problem, approach, acceptance criteria, verification, out of scope — shows it to
you before filing, then creates the issue with the `local` label.

```bash
.claude/skills/ticket/file-issue --title "..." < /tmp/spec.md
```

The card appears in **Backlog** within 30 s. Neither driven context places it
itself; the watcher does, so the bot's board password stays in one place instead
of being copied to a laptop and a container.

Credentials are `~/.config/viberfox/forgejo.env` (`0600`, scope `write:issue`) in
whichever context you are in — the workstation and the container each hold their
own token. Both author issues as **you**, not as the bot: a person is driving,
and it keeps the `@agent` comment path working, since the watcher ignores
comments authored by `viberfox-agent` as a loop-breaker.

`--branch` defaults to the branch currently checked out, and **the issue number
is inserted into its name**: `fix/road-joins` → `fix/12-road-joins`, renamed
locally to match. The number only exists once the issue does, so the rename
happens on filing; a branch already numbered, or already pushed to `origin`, is
left alone. The watcher's own fallback is `issue-<n>`, which carries the number
for the same reason.

That name is then written into the issue body as a `` Branch: `` line, which is
not decoration — see [Handover](#handover).

### Work

Ordinary sessions on an ordinary branch. Two things happen automatically when a
session **ends normally**, both from the `Stop` hook in
`~/.claude/hooks/agent-context-push`:

1. a mechanical digest of the session is published to
   `refs/agent-context/<branch>`
2. the branch is pushed, if it has commits

The hook is scoped by **remote URL**, not by where it is installed: it lives in
the global settings and exits silently in any repo whose `origin` is not
viberfox. Without that guard it would publish chat digests into every other
project on the workstation.

**The same script runs in both driven contexts** — `~/.claude/hooks/` on the
workstation and `/home/agent/.claude/hooks/` on the `agent_claude` volume. They
must stay byte-identical; `md5sum` both after touching either. They drifted once
already, and the symptom was silent: claude.ai/code sessions published their
digest but never pushed their branch, so a handover got the reasoning and none of
the code.

Each claude.ai/code session gets its own worktree (`--spawn worktree`) on its own
branch, so the digest is per-branch there exactly as on the workstation. The
worktree *directories* are named `bridge-cse_*`, but the branches are ordinary
`feat/…` / `fix/…` names — which is what the hook keys on.

> **Commit before you stop.** Uncommitted work does not survive a handover. The
> digest header records the file count (`uncommitted at stop: 7 file(s)`) so the
> next agent knows something existed, but it cannot recover it.

## Autonomous lane (the issue watcher)

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

**VPS — `viberfox-agent`** (the claude.ai/code container)

| What | Where |
|---|---|
| Same `Stop` hook + jq filter | `/home/agent/.claude/hooks/` on the `agent_claude` volume |
| Hook registration | `/home/agent/.claude/settings.json` → `hooks.Stop` |
| Forgejo token (`write:issue`) | `/home/agent/.config/viberfox/forgejo.env`, `0600` |

**VPS — `viberfox-issue-watcher`** (`/data/agent/`, docs in `README.md` there)

| What | Where |
|---|---|
| Watcher + board driver | `issue-watcher/watcher.ts`, `issue-watcher/board.ts` |
| Token, bot password | `watcher.env`, `0600` |
| Modes, labels, column ids | `docker-compose.yml` → `environment:` |

The `Stop` hook exists in two places on purpose — the workstation and the
container are separate environments — but it is the same script. `md5sum` both
after editing either; see the warning under [Work](#work) for what drift looks
like.

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
