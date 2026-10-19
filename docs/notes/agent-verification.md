---
topic: agent-verification
triggers: [harness, ci.yml, "--expect", .claude/settings.json, verifier, gate, "add a check", "how do we know it works", verify-branch, merge-to-main, "pull request", "merge to main"]
updated: 2026-10-17
---

# A gate may not depend on anything the agent controls

Read this before adding a check, or before trusting one. It is one rule with two
corollaries, and all three were learned by finding checks that had been passing
for months without checking anything.

## The rule

An agent's work is judged at three levels, and they are not interchangeable:

| Level | Answers | Reachable by the agent |
|---|---|---|
| `cargo test -p cartopolis_geo`, `-p cartopolis` | do the units hold | it writes them |
| `--expect` on a `--shot` run | did a world appear on screen | it chooses the flags |
| CI's verdict on the pushed head | both of the above, reproducibly | **no** |

Where nobody is in the room, only the third may decide that work is done. That
is why publishing still waits for it: `tools/deploy-main` refuses a commit CI
never passed — the mechanism is in
[workflow.md](../workflow.md#the-verdict-is-still-cis-and-it-arrives-afterwards).

The first two are not lesser checks, they are *faster* ones, and their job is to
fail before CI has to. Treating either as a gate is the mistake: the thing being
judged picks the invocation.

## Landing trades this deliberately (2026-08-30)

The rule above cost two full 15-25 minute CI runs per landing on a capacity-3
runner — one on the pull-request ref, one on the merge commit — and the first of
the two was being merged after a median of zero seconds, i.e. not read. A
session now runs the gate itself
([`verify-branch`](../../.claude/skills/finish-branch/verify-branch)) and pushes
`main` with no pull request at all.

What is kept of the rule, and it is what makes the trade survivable:

- **The invocations are not the agent's to choose.** They are fixed in a
  checked-in script that mirrors `ci.yml` command for command — corollary 1
  satisfied, in the only way it can be, by not being prompt text.
- **A pass is a receipt keyed on the head sha**, and `merge-to-main` refuses a
  head without one. Committing or rebasing invalidates it.
- **The independent run still happens**, on the push to `main`, where it runs
  *more* than a pull request did (the smokes and both cross-target lanes are
  `main`-only). Its verdict now arrives after the landing; a red `main` is fixed
  forward.
- **Publishing is unmoved.** `tools/deploy-main` still refuses a commit whose CI
  never went green. Landing on no verdict is recoverable and publishing on no
  verdict is not — that asymmetry is the whole reason this is safe to give away
  at the landing end and not at the deploy end.

What is genuinely lost: CI's *environment* — a clean checkout, a cold cache, a
machine the session cannot touch. Neither of the first two decides any step in
the gate, and the runner's rustfmt is byte-identical to the container's. The
honest statement is that `main` is now protected by a script the agent runs, not
by one it cannot reach.

The autonomous lane — an issue watcher on the VPS that built tickets unattended
— kept the old rule in full until it was retired with the tickets on 2026-10-17
([retired/ticket-workflow.md](retired/ticket-workflow.md)): it held no
checkout, so it could not run the local gate, and it landed nothing CI had not
passed, by dispatching `ci.yml` on the branch ref. Anything that ever lands work
with nobody in the room again owes the same.

## Corollary 1 — a check written as prompt text is advice, not a gate

Advice drifts and nothing reports it. The autonomous lane's verification step was
`cargo test -p cartopolis --bin cartopolis` in three prompts and two docs. After the
lib+shim split that command selects a nine-line binary with no tests in it: it
ran **0 of 207 tests and exited 0**, for as long as the shim has existed.

So a new check earns its place in `.forgejo/workflows/ci.yml`, where it runs
whether or not a model read the sentence describing it. Prompt text may *point
at* the gate; it may not be the gate.

## Corollary 2 — anything CI cannot see needs a runtime self-check

`~/.claude/settings.json` had `hooks.Stop: []` while `agent-context-push` sat
installed and documented as running every turn — and on the VPS the same hook,
still matching the project's old name, did nothing at all for weeks after the
rename. No push, no log line. A repository cannot test a file it does not
contain, and `git log` will never show one going stale.

The answer, on 2026-10-17, was to keep what the workflow needs in the
repository: hooks are registered in its own `.claude/settings.json` and live in
`.claude/hooks/`, so every checkout carries them and a review sees them change.
Where an out-of-repo dependency is still unavoidable — container images, the
environment an editor launches agents with, midas's own session hooks —
something that *does* run must verify it and say so, reporting the real state
rather than asserting it, as midas's session hook does for the build workshop.

## What no gate here covers, and must not be faked

CI renders on Mesa lavapipe. Geometry, placement, culling and visibility are the
same picture as a GPU's; **colour, exposure, bloom and lighting are not**, ground
level never settles, and `VK_ERROR_DEVICE_LOST` has no CPU analogue at all — the
per-frame streaming caps stay a rule enforced by reading. `software_renderer` is
in every metrics sink so a capture says which it was. The bounds and their
measurements are in
[headless-shots-software-renderer.md](headless-shots-software-renderer.md).

A change in that territory is not verifiable by anything above. It needs eyes on
a workstation, and saying so is the correct outcome — not an approximation
dressed as a pass.
