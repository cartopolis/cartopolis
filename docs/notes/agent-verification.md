---
topic: agent-verification
triggers: [harness, "agent:done", "agent:ci", watcher.ts, ci.yml, "--expect", Stop hook, agent-context-push, "who checks the agent", verifier, scene smoke]
updated: 2026-08-06
---

# What actually checks an agent's work

Written for you, on the day the answer changed (2026-08-06). Read it once; the
individual mechanisms are documented where they live, and this is only the map.

The workflow in [workflow.md](../workflow.md) is about *who owns* a ticket. This
is the other question: when a session says it is done, **what disagreed with it?**
For a long time the honest answer was "nothing", in three separate ways.

## The three that were broken

**The digest and the push were not running.** `hooks.Stop` in the workstation's
`~/.claude/settings.json` was an empty array. The script was installed, the docs
described it, the SessionStart banner told every session it was pushing for them
— and nothing invoked it. The container's copy was registered the whole time, so
this only ever cost Zed sessions: their reasoning and their commits both stopped
at the machine. Nothing logged, because nothing ran.

**The test gate ran no tests.** `cargo test -p viberfox --bin viberfox` selects
the nine-line shim binary, which has no `#[cfg(test)]` in it. Measured: 0 tests,
exit 0, against a suite of 207. It was the verification step in `workflow.md`,
in the `ticket` skill's spec template and in both of the watcher's prompts, and
it had been passing unconditionally since the lib+shim split.

**Nothing in CI rendered a frame.** Every check proved the code compiles and its
units behave. A black screen does both.

## The three that replace them

| Layer | Answers | Can the agent influence it? |
|---|---|---|
| `cargo test -p viberfox_geo` + `-p viberfox` | do the units hold | it writes them |
| `--expect` in a `--shot` run | did a world appear on screen | it can pass different flags |
| CI's verdict on the pushed head | all of the above, reproducibly | **no** |

The last row is the point. An evaluator the agent can influence is not an
evaluator, so `agent:done` no longer means "the session said so" — a session that
pushes gets `agent:ci`, and the watcher resolves it from Forgejo's commit status
on a later poll. Green → `done`, any failing check → `failed` with the job named.
The mechanism, its timeout policy and its escape hatch are in
[workflow.md](../workflow.md#the-verdict-is-cis-not-the-sessions).

`--expect` is the piece that made a scene checkable at all: the harness measured
~25 things per capture and reported every one of them, but the only thing a
caller could *assert* was the exit code — which is 0 for a flawlessly rendered
empty scene. `--expect 'globe_rendered>0'` closes that, and CI now ends with one
orbit capture behind it. See CLAUDE.md's `--shot` section for the flag and
[headless-shots-software-renderer.md](headless-shots-software-renderer.md) for
what a CPU rasteriser may and may not be asked.

## What still has no verifier, and should not be pretended otherwise

- **Colour, exposure, bloom, lighting.** llvmpipe is a different rasteriser;
  a judgement made against a CI capture is not evidence. `software_renderer` is
  in every metrics sink so a shot says which it was. This stays a workstation
  job, and `cargo shots` stays a driven-lane tool.
- **Ground level.** A 400 m view never settles on a CPU rasteriser. Raising
  `--wait` does not fix it; it fails slower.
- **`VK_ERROR_DEVICE_LOST`.** A CPU rasteriser has no such failure mode, so the
  per-frame streaming caps remain a rule enforced by reading.
- **Taste.** Whether the ticket was worth building.

## The part that lives outside the repo, and therefore outside CI

Two of the three failures above were in files this repository cannot see or
test: a hook registration in `~/.claude/settings.json`, and prompt text in
`/data/agent/issue-watcher/watcher.ts`. Nothing in a `git log` would ever have
shown either going stale.

The mitigation is small and worth keeping: the `SessionStart` hook now *checks*
rather than asserts — it verifies the `Stop` hook is registered and reports the
real HEAD, and warns in the injected context when either is not what the workflow
assumes. If you add another out-of-repo dependency, give it the same treatment;
a claim nobody measures is the failure mode this whole note is about.
