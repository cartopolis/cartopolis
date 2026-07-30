---
name: ticket
description: File a viberfox issue on the kanban board from a local session, with a spec you and the user just agreed on. Use when work should be tracked as a ticket — whether it is being started now, deferred, or handed to the remote agent. Applies the `local` label so the VPS agent leaves it alone.
---

# ticket

File a Forgejo issue for viberfox from *this* session, with the spec already
written. See [docs/workflow.md](../../../docs/workflow.md) for the two lanes and
why the default is local.

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
`cargo check -p viberfox`, `cargo test -p viberfox --bin viberfox`, and anything
that needs `cargo shots` on a workstation.

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
`--label` defaults to `local`. It prints the issue number; the URL goes to
stderr.

The card appears in **Backlog** within 30 s — the watcher seeds it, because
Forgejo 16 has no kanban API and only the bot's session can move a card.

## Which label

- **`local`** (default) — you or a later local session will do the work. The
  remote agent never touches it.
- **`agent`** — hand it straight to the VPS agent to implement. Use when the
  spec is complete and the work needs no GPU.
- **`agent:skip`** — a record only; nobody should act on it yet.

**Anything touching camera, shaders, egui layout or map draw order must stay
`local`.** The VPS container has no GPU, so `--shot` and `cargo shots` cannot
run there and the change cannot be verified — see the run-modes notes in
CLAUDE.md.

## Handing it over later

If the work stalls, the remote agent can take it over: remove the `local` label
(or comment `@agent`). It will attach to the branch named in the issue body and
read the chat digest this session publishes to `refs/agent-context/<branch>`.

Two things have to be true for that to work, and both are automatic *provided
the session ends normally*:

- the branch is pushed — the `Stop` hook pushes it whenever it has commits
- the digest is published — same hook

So **commit before you stop.** Uncommitted work does not survive the handover;
the digest records the file count so the next agent at least knows it existed.
