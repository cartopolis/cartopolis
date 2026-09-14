---
topic: test-suite-build-cost
triggers: [cargo test, "test suite", "tests are slow", include_bytes, fixture, testdata.rs, doctest, "test = false", "harness", "slot", "-p vs workspace", "cargo check -p", "rebuilt everything", "target dir full", "18 GB"]
updated: 2026-09-14
---

# What the test suite costs

Measured 2026-09-14 in the agent container (8 cores shared with nominatim and
overpass, `CARGO_BUILD_JOBS=6`) in a slot warmed by `cartopolis-slot-warm`.
Method: time the command from a settled state, after touching exactly one file.

## Running the tests is free; building them is the whole cost

1,533 tests execute in **2.3 s** — the slowest single binary, and they run in
parallel. `ci-build-cost.md` measured the same ratio at 764 tests: **execution is
well under one percent of any job that runs them.** So "the tests are slow" is
always a question about compilation, and a test that buys real coverage costs
nothing to run. Touching one leaf system in `cartopolis`:

| command | time |
|---|---|
| `cargo check -p cartopolis` | 20.7 s |
| `cargo build -p cartopolis --lib` | 48.9 s |
| `cargo test -p cartopolis --no-run` | 81.8 s |

## The build scope is part of the artifact key, and a slot is warm for one

The expensive mistake, and it is silent. A slot is warmed with
`--workspace --all-targets`; in a slot where that had *just* finished — a genuine
no-op, 18 s wall against 1.0 s user — the `-p` commands this repo's `CLAUDE.md`
recommends cost:

| in a slot warmed `--workspace` | time |
|---|---|
| `cargo test --workspace … --no-run` | 18 s, no-op |
| `cargo check -p cartopolis` | **72.7 s** |
| `cargo test -p cartopolis --no-run` | **190.1 s** |

Under resolver v2 features unify over **the packages the invocation selects**,
and whether a dev-dependency-bearing target is in the build changes that set too.
Different features are a different fingerprint for every crate above them. Same
mechanism `ci-build-cost.md` records for the five `cargo test -p …` steps that
became one; collapsing those did not remove it, it moved into the gap between
what an agent types and what the slot holds.

The second cost reaches the *next* person. Each resolution keeps its own copy of
the graph: after one session that ran both scopes, `target/deps` held **1,552
rlibs for 481 distinct crates** — four of `bevy_pbr` (285 MB), four of
`bevy_render`, four of `tokio` — and the slot stood at **25 GB against the
warmer's 18 GB ceiling**, which is what makes it drop `incremental/` and rebuild
(~500 s) for whoever claims it next.

**So prefer the workspace commands in a slot.** `-p` buys nothing there — the
packages it would skip are already built. Scope to `-p` outside a slot, or when
staying in one crate for a whole session and paying the first build once.

## `include_bytes!` does not deduplicate, and `const` makes it worse

Eleven test modules in `cartopolis_geo` each carried their own `include_bytes!`
of `groningen_z14.mvt`. Two macro invocations naming one path produce two
statics, so the test binary embedded **twelve copies of 495 KB** — 5.9 MB of a
21.4 MB binary. Nothing reports this: a binary 6 MB fatter than it needs to be
looks exactly like one that is not, and the copies arrived one reasonable test
module at a time.

Collapsing them into one `const` gave **five** copies, not one, and the residue
is the part to remember: **a `const` is substituted into each use site**, so the
array is re-materialised per mention and lands separately in several of this
profile's 256 codegen units. A `static` is one address every use loads from.
`geo/src/testdata.rs` owns all three fixtures and its own tests pin both rules,
because nothing about the calling code reads differently either way.

## A test harness is a link, with or without tests to run

`cargo test` builds a harness for every target that does not decline one. Three
ran nothing: `cartopolis`'s `[[bin]]` — a nine-line shim over the lib — was a
**121.8 MB** link per suite build to execute zero tests; `simulator`'s the same
shape at 7.0 MB; and doctest collection on `cartopolis_core` and
`cartopolis_simulator`, neither of which has a runnable doc fence. Hence
`test = false` on the shims and `doctest = false` on the libs. The client and
`cartopolis_geo` already collected none for a *harder* reason worth not
confusing with this one: rustdoc is not given `.cargo/config.toml`'s
`rustflags`, so a doc-test there trips `simd_baseline`'s `compile_error!`.

## What the three were worth, and what was left alone

Same command and machine, from settled, touching `geo/src/turf.rs`:

| | before | after |
|---|---|---|
| test binaries linked | 17 | **15** |
| bytes linked | 807.1 MB | **672.8 MB** |
| `cartopolis_geo` test binary | 21.4 MB | **15.9 MB** |
| `cargo test --workspace --no-run` after the edit | 31.3 s | **22.2 s** |

Nothing changed about what is tested. **The suite's content was audited and left
alone** — 1,534 functions scanned for no assertion, tautologies, wall-clock
sleeps, network reads and duplicated bodies; the seven without an assertion are
all declared printing or eyeball hatches, and the one duplicated body asserts
`Licence::may_share` on each side of the wire, which is the point. "Clean up the
tests" reads as an invitation to delete some; there are none to delete.
**Moving the 1,025 client tests somewhere cheaper** is answered by
`client-build-cost.md`: extracting `crates/geo` removed 11% of the client and
moved `cargo check -p cartopolis` by 0.4 s, because the tail is Bevy's generics
monomorphised inside our crate, not our line count.
