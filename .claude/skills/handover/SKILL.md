---
name: handover
description: Hand this worktree's ticket to the autonomous agent on the VPS — sweep leftovers into a commit, push the branch, confirm the chat digest is published, and flip the label. Use when local work stops and the remote agent should continue it.
---

# handover

Pass a ticket you have been working on locally to the autonomous agent, with
everything it needs to continue rather than restart.

```bash
.claude/skills/handover/hand-over --note "buildings collide, water does not yet"
```

Requires a bound worktree — run [`/ticket`](../ticket/SKILL.md) first. It
refuses otherwise, because there is nothing to hand over and nowhere to put it.

## What it does, in order

1. **Sweeps leftovers into a `wip:` commit.** Uncommitted work does not survive
   a handover; the digest reports the loss but cannot recover it.
2. **Pushes `HEAD` to the ticket's branch.** Never forced — a rejection means
   the ticket moved under you and needs a person.
3. **Confirms the chat digest is in the context repo.** Warns loudly if not:
   handing over code with no reasoning is how the next agent redoes work you
   already rejected.
4. **Comments, then flips `driven` → `agent`**, and deletes the marker. The
   watcher picks it up within 30 s.

Order matters. The label is the lock, so nothing may flip it until the code and
the reasoning are both actually on the server.

## The note is the useful part

`--note` is what you would tell a colleague taking over. Steps 1–3 move bytes;
this is the only part carrying intent that is not already in the digest.

Good: *"routing works, the ribbon renders in the wrong place above 2 km — I
suspect `ROUTE_Y`, untested"*. Useless: *"continuing work"*.

## Getting it back

Not a command. Wait until `agent:running` is gone, then in a clean worktree:

```bash
.claude/skills/ticket/file-issue --bind 23
```

That claims it back, detaches onto the branch's current tip, and rewrites the
marker. It refuses while the agent is mid-run, if another worktree holds the
ticket, or if this one has commits that are not on origin.
