# The agent workflow

How work gets from an idea to a branch, across a desktop editor and a headless
agent on the VPS, with a Forgejo kanban board as the shared view.

Everything below is driven by **Forgejo labels**. The board column is a
*projection* of them, never the source — see [Why the board cannot be the state
store](#why-the-board-cannot-be-the-state-store).

- [The three workflows](#the-three-workflows) — ① Zed, ② claude.ai/code, ③ issue watcher
- [The driven lane in practice](#the-driven-lane-in-practice) — ① and ②
- [The autonomous lane in practice](#the-autonomous-lane-in-practice) — ③
- [Handover](#handover)
- [States](#states)
- [Setup](#setup)
- [When it goes wrong](#when-it-goes-wrong)

## The three workflows

Two independent questions, not one. Keeping them apart is the whole point of
this section, because every mistake in this document so far came from collapsing
them into a single "local vs remote".

**Who is driving?** — is a person reading each answer and correcting course, or
is it running unattended? This decides how much it may assume, and it is what
the `driven` / `autonomous` labels mean.

**Where does it run?** — the workstation or the VPS. This decides whether you
have to be at your desk, and which machine's checkout, build cache and login the
session uses. ② and ③ share all three: same image, same volumes, same clone.

|  | **local** (workstation) | **remote** (VPS) |
|---|---|---|
| **driven** (person steering) | ① **Zed** | ② **claude.ai/code** |
| **autonomous** (unattended) | *(nothing — see below)* | ③ **issue watcher** |

So `driven`/`autonomous` and `local`/`remote` are **not two names for the same
split**. ① and ② share a lane and differ in host; ② and ③ share a host and
differ in lane. Everything that follows hangs off which of those two things you
are asking about.

### ① Zed — driven, local

`claude-acp` in Zed on the workstation. You are in the room, so a wrong
direction costs one sentence.

*Use it for:* real work, at the desk.

### ② claude.ai/code — driven, remote

Remote Control into the `viberfox-agent` container, reachable from a browser or
the phone app. **Same lane as Zed, not a variant of the watcher** — a person is
steering it turn by turn, so it is a full design/spec session that happens not
to be at your desk. `/ticket`, the `driven` label, the `Stop` hook and the
handover all apply to it unchanged.

*Use it for:* the same work as ①, from a browser or the phone — thinking,
specs, review, changes you can state and verify in text.

### ③ Issue watcher — autonomous, remote

Headless `claude -p` in the `viberfox-issue-watcher` container, triggered by an
issue or a label rather than by you. Nobody sees the intermediate turns, which
is why it [refines before it builds](#refinement) and why refinement is required
to cite the code rather than assume.

*Use it for:* work that should happen while you are not at a keyboard.
*Not for:* anything visual, or anything you cannot state precisely.

### The lane axis — how much it may assume

A spec written with you in the room is reviewed as it is written. The same
ticket handed to the watcher raw has to be *inferred* from whatever the issue
says, and a confidently wrong inference costs a 45-minute session plus a branch
someone has to read to discover it went sideways. That is why the driven lane is
the default for real work, and why the autonomous one refines before it builds.

Each lane has a label, and which one is load-bearing depends on how much you
want running unattended:

| Label | Says | Load-bearing when |
|---|---|---|
| `driven` | hands off, a person owns this | `AGENT_NEW_ISSUE_MODE=refine` — the watcher acts by default, so tickets opt **out** |
| `autonomous` | take this: refine it, then build it | `AGENT_NEW_ISSUE_MODE=off` — nothing runs unless a ticket opts **in** |

Neither is redundant in the other's mode. With the watcher acting by default,
`autonomous` is still the only way to hand it a ticket it has already passed —
its cursor only ever moves forward, so an issue that sat in Backlog for a week
will never be picked up on its own.

`autonomous` differs from the bare `agent` label in how much it asks for:
`autonomous` runs the **full pipeline** (refine, then implement), `agent` means
"I have decided this is actionable, skip the spec". `autonomous` on a ticket
whose branch already exists skips refinement too — rewriting the spec of
half-built work is not an improvement.

> **`driven` names the lane, not the machine.** It means "a person is driving
> this ticket; the watcher must not touch it" — true of a claude.ai/code session
> on the VPS just as much as a Zed session on the workstation. It replaced
> `local`, which was a misnomer for exactly that reason; `local` is still
> accepted as an alias so nothing in flight breaks.

### The host axis — where the work happens

`local` and `remote` say nothing about how a session behaves; ① and ② follow the
same rules throughout this document. What the host decides is practical:

- **① requires your desk.** ② and ③ do not — ② is reachable from a browser or
  the phone app, and ③ needs nobody at all.
- **② and ③ share a machine**, and deliberately: same image, same volumes, so
  the same clone, crate cache and login serve both. A branch a claude.ai/code
  session pushes is one the watcher can pick up without fetching anything new.
- **Capabilities differ per host** — a container may not be able to run
  everything the workstation can. That is an operational fact about a given
  machine, not part of this workflow; the run-modes and tooling notes in
  CLAUDE.md are where it belongs, and a session that cannot run something is
  expected to say so rather than fake the result.

The fourth cell of the matrix is empty simply because no autonomous runner is
installed on the workstation. Nothing prevents one.

## The driven lane in practice

Identical in ① and ②.

### File the ticket

Use the [`/ticket`](../.claude/skills/ticket/SKILL.md) skill. It writes a spec —
problem, approach, acceptance criteria, verification, out of scope — shows it to
you before filing, then creates the issue with the `driven` label.

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

## The autonomous lane in practice

A new issue with no `driven` label is **refined** before anything is built.

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
  On a `driven` issue this doubles as the handover.
- `@agent refine <what is wrong>` — throw the spec away and write it again.
- `agent:skip` — hands off, permanently.

Only users in `AGENT_ALLOWED_USERS` (currently `jeroen`) can start work by
comment. Labelling is already restricted to users with write access, so the
label triggers need no allowlist.

## Handover

The driven lane stalls; the watcher picks it up. **Remove the `driven` label**
(or comment `@agent`).

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
| `driven` (or `local`) | Backlog¹ | A person owns it. The watcher never touches it. |
| `agent:skip` | Backlog¹ | Nobody acts on it. |
| *(none)* | Backlog | New. Refinement starts on the next poll, unless `AGENT_NEW_ISSUE_MODE=off`. |
| `autonomous` | Backlog | Opt in: refine, then implement. Works on any issue, however old. |
| `agent:refining` | Backlog | Reading the code, writing a spec. Read-only. |
| `agent:needs-input` | Backlog | Refined, but blocking questions. **Parked.** |
| `agent:refined` | To Do | Spec agreed. Queued for implementation. |
| `agent` | To Do | Explicit "implement this now" — skips refinement. Also the takeover gesture. |
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

**① Zed — the workstation**

| What | Where |
|---|---|
| `Stop` hook + jq filter | `~/.claude/hooks/agent-context-push`, `agent-context.jq` |
| Hook registration | `~/.claude/settings.json` → `hooks.Stop` |
| Forgejo token (`write:issue`) | `~/.config/viberfox/forgejo.env`, `0600` |

**② claude.ai/code — the `viberfox-agent` container**

| What | Where |
|---|---|
| Same `Stop` hook + jq filter | `/home/agent/.claude/hooks/` on the `agent_claude` volume |
| Hook registration | `/home/agent/.claude/settings.json` → `hooks.Stop` |
| Forgejo token (`write:issue`) | `/home/agent/.config/viberfox/forgejo.env`, `0600` |

**③ Issue watcher — the `viberfox-issue-watcher` container** (`/data/agent/`, docs in `README.md` there)

| What | Where |
|---|---|
| Watcher + board driver | `issue-watcher/watcher.ts`, `issue-watcher/board.ts` |
| Token, bot password | `watcher.env`, `0600` |
| Modes, labels, column ids | `docker-compose.yml` → `environment:` |

The `Stop` hook exists in two places on purpose — ① and ② are separate
environments — but it is the same script. `md5sum` both after editing either;
see the warning under [Work](#work) for what drift looks like. ③ needs no hook:
it publishes nothing, because the watcher already reports on the issue itself.

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
