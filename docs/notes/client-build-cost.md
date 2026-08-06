---
topic: client-build-cost
triggers: [slow build, compile time, crate split, cargo timings, profile, dev-bevy, target dir, incremental]
updated: 2026-08-06
---

# Where the client's build time actually goes

Measured 2026-08-06, workstation (22 cores, rustc 1.97.1, clang+lld,
`target-cpu=x86-64-v3`). Method: touch or append a `pub const` to one file, then
time `cargo build -p viberfox --profile dev-bevy` with dependencies already
built. The "cold incremental" rows delete `target/dev-bevy/incremental/viberfox-*`
first, which is what CI and every profile switch effectively do.

| Scenario | Time |
|---|---|
| Edit a leaf system (`water.rs`) | 3.8 s |
| Edit a widely-imported module (`resources.rs`) | 3.9 s |
| Edit `viberfox_core` (rebuilds sim + client) | 4.8 s |
| Compile `viberfox`, deps warm, **incremental cache cold** | **52.4 s** |
| …of which `cargo check` (frontend only) | 7.1 s |
| `cargo check` no-op after working in `dev-bevy` | 9.5 s |

## What this rules out

**The edit loop is not the problem.** At ~4 s it is already finer-grained than
any crate boundary could make it — incremental compilation is doing the work a
crate split is usually credited with. Splitting crates to speed up a 4 s edit is
a category error, and it was the first thing assumed here.

**The cost is codegen and linking, not type-checking.** 7.1 s of the 52.4 s is
the frontend; the remaining ~45 s is LLVM and the link. So anything that
*discards* the incremental cache pays full price: CI (`CARGO_INCREMENTAL=0`),
switching profiles, flipping `solo` / `fast-dev` / `scalar-fallback`, and every
`cargo test`.

## Two structural taxes, both self-inflicted

**The bin-only target doubles the test build.** `viberfox` has no lib target, so
`cargo test -p viberfox --bin viberfox` compiles all ~52k lines a second time
under `cfg(test)` as a separate artifact. Note that `cargo check` does not
compile `cfg(test)` code at all, which is why the suite can rot unseen.

**Two dev profiles are maintained and both are paid for.** `target/` measured
**170 GB**: `debug` 94 GB, `dev-bevy` 72 GB, `release` 2.2 GB, `web-release`
2.1 GB. The contributor docs say `cargo check` (which lands in `dev`) but
`cargo run --profile dev-bevy`, so alternating between them rebuilds the other
set from scratch. The 9.5 s "no-op" check above is entirely that.

## What was done about it (2026-08-06)

Measured on the same machine, same method, after the changes:

| | Before | After |
|---|---|---|
| Edit a geodata file → its tests green | part of a 52.4 s binary compile | **2.2 s** (`cargo test -p viberfox_geo`, 60 tests, no Bevy in the graph) |
| Edit a leaf system → full client suite green | second full `cfg(test)` build | **5.4 s** |
| `cargo check -p viberfox` | — | 7.5 s |
| `cargo check -p viberfox --all-targets` | — | 8.2 s |
| `target/` | 197 GB | **118 GB** |

Four changes, in order of what they bought:

1. **A lib target plus a shim bin.** Bin-only meant `cargo test` had no rlib to
   link and recompiled the whole crate under `cfg(test)`.
2. **One dev profile.** `dev` and `dev-bevy` were both maintained; collapsing
   them removed a duplicate artifact set and the 9.5 s no-op check, and dropped
   workspace debuginfo to `line-tables-only`.
3. **`crates/geo` (`viberfox_geo`).** The Bevy-free half, below.
4. **Deleting the orphaned `target/dev-bevy`** — 79 GB no build config could
   reach any more.

Note the ordering trap this exposed: `cargo check` does **not** compile
`cfg(test)` code, so it missed four broken `include_bytes!` fixture paths that
only `--all-targets` (or `cargo test`) catches.

## The Bevy-free code that used to pay Bevy's compile cost

7,313 lines carrying **69 of the crate's 290 tests** imported no Bevy at all, yet
were compiled and linked as part of a Bevy binary. They are now `crates/geo`:

| Module | Lines | Tests |
|---|---|---|
| `systems/vector_tiles` | 3,496 | 32 |
| `systems/routing` | 1,937 | 16 |
| `systems/theme` | 714 | 4 (egui only, no Bevy) |
| `systems/road_texture` | 617 | 9 |
| `systems/shot_presets` | 282 | 5 |
| `utils/sun` | 267 | 3 |

`vegetation` turned out to be the same story one level down: 1007 lines of which
~870 were pure scatter and meshing, with a 130-line ECS half on top. It split
into `geo::scatter` and `systems::vegetation`, which is what finally freed the
crate — `vector_tiles`' fixture called `scatter_vegetation`, so the boundary
would have compiled and then failed under `cargo test`.

`street_furniture` went the same way on the same day (990 pure / 110 ECS),
which took `mvt-reader` and `geo-types` out of the client's manifest — it was
the last file in `viberfox` parsing MVT. geo is now 7,945 lines and 74 tests,
with a ~1 s edit-to-green loop.

A remaining tier (`terrain`, `height`, `route_ahead`, `collision`, `tile_source`
— ~3.3k lines, ~36 tests) touches Bevy fewer than 15 times each and would likely
split the same way. Not done, and worth weighing rather than assuming: the build
win from the first two splits is already banked, so the third is mostly about
where code lives.

## Not measured

Whether a cold full build is dominated by Bevy itself — it certainly is, and no
rearrangement of our crates changes that. The numbers above deliberately hold
dependencies warm, because that is the state every real edit starts from.
