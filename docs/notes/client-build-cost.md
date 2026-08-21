---
topic: client-build-cost
triggers: [slow build, compile time, crate split, cargo timings, profile, dev-bevy, target dir, incremental]
updated: 2026-08-19
---

# Where the client's build time actually goes

Measured 2026-08-06, workstation (22 cores, rustc 1.97.1, clang+lld,
`target-cpu=x86-64-v3`). Method: touch or append a `pub const` to one file, then
time `cargo build -p cartopolis --profile dev-bevy` with dependencies already
built. The "cold incremental" rows delete `target/dev-bevy/incremental/cartopolis-*`
first, which is what CI and every profile switch effectively do.

| Scenario | Time |
|---|---|
| Edit a leaf system (`water.rs`) | 3.8 s |
| Edit a widely-imported module (`resources.rs`) | 3.9 s |
| Edit `cartopolis_core` (rebuilds sim + client) | 4.8 s |
| Compile `cartopolis`, deps warm, **incremental cache cold** | **52.4 s** |
| …of which `cargo check` (frontend only) | 7.1 s |
| `cargo check` no-op after working in `dev-bevy` | 9.5 s |

## What this rules out

**The edit loop is not the problem.** At ~4 s it is already finer-grained than
any crate boundary could make it — incremental compilation is doing the work a
crate split is usually credited with. Splitting crates to speed up a 4 s edit is
a category error, and it was the first thing assumed here.

**And splitting more of our code out will not move the 45 s tail either.** This
was asked again on 2026-08-13 ("make the build cache more effective by separating
our code from the Bevy path"), so here is the mechanism the numbers above imply
but never state.

A 7.5 s frontend against a 52.4 s full compile is a 6:1 codegen-to-typecheck
ratio, and that ratio is the signature of **monomorphisation**, not of line count.
rustc shares generic instantiations across crates at `opt-level` 0 and 1 and stops
at 2 and 3 — and `[profile.dev.package."*"] opt-level = 3` (the Bevy recipe that
makes a debug build playable) puts every dependency in the second group. So Bevy
exports no shareable instantiations, and every `Query`, `Res`, `SystemParam`,
`MaterialPlugin` and `IntoScheduleConfigs` our code names is monomorphised and
LLVM'd **inside `cartopolis`**. That tail is Bevy's generics wearing our crate's
name. Moving our own pure lines out does not move it; the ~277 ECS system
signatures that instantiate them stay behind by definition.

The evidence was already here and unread: `crates/geo` removed 7,313 lines, 11% of
the client, and `cargo check -p cartopolis` went 7.1 s → 7.5 s. The extraction was
worth doing — it took 60 geodata tests from "part of a 52.4 s binary compile" to
2.2 s, which is what the table below measures — but it bought that on the *test*
loop, not the compile.

Estimated value of extracting every genuinely Bevy-free line still in the client
(~2,650 lines, see the correction below): **1–2 s off a 52.4 s cold compile, ~0 off
the 3.8 s edit loop.** It is a code-organisation change with a test-loop payoff.
Budget it as that, not as a build-time fix.

A crate split also *competes with* the mechanism that already makes the edit loop
fast: `codegen-units = 256` parallelises codegen within one crate, and a split
converts that into a serial two-crate dependency chain. On a many-core workstation
a split can make the cold build marginally slower.

**Where the build time actually is, if someone wants to spend a day on it:** the
three unmeasured levers are `fast-dev` (Bevy dynamic linking — it collapses the
link, and nothing in this tree has ever measured it against the 52.4 s figure),
`mold` over `lld`, and `cargo build --timings` to split that 45 s into codegen
versus link in the first place. Do the third one first.

**The cost is codegen and linking, not type-checking.** 7.1 s of the 52.4 s is
the frontend; the remaining ~45 s is LLVM and the link. So anything that
*discards* the incremental cache pays full price: CI (`CARGO_INCREMENTAL=0`),
switching profiles, flipping `fast-dev` / `scalar-fallback`, and every
`cargo test`.

## Two structural taxes, both self-inflicted

Both are described as they *were* when measured; the section below records what
was done about them the same day.

**The bin-only target doubled the test build.** `cartopolis` had no lib target, so
`cargo test -p cartopolis --bin cartopolis` compiled all ~52k lines a second time
under `cfg(test)` as a separate artifact. Note that `cargo check` does not
compile `cfg(test)` code at all, which is why the suite can rot unseen.

> That command outlived the fix and became a trap: with the shim bin in place it
> selects a nine-line target with no tests in it, so it runs **0 tests and exits
> 0**. It was still the autonomous lane's verification gate in `workflow.md` and
> in the `ticket` skill until 2026-08-06. The command is `cargo test -p cartopolis`.

**Two dev profiles are maintained and both are paid for.** `target/` measured
**170 GB**: `debug` 94 GB, `dev-bevy` 72 GB, `release` 2.2 GB, `web-release`
2.1 GB. The contributor docs say `cargo check` (which lands in `dev`) but
`cargo run --profile dev-bevy`, so alternating between them rebuilds the other
set from scratch. The 9.5 s "no-op" check above is entirely that.

## What was done about it (2026-08-06)

Measured on the same machine, same method, after the changes:

| | Before | After |
|---|---|---|
| Edit a geodata file → its tests green | part of a 52.4 s binary compile | **2.2 s** (`cargo test -p cartopolis_geo`, 60 tests, no Bevy in the graph) |
| Edit a leaf system → full client suite green | second full `cfg(test)` build | **5.4 s** |
| `cargo check -p cartopolis` | — | 7.5 s |
| `cargo check -p cartopolis --all-targets` | — | 8.2 s |
| `target/` | 197 GB | **118 GB** |

Four changes, in order of what they bought:

1. **A lib target plus a shim bin.** Bin-only meant `cargo test` had no rlib to
   link and recompiled the whole crate under `cfg(test)`.
2. **One dev profile.** `dev` and `dev-bevy` were both maintained; collapsing
   them removed a duplicate artifact set and the 9.5 s no-op check, and dropped
   workspace debuginfo to `line-tables-only`.
3. **`crates/geo` (`cartopolis_geo`).** The Bevy-free half, below.
4. **Deleting the orphaned `target/dev-bevy`** — 79 GB no build config could
   reach any more.

Note the ordering trap this exposed: `cargo check` does **not** compile
`cfg(test)` code, so it missed four broken `include_bytes!` fixture paths that
only `--all-targets` (or `cargo test`) catches.

## The Bevy-free code that used to pay Bevy's compile cost

7,313 lines carrying **69 of the crate's 290 tests** imported no Bevy at all, yet
were compiled and linked as part of a Bevy binary. They are now `crates/geo`:

| Module | Lines | Tests | Moved? |
|---|---|---|---|
| `systems/vector_tiles` | 3,496 | 32 | yes |
| `systems/routing` | 1,937 | 16 | yes |
| `systems/theme` | 714 | 4 (egui only, no Bevy) | **no — still in `cartopolis`** |
| `systems/road_texture` | 617 | 9 | yes |
| `systems/shot_presets` | 282 | 5 | **no — still in `cartopolis`** |
| `utils/sun` | 267 | 3 | **no — still in `cartopolis`** |

> **Correction, 2026-08-13.** This table read as a list of what moved, and three
> rows of it never did. `crates/geo/src` holds `furniture`, `mip`, `places`,
> `pois`, `road_texture`, `routing`, `scatter` and `vector_tiles` — `theme`,
> `shot_presets` and `utils/sun` are still client files, so roughly 1,260 lines
> and 12 tests of the claim above are outstanding. `theme` cannot go to `geo` as
> it stands: it is egui-only, and `geo` has no egui and should not grow one.

`vegetation` turned out to be the same story one level down: 1007 lines of which
~870 were pure scatter and meshing, with a 130-line ECS half on top. It split
into `geo::scatter` and `systems::vegetation`, which is what finally freed the
crate — `vector_tiles`' fixture called `scatter_vegetation`, so the boundary
would have compiled and then failed under `cargo test`.

`street_furniture` went the same way on the same day (990 pure / 110 ECS),
which took `mvt-reader` and `geo-types` out of the client's manifest — it was
the last file in `cartopolis` parsing MVT. geo is now 7,945 lines and 74 tests,
with a ~1 s edit-to-green loop.

A remaining tier (`terrain`, `height`, `route_ahead`, `collision`, `tile_source`
— ~3.3k lines, ~36 tests) touches Bevy fewer than 15 times each and would likely
split the same way. Not done, and worth weighing rather than assuming: the build
win from the first two splits is already banked, so the third is mostly about
where code lives.

## The crate-root module was its own chokepoint (fixed 2026-08-19)

One lever this note's crate-split analysis never listed: **module splitting
inside the crate**. `codegen-units` partitions CGUs by module, and `lib.rs`
carried ~2,600 lines of sky/weather/frame-pacing implementation in the crate
*root* — the same partition as all 35 `add_systems` calls and 12
`MaterialPlugin` instantiations, i.e. the densest generic-instantiation site in
the workspace. Tuning one cloud constant re-codegened all of it. That block is
`systems/map/sky.rs` + `systems/dev/frame_pacing.rs` now (registration stayed
in `lib.rs`), which — unlike a crate split — *adds* intra-crate codegen
parallelism instead of trading it away. Not re-measured with `--timings` yet;
the mechanism is the same one the `codegen-units = 256` paragraph above
documents.

## Not measured

Whether a cold full build is dominated by Bevy itself — it certainly is, and no
rearrangement of our crates changes that. The numbers above deliberately hold
dependencies warm, because that is the state every real edit starts from.
