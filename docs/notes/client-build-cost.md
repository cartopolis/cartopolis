---
topic: client-build-cost
triggers: [slow build, compile time, crate split, cargo timings, profile, dev-bevy, target dir, incremental, mold, lld, linker, "link time", "print link-args", save-temps, fuse-ld]
updated: 2026-09-16
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

## The codegen-versus-link split, measured at last (2026-08-24)

"Do the third one first" is done. Measured in the **agent container** (8 cores,
`CARGO_BUILD_JOBS=6`, deps warm, `cargo build -p cartopolis --timings` after
deleting `incremental/cartopolis-*`), so the absolute numbers are ~5× the
workstation's and only the *proportions* transfer:

| phase | time | share |
|---|---|---|
| `cartopolis` lib — frontend | 67.8 s | 26% |
| `cartopolis` lib — codegen | 104.2 s | 41% |
| `cartopolis` **bin** — the nine-line shim | 84.5 s | 33% |
| total | 257 s | |

Cargo emits those first two as explicit `sections` in the `--timings` HTML
(`UNIT_DATA[…].sections`), so this needs no profiler:

```bash
python3 -c "import re,json;h=open('cargo-timing.html').read();\
print([u for u in json.loads(re.search(r'const UNIT_DATA = (\[.*?\]);',h,re.S).group(1)) if u['duration']>1])"
```

**The 6:1 ratio above is an artefact of how it was derived.** It came from
`cargo check` (7.1 s) against a full build (52.4 s), and everything that is not
the frontend was then called codegen — but a third of the difference is the *bin*
unit, which the frontend figure does not contain at all. Codegen-to-frontend
measured directly is about **1.5:1**. Monomorphisation is still the largest single
term and the crate-split analysis above still stands; it is not a 6:1 term.

**The build is serial at the cargo level.** `CONCURRENCY_DATA` reports at most
**one** active unit for the whole 257 s: the lib, then the bin, which cannot start
until the lib's rlib exists. `codegen-units = 256` parallelises *inside* the lib,
and nothing parallelises the bin. So the last third of a cold-incremental client
build runs five of six job slots idle, and raising `CARGO_BUILD_JOBS` cannot
touch it.

## mold is ~2.6× faster at linking, and that is worth ~1.5 s

| linker | round 1 | 2 | 3 |
|---|---|---|---|
| `lld` | 2.43 s | 2.40 s | 2.38 s |
| `mold` 2.42 | 1.10 s | 0.82 s | 0.82 s |

Real, reproducible, and **smaller than it sounds**: the link is 2.4 s of a ~15 s
warm rebuild, so adopting mold buys ~10% of an edit loop, not a new one. `ci.yml`
already sets `-C link-arg=-fuse-ld=mold` and can stay; `.cargo/config.toml` still
says `lld` because a workstation without mold installed would fail to link, and
that trade has not changed.

Note what this does **not** say: the 84.5 s bin unit is not 84.5 s of linking.
The link is 2.4 s. The rest is rustc's own work plus reading the dependency rlibs
back from a 90%-full overlay filesystem with a cold page cache.

### Two traps, both of which produced a confident wrong answer here first

**Shadowing `ld.lld` on `PATH` does nothing.** `clang -fuse-ld=lld` resolves to
the absolute `/usr/bin/ld.lld` — `clang -fuse-ld=lld -### /dev/null` prints it —
so putting a `mold` symlink named `ld.lld` earlier in `PATH` is a placebo. An
A/B run that way "measured" mold at 15/14/15 s against lld's 15/17/17 and was
reporting nothing but run-to-run noise, with mold never once invoked. Verify an
interception before trusting it: shim the tool with a script that logs its argv.

**A first link after a quiet period is cold-cache.** The very first measurement
here was `lld` at 30 s against `mold` at 15 s — an apparent 2×, entirely page
cache. Any linker comparison needs a discarded warm-up link and alternating
rounds; the tell was that editing a *source file* (15.6 s, which includes a link)
came out faster than a link on its own.

### How to measure the link without a full rebuild

Changing `RUSTFLAGS` to swap linkers rebuilds the world, because cargo hashes it
into every unit's fingerprint. Capture the real command and replay it instead:

```bash
touch crates/cartopolis/src/main.rs
cargo rustc -p cartopolis --bin cartopolis -- --print link-args -C save-temps > /tmp/linkargs.txt
```

`--print link-args` is stable. `-C save-temps` is the load-bearing half: rustc
deletes the `.rcgu.o` files after a successful link, and without it the replay
fails on missing objects. Then run that command line with `-fuse-ld=lld` swapped
for `--ld-path=/path/to/mold`, redirecting `-o` somewhere harmless.

## Sharing generic code across crates is SLOWER here (measured 2026-09-16)

The monomorphisation analysis above ends with "Bevy exports no shareable
instantiations, and every `Query`, `Res`, `SystemParam` … our code names is
monomorphised and LLVM'd inside `cartopolis`", because `opt-level = 3` on
dependencies turns cross-crate generic sharing off. There is a flag that turns it
back on without touching the optimisation level — `-Zshare-generics=y` — and the
note has invited somebody to try it for a month. It loses.

Measured in a build slot (8 shared cores), all 559 dependencies rebuilt with
`RUSTFLAGS="… -Zshare-generics=y"` (32 min 12 s for that build), then the same
one-line edit to the crate root timed in both configurations, alternating:

| | round 1 | round 2 |
|---|---|---|
| as shipped | 222 s | 198 s |
| `-Zshare-generics=y` | 230 s | 251 s |

So it is worse, on the same edit, in the same slot, within minutes of itself. It
also requires a nightly compiler (or `RUSTC_BOOTSTRAP`) for the whole graph, and
one `RUSTFLAGS` value forks every artifact in the build directory. Do not spend
another hour on it; the mechanism in the section above is real but the flag that
addresses it does not pay.

**rustc's parallel front end is real and too small to adopt.** `-Zthreads=8` on
the client library's full compile, replayed outside cargo so nothing thrashed:
160 s against 171 s, about 6 %. Type-checking is only about a quarter of that
unit and the rest is codegen, which the flag does not touch. It needs nightly for
the same reason, for 6 %.

**How to time one unit without rebuilding the world.** Changing `RUSTFLAGS`
re-fingerprints every unit, so an A/B through cargo rebuilds hundreds of crates
each way. Capture the compiler's own command line and replay it instead — the
same trick the linker section above uses for `--print link-args`:

```bash
touch crates/cartopolis/src/lib.rs
cargo build -p cartopolis -v 2>&1 | grep 'crate-name cartopolis ' > /tmp/cmd.txt
# then strip the `Running \`…\``, drop the sccache prefix and `-C incremental=…`,
# point --out-dir at a scratch directory, and run it with and without the flag
```

**And the replay needs cargo's environment or it silently measures nothing.**
Run bare, the derive macros panic — `proc-macro-crate` reads `CARGO_MANIFEST_DIR`
to find the manifest — and the compile ends in 5,941 errors after type-checking,
*before codegen*. It takes about as long as a real compile (173 s vs 184 s), so
the timing looks plausible and is measuring a different thing. Set at least
`CARGO_MANIFEST_DIR`, `CARGO_PKG_*`, `CARGO_CRATE_NAME` and `OUT_DIR`, and check
the run produced an `.rlib` before believing its clock.

## What the edit loop actually costs in a build slot (2026-09-16)

Same box, a settled slot, one line changed in `systems/map/water.rs`, load 4-5:

| after one leaf edit | |
|---|---|
| `cargo check --workspace --exclude cartopolis_android --all-targets` | 16 s |
| `cargo build -p cartopolis` | 17-30 s |
| `cargo test --workspace --exclude cartopolis_android --no-run` | 34 s |

That is roughly 5× the workstation figures at the top of this note, which is what
the hardware difference predicts. **Nothing here is pathological**, which is worth
recording because "the build is slow" is usually about something else: the same
client-library compile measured **171 s at load 10 and 352 s at load 29**, so what
moves build time on this machine is how many builds share the eight cores, not
any flag in this file.

## Not measured

Whether a cold full build is dominated by Bevy itself — it certainly is, and no
rearrangement of our crates changes that. The numbers above deliberately hold
dependencies warm, because that is the state every real edit starts from.

`fast-dev` (Bevy dynamic linking) is still the one lever of the original three
with no number against it. It is the one that plausibly moves the bin unit, since
what it changes is exactly what that unit does.
