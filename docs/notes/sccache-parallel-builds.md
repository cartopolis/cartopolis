---
topic: sccache-parallel-builds
triggers: [sccache, parallel agents, worktree, CARGO_TARGET_DIR, build cache, cold build, rustc-wrapper]
updated: 2026-07-30
---

# sccache shares builds across worktrees — unless CARGO_TARGET_DIR is exported

Measured 2026-07-30, workstation, sccache 0.17.0, `vibe_sim --profile dev-bevy`
(154 cacheable rustc invocations). **An earlier version of this note concluded
sccache could not share across target dirs at all. That conclusion was wrong**,
and the correction matters more than the original: the failure was a single
environment variable.

## The result

Fresh worktree, fresh default `target/`, warm cache:

| | cold | warm, second worktree |
|---|---|---|
| wall time | 76 s | **49 s** |
| Rust cache hits | — | **132 / 154 (85.6 %)** |
| cargo lock contention | — | none — separate target dirs |

The ~22 misses are crates downstream of proc-macros: a proc-macro `.so` is
non-cacheable, rebuilt per target dir, and byte-varies, so its direct dependents
key differently. Everything else cascades from the cache.

## The root cause

**sccache hashes the values of `CARGO_TARGET_DIR` and `CARGO_BUILD_TARGET_DIR`
into the Rust cache key.** Isolated probe: identical compile, only that env var
differing → miss. Every earlier test exported it (one value per worktree), which
produced a universal 0 % hit rate and sent the investigation everywhere else
first.

Verified NOT in the key: `--out-dir`, `-L dependency=…`, `--extern` *paths*
(contents are hashed, correctly), the `OUT_DIR` env var. The cwd IS in the key,
but cargo runs rustc with cwd = the *package* directory — the shared registry
for every dependency — so worktrees don't differ there. (`SCCACHE_BASEDIRS`
exists for cwd normalisation; not needed for this reason.)

## The rules that keep it working

1. **Never export `CARGO_TARGET_DIR` / `CARGO_BUILD_TARGET_DIR`.** Every
   worktree builds into its default `target/`. This is what makes worktrees
   parallel: separate dirs → no cargo lock; sccache → no recompilation.
2. The wrapper lives in `~/.cargo/config.toml`, never the repo's committed
   `.cargo/config.toml` — CI has no sccache and a missing rustc-wrapper fails
   every job.
3. `dev-bevy` deps build with `debug = false` (committed): their `.dwo` files
   embed the absolute target-dir path, so a shared artifact would point its
   debug info at another worktree's (possibly deleted) target. Also most of the
   profile's disk weight. Workspace crates keep full debug.

Per-worktree disk after the debug change: **518 MB** for the vibe_sim graph
(was ~700 MB with dep debuginfo; the full client profile was 51 GB, dominated
by exactly this).

## Method failures worth remembering

The wrong conclusion survived three of my own probe bugs, each of which
produced a plausible 0 %: zsh doesn't word-split unquoted variables (the flag
string arrived as one argument — `CannotCache(missing input)`); sccache's parser
needs `--edition=2021`, not `--edition 2021` (space form reads `2021` as an
input file); and `cargo clean -p` without `--profile dev-bevy` removes nothing
from a non-default profile, making rebuild tests no-ops. Check the *server log*
(`SCCACHE_ERROR_LOG` + `SCCACHE_LOG=sccache=debug`) before believing any
hit-rate number: `CannotCache(...)` lines mean the probe never tested what it
claimed to.

## Not measured

The full Bevy client graph. Proc-macro fan-out is larger there, so the hit rate
will be lower than 85 %, but the mechanism is the same. And the VPS is
unaffected either way: its container bakes one constant `CARGO_TARGET_DIR` for
one shared target dir, so the env value never varies there.
