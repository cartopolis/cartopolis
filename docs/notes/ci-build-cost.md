---
topic: ci-build-cost
triggers: [ci.yml, rust-cache, sccache, valid_volumes, actcache, CARGO_BUILD_JOBS, mold, "ci slow", "cache miss", "cargo test workspace", forgejo-runner, "disk full", cartopolis-ci]
updated: 2026-08-12
---

# What CI actually costs, measured

Every number here comes from `action_task_step` in Forgejo's SQLite database,
which is the only place per-step timings survive:

```sql
select s.name, (s.stopped - s.started)
from action_task_step s
where s.task_id in (select id from action_task where job_id in
      (select id from action_run_job where run_id = <N>))
order by s.id;
```

Use it before changing anything here. Every wrong conclusion recorded below came
from reasoning about CI instead of measuring it.

## The measurements, 2026-08-12

The `test` job, which is the whole of a pull request's CI:

| run | configuration | test time |
|-----|---------------|-----------|
| 302 | five `cargo test -p …`, cold | 1918 s |
| 304 | five `cargo test -p …`, rust-cache warm | 272 s |
| 309 | one `cargo test --workspace`, cold, no sccache | 1001 s |
| 310 | one `cargo test --workspace`, rust-cache warm | 163 s |

**One invocation instead of five is worth about 45%, warm or cold.** Under
resolver v2 each `cargo test -p X` unifies features over only the packages it
selects, so five invocations meant five different feature sets for the shared
Bevy dependencies and five rebuilds of them. The tell was `big_space` — a small
vendored crate — taking 439 s immediately after a step that had just compiled the
entire Bevy stack. The cost of collapsing them is the cheapest-first ordering that
used to fail in seconds on a geodata regression.

Before collapsing them again after some future split, check what `--workspace`
changes: `cartopolis_geo` and `cartopolis_simulator` carry `tests/` directories
that a `--lib` would drop, and `big_space`'s only doc fences are ```ignore, so the
`--lib` it used to be given was conservatism rather than a real exclusion.

## rust-cache is not broken; the rename broke it

`Swatinem/rust-cache` is worth 6× (2058 s → ~320 s for the whole job) and it very
nearly got deleted from `ci.yml` on the evidence of run 302 alone, where the
restore took one second and a Bevy-free crate took 161 s to test. That run was
cold for a reason that had nothing to do with the cache being unhealthy:

**A cache entry's version hash covers the workspace path.** Renaming the repo
`viberfox` → `cartopolis` changed `/workspace/jeroen/viberfox` to
`/workspace/jeroen/cartopolis` and invalidated every entry in the store at a
stroke — 32.8 GB of them, orphaned to a repo name that no longer resolves. Run 302
was the first run afterwards, so it was cold and wrote a fresh cache; run 304
restored it and was 6× faster.

**Never judge a cache from one run.** Compare two consecutive runs on the same ref.

The second structural limit is worth knowing because it does not go away: entries
carry a `writeIsolationKey` of the ref that wrote them, so a cache written by a PR
run is invisible to every other branch. Caches written on `main` have an empty
isolation key and are readable everywhere, which is why `main` moving on every
merge is what keeps PR runs fast.

You can read the whole store, which is how the 32.8 GB was identified and deleted
without touching the eight live entries:

```bash
sudo strings /var/lib/forgejo-runner/.cache/actcache/bolt.db \
  | grep -o '{[^{]*"key":[^}]*}'
```

Blobs live at `cache/<hex(id)>/<id>` — note the directory is hexadecimal and the
filename is decimal.

## sccache sits underneath, for the cases rust-cache cannot serve

A fresh branch, or the whole repo the day after a rename. sccache is keyed on the
compiler invocation with no ref scope and no workspace path, so those cost a link
rather than a full Bevy build. On a rust-cache hit it is nearly idle — run 310
left only 39 MB in it, because almost nothing needed compiling.

`CARGO_INCREMENTAL: "0"` stops being a tuning and becomes a requirement: sccache
silently declines to cache an incremental compilation.

## `valid_volumes` matches the host path as a pattern, and fails silently

This cost a full run. `/data/ci-cache` in the runner's `valid_volumes` does **not**
permit a mount of `/data/ci-cache/sccache` — the entry has to name the path
exactly (or glob it). When it does not match, the runner drops the mount and runs
the job anyway: no error, no warning, a green build, and an empty cache directory.
Run 309's improvement was real but came entirely from the single invocation, mold
and the job cap, because the sccache it was supposedly measuring was never mounted.

**Verify a mount by looking at the host directory, not by reading the workflow.**
`sccache --show-stats` runs in both jobs (`if: always()`) for the same reason.

## Rejected, with the measurement that rejected it

- **Baking the Android NDK into `cartopolis-ci:latest`.** Its `actions/cache`
  restore measures **7 seconds**. The image would grow ~2 GB to save that, on a
  host disk that was at 87%. ALSA headers and the `wasm32`/`aarch64-linux-android`
  stds *are* baked in — those are a per-run apt install and a per-run ~100 MB
  toolchain download respectively.
- **Dropping the dependency `opt-level` for CI.** `profile.dev.package."*"` builds
  dependencies at `opt-level = 3`, which is most of a cold build. Lowering it in
  CI would change every fingerprint (one guaranteed cold run), slow the headless
  scene smoke — which renders on a CPU rasteriser — and buy nothing on a warm run,
  which is the common case.
- **A shared `CARGO_TARGET_DIR` across jobs.** Cargo takes a blocking lock on the
  target directory, so two jobs pointed at one would serialise a runner whose
  entire capacity is 2. sccache is concurrency-safe and gets the same sharing.

## Host disk: where it goes and what is safe to reclaim

The runner shares an 8-core, 193 GB box with nominatim, overpass, Forgejo and the
agent containers. It reached 87% full on 2026-08-12 and came back to 66 GB free
without touching anything that costs build time:

| reclaimed | what | why it was free |
|-----------|------|-----------------|
| 33 GB | actcache entries scoped to `jeroen/viberfox` | the repo does not exist under that name; unreachable forever |
| 30 GB | `agent_cargo-target/_data/debug/incremental` | pure incremental cache — but it regrows, and it *does* save agent rebuild time |
| ~2 GB | dangling images, one orphaned build container | leftovers |

Not reclaimable: the ~96 GB of docker volumes is mostly `geo_overpass_db` and
nominatim's database. That is the map services' data, not cache.

`CARGO_BUILD_JOBS: "3"` exists for the same shared-host reason: capacity 2 × one
rustc per core is 16 compilers on 8 cores, and the load average sat between 8.5
and 11.8 during an ordinary run, with two public services on the same silicon.

## Still open

- **`cargo nextest`.** Warm, compilation is largely cached and test *execution*
  becomes the dominant term, which is what nextest parallelises. Measure the
  compile-versus-run split first.
- **No `rust-toolchain.toml`.** The compiler is whatever `stable` was current when
  `cartopolis-ci:latest` was last built. A bump also invalidates rust-cache, which
  keys on the rustc version, so it costs one cold run whenever it happens —
  silently, and at a time nobody chose.
- **Forgejo is not flushing job logs to storage** (`log_in_storage = 0`, nothing at
  the recorded `log_filename`), so step timings from SQLite are the only
  machine-readable record and `sccache --show-stats` can currently be read only
  through the web UI.
