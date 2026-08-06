---
topic: agent-verification
triggers: [harness, "agent:done", "agent:ci", watcher.ts, ci.yml, "--expect", Stop hook, agent-context-push, verifier, gate, "add a check", "how do we know it works"]
updated: 2026-08-06
---

# A gate may not depend on anything the agent controls

Read this before adding a check, or before trusting one. It is one rule with two
corollaries, and all three were learned by finding checks that had been passing
for months without checking anything.

## The rule

An agent's work is judged at three levels, and they are not interchangeable:

| Level | Answers | Reachable by the agent |
|---|---|---|
| `cargo test -p viberfox_geo`, `-p viberfox` | do the units hold | it writes them |
| `--expect` on a `--shot` run | did a world appear on screen | it chooses the flags |
| CI's verdict on the pushed head | both of the above, reproducibly | **no** |

Only the third may decide a label, land a branch, or end a ticket. `agent:done`
therefore means "CI agreed", not "the session said so" — the mechanism is in
[workflow.md](../workflow.md#the-verdict-is-cis-not-the-sessions).

The first two are not lesser checks, they are *faster* ones, and their job is to
fail before CI has to. Treating either as a gate is the mistake: the thing being
judged picks the invocation.

## Corollary 1 — a check written as prompt text is advice, not a gate

Advice drifts and nothing reports it. The autonomous lane's verification step was
`cargo test -p viberfox --bin viberfox` in three prompts and two docs. After the
lib+shim split that command selects a nine-line binary with no tests in it: it
ran **0 of 207 tests and exited 0**, for as long as the shim has existed.

So a new check earns its place in `.forgejo/workflows/ci.yml`, where it runs
whether or not a model read the sentence describing it. Prompt text may *point
at* the gate; it may not be the gate.

## Corollary 2 — anything CI cannot see needs a runtime self-check

`~/.claude/settings.json` had `hooks.Stop: []` while `agent-context-push` sat
installed and documented as running every turn. No digest, no branch push, no log
line. A repository cannot test a file it does not contain, and `git log` will
never show one going stale.

Where an out-of-repo dependency is unavoidable — hook registrations, the watcher
on the VPS, container images — something that *does* run must verify it and say
so. The `SessionStart` hook is the pattern: it checks that the `Stop` hook is
registered and reports the real HEAD, rather than asserting either.

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
