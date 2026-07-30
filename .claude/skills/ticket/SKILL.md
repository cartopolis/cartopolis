---
name: ticket
description: File a viberfox issue on the kanban board from a session you are steering (Zed or claude.ai/code), with a spec you and the user just agreed on. Use when work should be tracked as a ticket — whether it is being started now, deferred, or handed to the autonomous agent. Applies the `driven` label so the watcher leaves it alone.
---

# ticket

File a Forgejo issue for viberfox from *this* session, with the spec already
written. See [docs/workflow.md](../../../docs/workflow.md) for the two lanes and
why the default is `driven`.

The point of filing from here is that you have the codebase in context and the
user is in the room. A ticket written now can cite real files and settle the
open questions in a sentence; the same ticket refined later by the remote agent
has to infer them, and a confidently wrong spec costs a 45-minute session.

## Write the spec first

Do not file a one-liner. Draft the body and **show it to the user before
filing** — this is the review step the whole workflow is built around.

```markdown
## Problem
What is wrong, citing `crates/viberfox/src/systems/foo.rs:120`.

## Approach
The change, in terms of crates and files.

## Acceptance criteria
- [ ] A checklist someone else can verify.

## Verification
`cargo check -p viberfox`, `cargo test -p viberfox --bin viberfox`, and any
visual check via `cargo shots`.

## Out of scope
What a reader would reasonably assume is included but is not.
```

Every claim about current behaviour needs a `file.rs:line`. If you would have to
guess at an acceptance criterion, ask the user instead of writing it.

## File it

```bash
.claude/skills/ticket/file-issue --title "..." < /tmp/spec.md
```

`--branch` defaults to the branch currently checked out (`main` counts as none).
`--label` defaults to `driven`. It prints the issue number; the URL goes to
stderr.

**The issue number is inserted into the branch name**, keeping the
Conventional-Commits prefix: `fix/road-joins` becomes `fix/12-road-joins`, and
your local branch is renamed to match. Cut your branch with a type prefix and a
short slug and this does the rest; the watcher produces the same shape
(`fix/23-routing-speeding-pill`) for tickets it names itself. The number cannot be known before the
issue exists, which is why the rename happens on filing rather than when you cut
the branch.

Two cases where it leaves the name alone: a branch already carrying that number
(so re-filing is idempotent), and a branch already pushed to `origin` — renaming
that one would orphan the remote branch and any `refs/agent-context` digest
published under the old name. It says which it did on stderr.

The card appears in **Backlog** within 30 s — the watcher seeds it, because
Forgejo 16 has no kanban API and only the bot's session can move a card.

## Which label

- **`driven`** (default) — you, or a later session you are steering (Zed or
  claude.ai/code), will do the work. The watcher never touches it.
- **`autonomous`** — hand it to the watcher for the full pipeline: refine, then
  implement, unattended.
- **`agent`** — hand it straight to the watcher to implement. Use when the spec
  is complete and you want it built now, without a refinement pass.
- **`agent:skip`** — a record only; nobody should act on it yet.

If part of the verification can only run on certain machines, say so in the
**Verification** section. That is a note for whoever picks the ticket up, not a
reason to choose a label.

## Handing it over later

If the work stalls, the watcher can take it over: remove the `driven` label (or
comment `@agent`). It will attach to the branch named in the issue body and
read the chat digest this session publishes to `refs/agent-context/<branch>`.

Two things have to be true for that to work, and both are automatic *provided
the session ends normally*:

- the branch is pushed — the `Stop` hook pushes it whenever it has commits
- the digest is published — same hook

So **commit before you stop.** Uncommitted work does not survive the handover;
the digest records the file count so the next agent at least knows it existed.
