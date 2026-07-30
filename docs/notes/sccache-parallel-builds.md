---
topic: sccache-parallel-builds
triggers: [sccache, parallel agents, worktree, CARGO_TARGET_DIR, build cache, cold build, rustc-wrapper]
updated: 2026-07-30
---

# sccache does not share Rust builds across target dirs here

Measured 2026-07-30, workstation, sccache 0.17.0, `vibe_sim --profile dev-bevy`
(158 rustc invocations — small enough to iterate, real enough to trust).

The question was whether a second git worktree can build without either
recompiling everything or serialising behind the first one on cargo's target-dir
lock. That is the gate on running several local agent sessions that build.

**The answer is no.** Three configurations, one result:

| Setup | Rust cache hits |
|---|---|
| Worktree B, separate target dir, after A populated the cache | **0.00 %** (153 misses) |
| Same, with `--remap-path-prefix` normalising both worktree paths | **0.00 %** (153 misses) |
| **Same worktree**, different target dir | **0.00 %** (153 misses) |

The third row is the one that settles it: with the source path held identical,
changing only `CARGO_TARGET_DIR` still misses every Rust unit. So the key varies
with the target directory, and no amount of path normalisation on the *source*
side helps. The one hit in every run was the C compilation in `libsqlite3-sys`,
which caches fine — the C/C++ hit rate was 100 %.

Not a plumbing failure: 493 MB landed on disk, zero write errors, zero read
errors, `Average cache write 0.001 s`. sccache stored everything and then could
not find any of it from a different target dir.

Even with perfect keys the ceiling is low. Of 158 invocations, sccache reports
35 non-cacheable on `crate-type` (proc-macro and dylib units, which Bevy has many
of) and 2 on `incremental`, which `[profile.dev]` turns on and `dev-bevy`
inherits. That is ~23 % unreachable before the keys are even considered.

## What this means for parallel agents

A second worktree that *builds* costs a full separate build — 50–90 GB and the
whole compile time — or shares `CARGO_TARGET_DIR` and serialises on cargo's
lock. sccache removes neither horn. Parallel sessions that read, edit and commit
are fine; parallel sessions that each run `cargo build` are not, and the
workflow should not be designed as though they are.

The VPS watcher already lives with this: `AGENT_MAX_CONCURRENT=2` with one
shared target dir, and its own comment says extra concurrency "mostly buys lock
contention, not throughput". That was the right call and this measurement is why.

## Rejected

- **`--remap-path-prefix`** — the obvious hypothesis (debug info embeds absolute
  source paths, `debug = 1` is on). Measured: no effect. Row 3 shows why the
  source path was never the variable.
- **Leaving sccache wired up anyway.** It was configured at
  `~/.cargo/config.toml` (never the repo's — CI has no sccache and a missing
  `rustc-wrapper` fails every job) and reverted after measuring. A wrapper on
  every rustc invocation that demonstrably returns nothing is worse than absent,
  because the next person debugging a slow build has to rule it out first. The
  binary is still at `~/.cargo/bin/sccache` if someone wants to retest a later
  version.

## Not tested

Whether a Bevy-scale build behaves differently. It should be worse, not better —
more proc-macro and dylib units, which are the non-cacheable ones — but the
number is unmeasured. A cold `dev-bevy` build of the full client takes tens of
minutes, and the vibe_sim result was unambiguous enough not to spend it.
