---
topic: ci-build-cost
triggers: [ci.yml, rust-cache, sccache, valid_volumes, actcache, CARGO_BUILD_JOBS, mold, "ci slow", "cache miss", "cargo test workspace", forgejo-runner, "disk full", cartopolis-ci, nextest, "queue blocked", paths-ignore, save-if, "runner capacity", "agents ci"]
updated: 2026-08-16
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

## Where a warm run's time actually goes, 2026-08-16

One stored job log (task 640, PR #92, 448 s) answers this completely, because
`cargo` and `libtest` both print their own timings:

| phase | time |
|-------|------|
| `rust-cache` restore, 1182 MB, full match | ~70 s |
| compiling the five workspace crates | **6 m 02 s** |
| running all 764 tests | **2.2 s** |
| `rust-cache` save (post step) | ~90 s |

**Test execution is half a percent of the job.** Everything else is compiling our
own code — `sccache` served four of the five crates from cache and missed on
`cartopolis`, which is the whole six minutes. So the cost of a CI run is a
function of *which crate you touched*, not of how many tests exist: touch the
client and pay six minutes, touch `geo` alone and the client comes back from
sccache.

That kills the open question below it: **`cargo nextest` is not worth measuring.**
It parallelises test execution, and there are 2.2 seconds of that to win.

The ~90 s post step is this action writing its archive, and it is measurable
without reading a log — subtract the sum of a job's steps from the job's own
duration. Averaged over every green run since 2026-08-05: PR `test` 518 s of job
against 429 s of steps, `main` `test` 675 against 593, `targets` 309 against 217.

## Per merged change, the runner does the work three times

A merge costs a PR `test` job, a `main` `test` job and a `targets` job — about
1500 runner-seconds on a runner with three slots. With several agents holding
branches open at once, that queue is the thing that blocks, and four of the runs
in it are avoidable rather than slow. `ci.yml` now declines them, and each
mechanism is one `if` with its reasoning beside it:

- **A tree already tested is not tested again.** A merge commit's `HEAD^2` is the
  branch head the PR lane compiled; when the branch was rebased before merging
  the two trees are bit-identical, and the merge lane skips `cargo test`. Needs
  `fetch-depth: 2` — a depth-1 clone cannot name a parent.
- **Markdown does not compile Bevy.** `paths-ignore` on `docs/**` and `**/*.md`.
  Eight of the forty commits on `main` before 2026-08-16 were docs-only, and each
  one bought two full runs.
- **Forgejo's skip markers are the per-push lever**, and they need no workflow
  support: `SKIP_WORKFLOW_STRINGS` is on by default. **A draft gate is not
  available** — see below.
- **The scene smokes run only when something could have changed a frame** — a
  deny-list, so an unrecognised path still renders.
- **PR runs restore the cache but never save it.** A PR-written entry carries a
  `writeIsolationKey` of its own ref and the workspace crates are pruned out
  before writing, so it costs 90 s to store dependencies `main`'s entry already
  has, on a disk that was at 82%.
- **`targets` runs `needs: test`** rather than beside it. A `main` run used to hold
  two of three slots for its whole length; one `targets` job queued 967 s behind
  it. Nobody waits on a `main` verdict and somebody always waits on a PR.

**Skipping CI on draft pull requests cannot work here, measured 2026-08-16.**
Forgejo 16.0.1 fires no Actions run at all on the draft→ready transition. On
PR #94, with `ready_for_review` declared in `types`, removing the title's `WIP:`
prefix moved the API's `draft` to `false` and created no run row. Forgejo derives
draft state from that title prefix, so the transition arrives as an `edited`
event or as nothing — and putting `edited` in `types` would rebuild the world
every time somebody fixed a typo in a description. A gate whose release
condition never fires is a branch that silently never gets a verdict, so the
gate was removed rather than shipped with a caveat.

Both of the trigger types that *are* used were confirmed on the same PR: `opened`
created run 398, and a push created run 400 as `pull_request_sync`, matching a
plain `on: pull_request`.

**The skip markers do work on a pull-request push**, confirmed on the same PR by
accident, which is the useful part: the next force-push created no run at all,
and the only difference was a commit *body* that mentioned the marker while
explaining that it was the lever to use. `SKIP_WORKFLOW_STRINGS` is matched as a
plain substring over the whole commit message — not a prefix, not a line of its
own — so **a commit that talks about skipping CI skips CI**. Split the brackets
when writing about them.

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

The third key input is the one that will catch you, because it is not about the
code at all: **rust-cache hashes every environment variable whose name begins
with `CARGO`, `CC`, `CFLAGS`, `CXX`, `CMAKE` or `RUST`.** Changing
`CARGO_BUILD_JOBS` from 3 to 2 — a scheduling knob that changes not one byte of
any artifact — invalidated the entire store on 2026-08-16: the run that did it
restored in **1 second** instead of ~70 and rebuilt Bevy from nothing.

That is survivable once. What made it worth a rule is the second half: the merge
that would have re-saved a warm entry under the new key touched only `docs/` and
`.forgejo/`, so the tree-already-tested and render-path gates correctly skipped
every step that compiles, and the cache would have stayed cold for every pull
request until some later merge happened to touch the client.

So the job count lives on the cargo command line as `-j "$JOBS"` and the `env:`
block holds only variables that really are compilation inputs. **Anything in
`env:` that starts with one of those prefixes is a cold run for the whole repo
the day someone tunes it** — and *removing* one costs exactly what adding it did,
which the run that did it reported as a flat `No cache found`.

The repair has to be deliberate, because the gates above can produce a `main` run
that compiles nothing and therefore saves nothing: after any key change, dispatch
`ci.yml` on `main` once (`workflow_dispatch`, added for this) and let it run
whole. Otherwise every pull request stays cold until some later merge happens to
touch the client — which is a slow, silent, and very confusing way to lose a
cache.

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
  target directory, so two jobs pointed at one would serialise the runner's
  slots against each other. sccache is concurrency-safe and gets the same sharing.
- **Testing only the packages a PR touched.** Tempting, because the six-minute
  term is one crate — but resolver v2 unifies features over the packages a single
  invocation *selects*, so any subset re-unifies the shared Bevy dependencies and
  rebuilds them. That is the 45% regression run 302 already measured, arriving by
  a different door. One `--workspace` invocation, always.
- **`cargo nextest`** — see the measurement above. 2.2 seconds of test execution.

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

The job count exists for the same shared-host reason: one rustc per core per
slot is 24 compilers on 8 cores, and the load average sat between 8.5 and 11.8
during an ordinary run — 12.2 on 2026-08-16, with two public services *and* the
agent containers' own builds on the same silicon.

**A cold job got slower and the queue still got faster.** The first cold run at
`-j 2` took 2671 s against 1918-2058 s for the cold runs recorded at `-j 3`
above — call it a third slower per job, though it is not a controlled A/B (a
different day, a different load, and the runner shares the box). Three slots at
that rate is still ~15% more throughput than two at the old one, and throughput
was never the point: what improves is that a pull request now finds a free slot
while a `main` run holds one. If a *single* job's wall clock ever matters more
than the queue does, this is the knob that bought the queue and it can be sold
back.

**Slots and compilers are one budget, and it is six.** The runner went to
capacity 3 on 2026-08-16 and the per-job count went to 2 in the same change, so
the peak compiler count did not move. The third slot buys the overlap of the
third of a job that compiles nothing — checkout, the cache restore and save, the
scene smoke's settle — and buys an agent's pull request a slot while `main` has
one. Raising it without lowering the other is what the load average forbids.

The memory ceiling is the harder one and it has no swap behind it: 23 GB total,
of which the emulator container holds up to 12 and the agent containers 12 more.
Six OOM kills are in the kernel log for the 30 days to 2026-08-16, twice with
`qemu-system-x86` invoking the killer. A CI job container measured 0.4 GB through
checkout and cache restore and peaks around 2 GB linking with mold, so three of
them is ~6 GB — which fits only while the Android emulator is *not* left running.
Tear it down when a session ends (`android-emulator-on-the-vps`).

## Still open

- **Workspace crates at `opt-level = 0` for the test lane.** `profile.dev` builds
  our own code at 1, and the six-minute term above is its codegen.
  `CARGO_PROFILE_TEST_OPT_LEVEL=0` touches only `cargo test`, but the scene smoke
  in the same job is a `cargo run` on the *dev* profile, so it would compile the
  crate a second time unless it is moved to `--profile test` in the same change.
  Costs one cold run to measure (any `CARGO_*` variable is hashed into the
  rust-cache key) and must be set for both lanes or they stop sharing a cache.
- **No `rust-toolchain.toml`.** The compiler is whatever `stable` was current when
  `cartopolis-ci:latest` was last built. A bump also invalidates rust-cache, which
  keys on the rustc version, so it costs one cold run whenever it happens —
  silently, and at a time nobody chose.
- **Forgejo is not flushing job logs to storage** (`log_in_storage = 0`, nothing at
  the recorded `log_filename`), so step timings from SQLite are the only
  machine-readable record and `sccache --show-stats` can currently be read only
  through the web UI.
