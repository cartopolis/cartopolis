---
topic: retired/flicker-gate
triggers: ["flicker smoke", "smoke-flicker", "flicker_px", "z-fight", "z-fighting", "z index", "depth stability", "landing gate", "verify-branch"]
updated: 2026-09-13
---

# Retired: the flicker smoke as a gate

From 2026-08-29 to 2026-09-13 a paired street capture ran on every landing that
touched the client — `verify-branch`'s `smoke-flicker` step and a matching
`Headless flicker smoke (street)` job in `ci.yml` — asserting
`flicker_px <= 1500` over Groningen at 100 m. Both are gone. **The `--flicker`
instrument stays** (`shot_harness`, the `flicker_px` metric, `--expect`); what
was retired is running it automatically before anything lands.

## Why

**It never caught a z-fight.** Two weeks, every client landing, no hit. The bugs
named in the commit that introduced it — roads over roads, water over land, the
ghost-pavement duplicates — were all already fixed by then. It was built as a
regression gate for a class of bug that had stopped regressing.

**Meanwhile the z-fighting it was watching for kept happening**, which is the
part that settles the argument: had the gate been working, this would be the
evidence for keeping it. Instead it is evidence that a 1500 px threshold over a
~500 px moving floor at one viewpoint does not see what actually goes wrong. A
fight inside one mesh rasterises deterministically and never flips a pixel; a
fight anywhere the camera is not pointed is not in the frame at all.

**It cost 15–30 minutes per landing and did not fit its own budget.** Measured
2026-09-13: the first of the two captures wrote its image 460 s in, so the pair
needs ~950 s before the process exits — over the step's `timeout 900` on an idle
machine, and well over it on a shared one. It ran on changes that cannot move a
triangle (this retirement was prompted by it blocking a touch-input change).

**Its failures were actively misleading, twice.** The dump path is a fixed
`/tmp/verify_flicker.json` that nothing deleted first, so the failure handler
printed whatever an earlier run had left there — a stale *pass*, read as the
current build's evidence. And the 901 s timeout, with `platform::watchdog`
dumping every thread asleep on a futex, was once diagnosed as contact shadows
stalling the capture; it was the budget running out, and the correction is
written up in [surface-finish-and-occlusion.md](../surface-finish-and-occlusion.md).
A check that manufactures findings is worse than no check.

## What to do instead

Reach for `--flicker` **when you have been editing geometry, the depth-bias
ladder or a coplanar surface group**, at the viewpoint where the problem is —
which is the only time it has ever had anything to say. The argument for the
instrument is unchanged and is still right: a depth fight re-rolls with every
view matrix, so no single still can show one, and depth resolution is geometry,
which lavapipe rasterises faithfully. That makes it one of the few things a
software renderer can judge honestly.

What does not follow from that, and is the mistake this note records, is that a
good instrument makes a good gate.
