---
name: handover
description: Hand this session's branch to the next session — sweep leftovers into a wip commit carrying a note on where things stand, push the branch, and release it for another worktree. Use when a session stops before its work is done.
---

# handover

A session stops before its work is done — out of context, killed, or finished
for the day — and another session continues it: later, on another machine, in
Zed or in claude.ai/code. This leaves the branch so the next one can *continue*
rather than restart.

```bash
.claude/skills/handover/hand-over --note "routing works; the ribbon sits wrong above 2 km — suspect ROUTE_Y, untested"
```

Run it from the session's own worktree, on its branch. It refuses on `main`, on
a detached HEAD, and to sweep uncommitted changes in the repository's main
checkout — other sessions may be working there, and `git add -A` would hand
their edits over too (see [docs/workflow.md](../../../docs/workflow.md)).

## What it does, in order

1. **Commits the work and the note.** Uncommitted changes are swept into a
   `wip: handover of <branch>` commit whose message carries the note; a clean
   tree gets an empty one, because the note has to live somewhere the next
   session will find it. Uncommitted work does not survive a session.
2. **Pushes the branch.** Never forced — a rejection means another session moved
   it, and that needs a person.
3. **Releases the branch** by detaching this worktree at the same commit, so a
   new worktree on this machine can check the branch out (git lets one worktree
   hold a branch at a time). Not done in the main checkout.

## The note is the useful part

Steps 1–3 move bytes; the note is the only part carrying intent that is not
already in the commits. Write what you would tell a colleague taking over: what
works, what does not, what you suspect and have not tested, what you decided
against. Good: *"routing works, the ribbon renders in the wrong place above
2 km — I suspect `ROUTE_Y`, untested"*. Useless: *"continuing work"*.

## Picking it up

In a new session, in a worktree of its own:

```bash
git fetch origin <branch> && git switch <branch>
```

Then read before changing anything: the handover note and the commit bodies,
with `git log origin/main..<branch>`.

**Fold the `wip:` commit before landing.** It is a sweep, not a change anyone
would review, and `merge-to-main` refuses a branch that still carries one. Turn
it into real commits — for instance `git reset --soft` to the commit before it
and commit again with the [`commit`](../commit/SKILL.md) skill's conventions.
