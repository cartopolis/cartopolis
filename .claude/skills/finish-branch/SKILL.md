---
name: finish-branch
description: Land a finished branch into main after reading the agent chat context that explains it. Use when a branch is done and being merged. The chat digest is kept, not destroyed.
---

# finish-branch

Land a branch, having first read the reasoning behind it.

The digest on `refs/agent-context/<branch>` **stays** after the merge. Keeping it
is the point: "why is this code like this?" outlives the branch by years, while a
commit body only ever holds what the author thought to write down at the time.

> This skill used to destroy the digest at merge time, from when digests were
> pushed into the public cartopolis repo where anyone could read them. Moving them
> to the private `cartopolis-agent-context` repo removed that reason. See
> [docs/notes/agent-context-refs.md](../../../docs/notes/agent-context-refs.md)
> for what the destruction machinery was and why it is now only a manual escape
> hatch.

## Order

**1. Read the context.** Run `branch-context` and confirm the branch actually is
finished: nothing left stubbed, the open questions answered, the verification the
session claimed actually run. A digest header saying `uncommitted at stop: 12
file(s)` means you are not looking at all the work that existed.

**2. Check what the branch was *for*.** A branch often drifts across several
sessions. The oldest session file usually holds the design decision; the later
ones only execute it. If what you are about to merge no longer matches that
intent, this is a conversation, not a merge.

**3. Verify it here.** The branch is judged on this machine now, not by opening
a pull request:

```bash
.claude/skills/finish-branch/verify-branch
```

It runs the commands out of `.forgejo/workflows/ci.yml` — `cargo fmt --check`,
`cargo test --locked --workspace`, and, when the diff can change a frame, the
two headless smokes and the wasm/android target checks. It prints what it
skipped and why, and a pass writes a receipt keyed on the head sha.

**4. Land it.** One command, and it does the verify above if you have not:

```bash
.claude/skills/finish-branch/merge-to-main                 # land
.claude/skills/finish-branch/merge-to-main --deploy wasm   # land, then publish
```

**Keep the node you worked on.** The gate runs on the node this worktree holds
— a VPS node or a Mac — and only queues for a VPS node when it holds none, so
releasing it first just buys a wait. `--deploy` takes `midas deploy`'s targets
(`sim`, `atlas`, `web`, `wasm`, `apk`, comma-separated) and runs them after the
landing; a failed deploy leaves the landing in place and names what to re-run.

It rebases onto `origin/main`, runs the gate, pushes the branch, fast-forwards
`main` to it and closes the ticket. **No pull request.** `ci.yml` has no push
trigger on branches, so a PR was the only way to start a run — which meant every
landing paid for two full 15-25 minute runs of one tree on a capacity-3 runner,
and the last eight merges before this change spent a median of *zero seconds*
between opening the PR and merging it. Nobody was reading the verdict it bought.

CI is not switched off: it runs on `main` **nightly** (since 2026-10-16; it ran
on every push before), including the render smokes and both cross-target lanes
that a pull request never had, and the gate's pass is recorded at the midas hub
(`midas verified`), which is what `midas deploy` asks for. What changed is that
its verdict now arrives **after** the landing. That is a real weakening, it is
the trade the speed comes from, and a red `main` is fixed forward — see
[docs/notes/agent-verification.md](../../../docs/notes/agent-verification.md).

Say "merge to main" in the driven lane and this is the step —
[docs/workflow.md](../../../docs/workflow.md#landing-it-the-driven-lane-verifies-its-own-branch-and-pushes-2026-08-30).
Follow the `commit` skill's conventions for any fixup commit along the way.
`--allow-unverified` skips the gate and exists to be visible in a transcript,
not to be routine.

**5. Leave the context ref alone.** There is nothing to clean up. The ref stays
keyed on the branch name, which remains meaningful long after the branch itself
is deleted.

## Deleting one anyway

If a digest genuinely has to go — a credential slipped past the filter's
redaction, or something was pasted that should never have been typed — it takes
both halves, and neither alone is enough:

```bash
CTX=ssh://git@code.garage44.eu:2222/jeroen/cartopolis-agent-context.git
git push "$CTX" --delete refs/agent-context/<branch>   # server prunes on receipt
git update-ref -d refs/agent-context/<branch>          # then this clone
git reflog expire --expire-unreachable=now --all
git gc --prune=now
```

The server's `post-receive.d/prune-agent-context` hook runs `git gc --prune=now`
when it sees such a deletion, which is what makes the objects unretrievable
rather than merely unreachable. Measured: it completes within about 2 s.

**Do not** delete the digest *file* in a new commit and call it removed — the
content stays fully readable in the ref's history. And do not skip the local
half: two stores, two prunes.

If the reason is a leaked credential, **rotate it first**. Deleting a digest does
not un-leak anything that was already fetched.
