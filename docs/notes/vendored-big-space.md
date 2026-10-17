---
topic: vendored-big-space
triggers: [crates/big_space, bevy_compat.rs, big_space, "Bevy bump", "bevy upgrade", "floating origin", ADR-019]
updated: 2026-10-17
---

# `big_space` is a vendored fork, advanced by hand on every Bevy bump

The floating-origin transform propagation (why big_space at all: the frozen
`docs/adr/019-planet-scale-floating-origin.md`) comes from `big_space` 0.10.0,
copied into `crates/big_space` because **upstream tops out at Bevy 0.18** and has
published nothing for 0.19 or 0.20. So there is nothing to upgrade to: each Bevy
bump here is also a port of this crate, done in place.

**What to re-sync when Bevy moves: `src/bevy_compat.rs`.** It is Bevy's own
simpler transform propagation (the variant without dirty-tree marking), copied
in because Bevy's built-in propagation is disabled under big_space and entities
outside a `BigSpace` still need theirs. A Bevy release that changes its
propagation leaves this copy behind silently, so diff it against the new
`bevy_transform` on every bump. The 0.20 move (53964fa5) touched it along with
`grid/propagation.rs` and `validation.rs`.

What else about the copy is deliberate:

- **Library only.** Upstream's examples, benches and dev-dependencies were
  dropped — which silently took the crate's own test suite with it, since every
  `mod tests` reaches for `bevy::prelude`. Restored 2026-07-26, minus
  `bevy_winit` (the client picks its winit backend per target, so it will not
  build from here).
- **It carries our own changes**, so a re-sync is a merge, not a copy-over:
  the unrotated-origin fast path in `global_transform` (4955eea5, bit-identical,
  with the old body kept as the test oracle).
- **Kept diff-comparable with upstream**: it keeps upstream's name
  (`module-graph.md`), its own `[lints]` rather than the workspace's
  `unused = "deny"`, and is excluded from CI's `cargo fmt` gate (`ci.yml`). Do
  not reformat or lint-fix it; that would bury the diff the re-sync needs.

Profiling has never pointed at it: `render-performance-survey.md`, "big_space is
not the bottleneck".
