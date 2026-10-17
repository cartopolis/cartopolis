---
name: finish-branch
description: Land a session's finished branch on main — check it does what was asked, run CI's gate, fast-forward main. Use when the user says to merge or land it.
---

# finish-branch

Land a branch, having first checked it is what was asked for.

## Order

**1. Check what the branch was *for*.** Re-read what the user asked for in this
session, the commit bodies (`git log origin/main..HEAD`), and — if the branch
was handed over — the handover note. A branch often drifts across sessions; if
what you are about to land no longer matches what was asked, this is a
conversation, not a merge.

**2. Confirm it is finished.** Nothing left stubbed, the open questions
answered, the verification actually run — not merely claimed in a commit
message. A `wip:` commit is a handover's sweep and is refused (see below).

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
— a VPS node or a Mac — and takes the fastest free node (by the gate's own
measured run time, `midas take --fastest-for`) only when it holds none, so
releasing it first just buys a wait.

**One landing at a time.** Before it rebases, the script takes the turn to land
at the midas hub (`midas land status` shows who has it and who is next) and
keeps it until its push to `main` is done, so a landing in progress is never
overtaken and its gate never tests a base that another landing has already
moved. Run it in the background: the wait for the turn is part of the run.
`--deploy` takes `midas deploy`'s targets
(`sim`, `atlas`, `web`, `wasm`, `apk`, comma-separated) and runs them after the
landing; a failed deploy leaves the landing in place and names what to re-run.

It rebases onto `origin/main`, runs the gate, pushes the branch and fast-forwards
`main` to it. **No pull request.** `ci.yml` has no push
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

Say "merge to main" in a session and this is the step —
[docs/workflow.md](../../../docs/workflow.md#landing-it-say-merge-to-main).
Run it from the session's own worktree: it refuses a dirty tree, and a checkout
other sessions share always has somebody's work in it.
Follow the `commit` skill's conventions for any fixup commit along the way.
`--allow-unverified` skips the gate and exists to be visible in a transcript,
not to be routine.
