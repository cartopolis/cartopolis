---
topic: test-suite-build-cost
triggers: [cargo test, "test suite", "tests are slow", "slow test", report-time, settle_until, talk_until, include_bytes, fixture, testdata.rs, doctest, "test = false", "harness", "integration test", "tests/ directory", "one target", approx, default-features, "-p vs workspace", "cargo check -p", "rebuilt everything"]
updated: 2026-10-27
---

# What the test suite costs

Measured 2026-09-14 in the agent container (8 cores shared with nominatim and
overpass, `CARGO_BUILD_JOBS=6`) in a slot warmed by `cartopolis-slot-warm`, on a
quiet box. Method: time the command from a settled state, after touching exactly
one file.

## Every `tests/*.rs` is a separate binary, and they run one after another

This is the fact that matters most and the one that is easy to miss. Cargo
compiles **one binary per integration-test target**, a direct child of `tests/`
is a target, and `cargo test` then runs those binaries **sequentially** — only
the tests *inside* one binary share libtest's thread pool.

`crates/simulator/tests/` held ten files. That was ten links of the same
statically-linked server — **364.9 MB for 58 tests** — and a run in which each
suite's server startup and its deliberate waits could not overlap any other's:
16.0 s summed. Moved under `tests/integration/` with a `main.rs` that declares
them as modules, it is **one 38.4 MB binary** and the same 58 tests take
**4.4 s**, stable over repeated runs.

What makes that safe is that none of them shares process state: every suite binds
`127.0.0.1:0` and opens its world in its own `tempfile::TempDir`, and none
installs a global logger or writes an environment variable. **A new suite that
needs any of those needs its own target again, and a note saying why.**

| whole suite, nothing to build | |
|---|---|
| before | 23.0 s |
| after | **9.7 s** |

It also halves the *build* after an edit outside the client, because a
`cartopolis_geo` change used to relink all ten: `cargo test --workspace --no-run`
after touching `geo/src/turf.rs` went **26.8 s → 13.0 s**.

## Test *binaries* are the build cost, and most of them ran nothing

`cargo test` builds a harness for every target that does not decline one. Four
had nothing to run: `cartopolis`'s `[[bin]]` — a nine-line shim over the lib —
was a **121.8 MB** link per suite build to execute zero tests; `simulator`'s the
same shape at 7.0 MB; and doctest collection on `cartopolis_core` and
`cartopolis_simulator`, neither of which has a runnable doc fence. Hence
`test = false` on the shims and `doctest = false` on the libs. The client and
`cartopolis_geo` already collected no doctests for a *harder* reason worth not
confusing with this one: rustdoc is not given `.cargo/config.toml`'s
`rustflags`, so a doc-test there trips `simd_baseline`'s `compile_error!`.

| test binaries linked for one full suite build | count | bytes |
|---|---|---|
| before | 17 | 807.1 MB |
| after `test = false` / `doctest = false` | 15 | 672.8 MB |
| after merging the integration targets | **6** | **346.6 MB** |

## A dev-dependency's default features can fork the whole Bevy stack

`cargo test -p cartopolis --no-run` cost **89.3 s** in a slot where
`cargo test --workspace --no-run` had *just* been a true no-op (988 ms, zero
units). Only four units rebuilt, so it was never the dependency graph — but
finding which four took a `--unit-graph` diff and then a diff of the two `rustc`
command lines, which differed in exactly one meaningful place: they linked a
**different `bevy` rlib**.

The cause: `approx` is `cartopolis_core`'s dev-dependency *and* a dependency of
`bevy_winit`, which asks for it with no features. Taking `approx`'s default
(`std`) made its resolved feature set depend on whether core's tests were in the
build — they are under `--workspace`, they are not under `-p cartopolis`.
Features are part of a unit's `-C metadata`, and that hash propagates upward, so
`approx` → `bevy_winit` → `bevy_internal` → `bevy` → `cartopolis` each resolved
to two artifacts. `default-features = false` on the **workspace** entry (a
member's is ignored unless the workspace entry says so, with only a warning)
takes the scope switch to **0.96 s and zero units**.

Two things this is *not*. It is not a general rule that `-p` and `--workspace`
thrash: they keep separate artifact sets that coexist, switching back was always
free, and after the fix `cargo test -p cartopolis` and `cargo test --workspace`
resolve identically (0 units differ, checked with `--unit-graph`). And the
remaining scope differences are **structural, not bugs**: `cargo check -p
cartopolis` (lib only, no dev-dependencies) differs from `--workspace
--all-targets` in 19 units, `-p cartopolis_geo --tests` in 24. A narrower
selection legitimately unifies fewer features, and closing that would mean
putting test-only features into the shipped client.

## `include_bytes!` does not deduplicate, and `const` makes it worse

Eleven test modules in `cartopolis_geo` each embedded their own copy of
`groningen_z14.mvt`: **twelve copies of 495 KB**, 5.9 MB of a 21.4 MB binary.
Collapsing them into one `const` gave **five** copies, not one — **a `const` is
substituted into each use site**, so the array is re-materialised per mention and
lands separately in several of this profile's 256 codegen units. A `static` is
one address every use loads from. `geo/src/testdata.rs` owns all three fixtures
and its own tests pin both rules; the binary is 15.9 MB.

## Running the suite: each binary is as slow as its slowest test

Measured 2026-10-27 on mac-2 (6 cores), the suite built, `cargo test --workspace
--exclude cartopolis_android` alone: **46 s of wall clock for 55 s of CPU** — most
of it waiting, not computing. The binaries run one after another, the tests inside
one in parallel, so a binary costs about its slowest test, and the suite the sum
of those. Per-test times come from libtest without nightly:
`RUSTC_BOOTSTRAP=1 target/debug/deps/<bin> -Z unstable-options --format json --report-time`.

The long poles were all one of three kinds, and each has a cure that keeps what
the test proves:

- **A fixed wait sat out whether or not the thing waited for had happened.** The
  chat loopback suite's `pump` ends after 150 ms of quiet, and `settle` pumped
  each peer three rounds one after another: 42 quiet tails, 6.5 s for one test.
  `settle_until` serves every peer at once and counts quiet only once the
  expected event is there. The calls integration tests talked for a fixed 3 s
  where twenty heard frames was the proof (`talk_until`). Wait for the event,
  with a generous upper bound that fails loudly.
- **A server interval the test had to sit out.** A revoked token ended its open
  connection on a 5 s check; `SimConfig::token_check_ms` is 50 in the test. The
  GNSS receiver's reopen and command pauses, and the voice bridge's hang-up
  pause, are shorter under `cfg(test)`, with a comment saying the long ones are
  for hardware.
- **A brute-force search in the test itself, built at `opt-level = 1`.** One terrain
  test compared 29k probes with every one of 110k triangles: 7.7 s of the
  client's 7.9 s, now a plan-grid lookup and 0.1 s. The sheet stays as big as it
  was, because the refinement budget running out is what the test is about.

| suite, nothing to build | before | after |
|---|---|---|
| whole suite | 46.2 s | **26.5 s** |
| simulator integration | 10.7 s | 6.8 s |
| chat | 8.6 s | 3.1 s |
| client | 7.9 s | 0.8 s |
| calls | 3.1 s | 1.7 s |
| vio | 3.1 s | 1.7 s |

What is left is spread out: the simulator's unit tests (3.7 s) and `geo` (3.5 s)
are many tests' CPU, no single pole; the integration binary's poles are two
physics tests that wait on simulated time (a ball falling and coming to rest, a
kick after `HOLD_AFTER_EDIT_S`), which a test-only shortcut would no longer prove.

`cartopolis_materials` and `carto-view` had test harnesses with nothing in them,
57–58 MB of link each per suite build; both are `test = false` now.

## What was audited and left alone

**The suite's content.** 1,534 functions scanned for missing assertions,
tautologies, wall-clock sleeps, network reads and duplicated bodies. The seven
without an assertion are all declared printing or eyeball hatches; the one
duplicated body asserts `Licence::may_share` on each side of the wire, which is
the point. Nothing to delete — worth saying, because "clean up the tests" reads
as an invitation to. Looked at again 2026-10-27, with the same result: the 44
`#[ignore]`d tests are each a labelled printing, eyeball, network or
known-fault hatch, run by hand, and cost the suite nothing.

**`[profile.test] opt-level = 0`**, the one profile knob nobody had tried — the
rejected experiment in `build-levers-that-do-not-work` was about *dependency*
opt-level, not test targets. Measured: the build after touching a client leaf
went 13.7 s → 13.1 s, and the tests went from 8.3 s to 12.5 s of run time. Four
seconds of running to buy half a second of compiling, so no. (Check this class of
thing with `cargo +nightly test --unit-graph`, not `cargo build --unit-graph` —
the latter reports the **dev** profile for test targets, which makes a real
`[profile.test]` setting look like a no-op.)

**Moving the 1,025 client tests somewhere cheaper** is answered by
`client-build-cost.md`: extracting `crates/geo` removed 11% of the client and
moved `cargo check -p cartopolis` by 0.4 s, because the tail is Bevy's generics
monomorphised inside our crate, not our line count.
