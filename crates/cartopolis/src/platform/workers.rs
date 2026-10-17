//! Real CPU parallelism (ADR-033) — the seam that makes tile processing use more
//! than one core *in the browser*.
//!
//! [`work::Dispatcher`](super::work) answers "how many jobs may start this frame".
//! This module answers the different question of "on which core does the job
//! actually run", and the two compose: the dispatcher gates admission, this pool
//! provides the muscle.
//!
//! **Why a pool of our own.** `bevy_tasks` 0.19 disables its multi-threaded pool on
//! wasm unconditionally — `#[cfg(all(not(target_arch = "wasm32"), feature =
//! "multi_threaded"))]` (`bevy_tasks/src/lib.rs:21`) — so on the web every
//! `IoTaskPool::spawn` lands on `wasm_bindgen_futures::spawn_local`, i.e. the main
//! thread, interleaved with rendering. Awaiting a `fetch` there is free (the
//! browser does that work elsewhere), but decoding a vector tile is not: 400-odd
//! tiles of MVT parse + rasterise + mip-chain ran *between animation frames* and
//! the map filled in at a crawl. Threads are the fix, and WebAssembly does have
//! them — shared linear memory plus `atomics`, behind cross-origin isolation.
//!
//! **Only CPU work belongs here.** The network half stays on the main thread: it is
//! already concurrent (the browser runs the requests), and `fetch` is reached
//! through `web_sys` bindings that a worker would need re-bound. So the tile path
//! splits — fetch on the main thread, [`submit`] the decode.
//!
//! **Native builds have real threads but not free ones.** [`submit`] used to
//! forward to `AsyncComputeTaskPool`, on the reasoning that native already had a
//! thread pool so there was nothing to add. That was right about the threads and
//! wrong about the neighbours: Bevy spawns **pipeline compilation** and the
//! screenshot readback onto that same pool, and our jobs are blocking rasterises
//! with no yield point, so a shader compile could sit behind hundreds of them and
//! the geometry waiting on it was silently skipped. So this owns a `TaskPool` of
//! its own on every target now, and the two halves differ only in how a thread is
//! created. See `imp::POOL`.

/// A unit of CPU-bound work. `Send` because it crosses to another core/worker.
pub type Job = Box<dyn FnOnce() + Send + 'static>;

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use super::Job;
    use bevy::tasks::{TaskPool, TaskPoolBuilder};
    use std::sync::OnceLock;

    /// Our own threads, deliberately **not** `AsyncComputeTaskPool`.
    ///
    /// It used to be that pool, on the reasoning that native already has real
    /// threads so there was nothing to add. That was true about the threads and
    /// wrong about who else is on them, and the cost lands on the platform least
    /// able to absorb it.
    ///
    /// Bevy spawns **pipeline compilation** onto `AsyncComputeTaskPool` — on
    /// `not(wasm32)`, `not(macos)`, `multi_threaded`, i.e. exactly Android
    /// (`bevy_render`'s `pipeline_cache::create_pipeline_task`). Our jobs are
    /// blocking MVT rasterises, ~120 ms each with no yield point, so each one owns
    /// a pool thread outright; the phone tier caps that pool at 3, and a teleport
    /// queues hundreds of jobs while the streamers keep refilling it. A shader
    /// compile handed to that queue waits behind all of them.
    ///
    /// The symptom is not a stall, which is what makes it hard to place: geometry
    /// whose pipeline is not resident yet is **silently skipped** by every pass
    /// node, so the first seconds after launch and after every teleport are a
    /// stuttering, half-drawn map, and each pipeline arrives as a hitch rather
    /// than a frame. Adreno compiles run 50–300 ms apiece and nothing prewarms.
    ///
    /// This is the same starvation `set_paused` was added for — Bevy's screenshot
    /// readback is detached onto that pool too — and
    /// `docs/notes/headless-shots-software-renderer.md` records the general rule
    /// that anything spawned there inherits it. A pool of our own is the fix
    /// for the class rather than for each victim: `set_paused` remains, because it
    /// answers a different question (quiescing *our* work before the shutter), but
    /// it is no longer the only thing standing between a tile burst and every other
    /// async consumer in the engine.
    static POOL: OnceLock<TaskPool> = OnceLock::new();

    /// Build the pool at `workers` wide.
    ///
    /// Called from `run()` before the app is built. Idempotent: a second call is a
    /// no-op rather than a panic, so a test harness that starts twice is fine.
    pub fn init(workers: usize) {
        let _ = POOL.get_or_init(|| {
            let threads = workers.max(1);
            let mut builder = TaskPoolBuilder::new()
                .num_threads(threads)
                .thread_name("carto tile worker".to_string());
            // Below the render and main threads on Android, because otherwise the
            // scheduler ranks them above it: these threads are CPU-saturated for
            // 120 ms at a stretch so EAS reads them as the important ones, while
            // the render thread blocks in FIFO acquire and reads as idle. On a
            // 2-big/6-little SoC that hands the streamers the big cores and the
            // frame an A55.
            //
            // Be honest about the ceiling: this is a CFS weight only. Java's
            // `Process.setThreadPriority` would *also* move the thread into the
            // background cpuset, and that half needs a `Runnable`, i.e. a
            // `classes.dex` (`docs/notes/android-build.md`). Pinning to the little
            // cluster with `sched_setaffinity` is the stronger libc-only version
            // and is deliberately not taken: it would need the cluster derived from
            // `cpuinfo_max_freq` rather than a hardcoded core layout, and starving
            // the streamers outright is a worse failure than deprioritising them.
            if cfg!(target_os = "android") {
                builder = builder.on_thread_spawn(|| {
                    #[cfg(target_os = "android")]
                    // SAFETY: `setpriority` takes no pointers and cannot fail in a
                    // way that matters — `PRIO_PROCESS` with `who == 0` is the
                    // *calling thread* on Linux, and lowering a priority never
                    // requires privilege.
                    unsafe {
                        libc::setpriority(libc::PRIO_PROCESS, 0, 10);
                    }
                });
            }
            builder.build()
        });
    }

    fn pool() -> &'static TaskPool {
        // `init` runs at the top of `run()`, but a unit test can reach a streamer
        // without one. Falling back to a single thread keeps that path working
        // instead of panicking on a `OnceLock` nobody set.
        POOL.get_or_init(|| {
            TaskPoolBuilder::new()
                .num_threads(1)
                .thread_name("carto tile worker (unsized)".to_string())
                .build()
        })
    }

    pub fn submit(job: Job) {
        pool().spawn(async move { job() }).detach();
    }

    pub fn worker_count() -> usize {
        pool().thread_num()
    }
}

#[cfg(target_arch = "wasm32")]
mod imp {
    use super::Job;
    use crate::platform::sync::{Arc, Condvar, Mutex, OnceLock};
    use std::collections::VecDeque;

    struct Pool {
        queue: Mutex<VecDeque<Job>>,
        /// Workers park here rather than spinning. `Condvar::wait` lowers to
        /// `memory.atomic.wait32`, which traps on the main thread but is exactly
        /// what a worker is allowed to do — one more reason the pool is
        /// worker-only and the main thread never blocks on it.
        idle: Condvar,
        workers: usize,
    }

    static POOL: OnceLock<Arc<Pool>> = OnceLock::new();

    /// Spawn `workers` Web Workers, each looping on the shared queue.
    ///
    /// `std::thread::spawn` is `Unsupported` on `wasm32-unknown-unknown` even with
    /// atomics — the platform has no way to *create* a thread, only to share memory
    /// with one. Creating the worker is a JS act, so `wasm_thread` does it: it
    /// starts a Worker that re-instantiates this same module against the *imported*
    /// (shared) memory, then runs the closure.
    pub fn init(workers: usize) {
        let workers = workers.clamp(1, 8);
        let pool = Arc::new(Pool {
            queue: Mutex::new(VecDeque::new()),
            idle: Condvar::new(),
            workers,
        });
        for i in 0..workers {
            let pool = pool.clone();
            let spawned = wasm_thread::Builder::new()
                .name(format!("carto-cpu-{i}"))
                .spawn(move || {
                    // A worker may park; the main thread must not (`platform::sync`).
                    crate::platform::sync::mark_worker();
                    loop {
                        let job = {
                            let mut q = pool.queue.lock().unwrap_or_else(|e| e.into_inner());
                            loop {
                                match q.pop_front() {
                                    Some(job) => break job,
                                    None => {
                                        q = pool.idle.wait(q).unwrap_or_else(|e| e.into_inner());
                                    }
                                }
                            }
                        };
                        job();
                    }
                });
            if let Err(e) = spawned {
                tracing::warn!(worker = i, error = ?e, "could not start CPU worker");
            }
        }
        let _ = POOL.set(pool);
        tracing::info!(workers, "CPU worker pool started");
    }

    pub fn submit(job: Job) {
        match POOL.get() {
            Some(pool) => {
                pool.queue
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push_back(job);
                pool.idle.notify_one();
            }
            // No pool (init failed, or a build without cross-origin isolation):
            // run it here. Slow, but the map still fills — degradation, not a hang.
            None => job(),
        }
    }

    pub fn worker_count() -> usize {
        POOL.get().map(|p| p.workers).unwrap_or(0)
    }
}

pub use imp::init;
/// Pool width, for diagnostics/HUD. Safe to call at any point now that the pool is
/// ours — it no longer reads a Bevy global that has to exist first.
#[allow(unused_imports, dead_code)]
pub use imp::worker_count;

use crate::platform::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// While set, [`submit`] parks jobs in [`PARKED`] instead of dispatching them.
static PAUSED: AtomicBool = AtomicBool::new(false);
/// Jobs submitted while paused, released in order by `set_paused(false)`.
static PARKED: Mutex<Vec<Queued>> = Mutex::new(Vec::new());

/// How soon a job wants a thread. **Lower runs first**, and it is only a
/// *tie-break between jobs that are waiting*: a running job is never preempted.
///
/// This exists because the pool behind [`submit`] is first come, first served.
/// The streamers already order their own work carefully (coarse tiles first,
/// nearest first, `fetch_gate`'s near reserve), and that order used to end at the
/// moment a job was handed to the pool, where a batch of far building cells or a
/// grass bake sat in front of a basemap tile the camera was standing on.
/// [`Urgency`] carries the order through the last stage.
pub type Urgency = u8;

/// The ground the camera sees: the clipmap's raster tiles. Nothing is drawn under
/// them, so a missing one is a hole rather than a late detail.
pub const BASEMAP: Urgency = 0;
/// Work for the cell the camera is over and its neighbours.
pub const NEAR: Urgency = 1;
/// The default: a job that has nothing distance-shaped to say.
pub const NORMAL: Urgency = 2;
/// Work for ground the camera is not looking at or is far from.
pub const FAR: Urgency = 3;

/// The urgency of a cell `rank` rings from the camera's (`fetch_gate::Rank`).
#[must_use]
pub fn urgency_for_ring(rank: super::fetch_gate::Rank) -> Urgency {
    match rank {
        0..=1 => NEAR,
        2..=3 => NORMAL,
        _ => FAR,
    }
}

/// Submissions after which a waiting job is treated as one class more urgent.
///
/// Counted in *submissions*, not seconds, because `std::time::Instant` panics on
/// wasm and a streaming burst is exactly the case where the two agree. Without
/// it a stream of basemap tiles could keep a detail job waiting for the whole of a
/// long fly-over; with it the worst case is `FAR` overtaking `BASEMAP` after
/// `3 * AGING_JOBS` newer submissions.
const AGING_JOBS: u64 = 48;

struct Queued {
    label: &'static str,
    heavy: bool,
    urgency: Urgency,
    job: Job,
}

struct Waiting {
    label: &'static str,
    heavy: bool,
    urgency: Urgency,
    seq: u64,
    queued: Instant,
    job: Job,
}

/// Every job the pool owes and has not yet handed a thread, and the count of
/// those it has. One lock for all of it, so a finishing job's "start the next one"
/// and a new submission's "is there room" cannot both see the same free thread.
struct Scheduler {
    running: usize,
    /// Of `running`, the [heavy](submit_heavy_ranked) ones.
    heavy_running: usize,
    next_seq: u64,
    waiting: Vec<Waiting>,
}

static SCHED: Mutex<Scheduler> = Mutex::new(Scheduler {
    running: 0,
    heavy_running: 0,
    next_seq: 0,
    waiting: Vec::new(),
});

#[cfg(not(target_arch = "wasm32"))]
type Instant = std::time::Instant;
/// `std::time::Instant::now` panics on wasm, and nothing there reads the wait.
#[cfg(target_arch = "wasm32")]
type Instant = ();

#[cfg(not(target_arch = "wasm32"))]
fn now() -> Instant {
    std::time::Instant::now()
}
#[cfg(target_arch = "wasm32")]
fn now() -> Instant {}

/// How many threads heavy jobs may hold at once: all but a reserve, which is what
/// keeps a short job from ever queueing behind them.
#[must_use]
pub fn heavy_lane_width(workers: usize) -> usize {
    let reserve = if workers >= 6 { 2 } else { 1 };
    workers.saturating_sub(reserve).max(1)
}

/// The index of the waiting job that should take a free thread, if any may.
///
/// Most urgent first, oldest first among equals, with [`AGING_JOBS`] of waiting
/// worth one class. A heavy job is passed over while the heavy lane is full, which
/// is what keeps the reserve for short jobs. Pure over its arguments so the order
/// can be asserted without a pool.
fn pick(waiting: &[Waiting], next_seq: u64, heavy_room: bool) -> Option<usize> {
    waiting
        .iter()
        .enumerate()
        .filter(|(_, w)| heavy_room || !w.heavy)
        .min_by_key(|(_, w)| {
            let aged = u8::try_from((next_seq - w.seq) / AGING_JOBS).unwrap_or(u8::MAX);
            (w.urgency.saturating_sub(aged), w.seq)
        })
        .map(|(i, _)| i)
}

/// Jobs handed to the pool and not yet finished — queued *or* running. Read by
/// [`outstanding`].
static OUTSTANDING: AtomicUsize = AtomicUsize::new(0);

/// Per-stream accounting of what the pool has done — the answer to "where did
/// the streaming time go" that `outstanding()` (a single instantaneous depth)
/// cannot give.
///
/// Three numbers per label, cumulative since launch: jobs finished, time spent
/// running them, and time they waited between [`submit`] and their first
/// instruction. Busy says which decode dominates; **wait** is the queue-shaped
/// number — it grows with pool starvation, and a teleport burst shows up here
/// as seconds of wait against milliseconds of busy. Consumers diff two
/// [`snapshot`](stats::snapshot)s, which is how the shot harness turns these
/// into per-capture columns.
///
/// Wall-clock, so machine-dependent — a paired A/B on one box, never a CI gate
/// (`crates/geo`'s `perf.rs` doc has the doctrine). On wasm only `jobs` is
/// counted: `std::time::Instant` panics there, and the harness that reads the
/// durations is native.
pub mod stats {
    use crate::platform::sync::Mutex;

    /// Cumulative cost of one labelled stream's jobs.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Totals {
        /// Jobs finished. A job that panics is not counted here (the pool's
        /// `outstanding` guard still releases it) — a stats row is not worth
        /// running user-visible work inside a drop guard.
        pub jobs: u64,
        /// Microseconds spent inside the jobs.
        pub busy_us: u64,
        /// Microseconds the jobs sat queued between `submit` and starting.
        pub wait_us: u64,
    }

    impl Totals {
        /// The delta `self − earlier`, for turning two snapshots into a window.
        #[must_use]
        pub fn since(&self, earlier: Totals) -> Totals {
            Totals {
                jobs: self.jobs - earlier.jobs,
                busy_us: self.busy_us - earlier.busy_us,
                wait_us: self.wait_us - earlier.wait_us,
            }
        }
    }

    /// A `Vec` with a linear scan, not a map: there are ~a dozen labels ever,
    /// they are `&'static`, and `Vec::new` is const so the whole thing needs no
    /// lazy init inside a hot path.
    static TOTALS: Mutex<Vec<(&'static str, Totals)>> = Mutex::new(Vec::new());

    pub(super) fn record(label: &'static str, wait_us: u64, busy_us: u64) {
        let mut totals = TOTALS.lock().unwrap_or_else(|e| e.into_inner());
        let idx = match totals.iter().position(|(l, _)| *l == label) {
            Some(idx) => idx,
            None => {
                totals.push((label, Totals::default()));
                totals.len() - 1
            }
        };
        let entry = &mut totals[idx].1;
        entry.jobs += 1;
        entry.busy_us += busy_us;
        entry.wait_us += wait_us;
    }

    /// Every label's cumulative totals, in first-seen order.
    #[must_use]
    pub fn snapshot() -> Vec<(&'static str, Totals)> {
        TOTALS.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// Hand one CPU-bound job to the pool (or hold it, while paused — see
/// [`set_paused`]) at [`NORMAL`] urgency.
///
/// `label` names the stream the job belongs to for [`stats`] — use the same
/// name the shot harness's settle list uses for that streamer, so a `busy`
/// entry and a stats row about one stream read as one stream.
pub fn submit(label: &'static str, job: Job) {
    enter(label, false, NORMAL, job);
}

/// [`submit`], saying how soon the job wants a thread. Jobs that are waiting for
/// one start most-urgent first (see [`Urgency`]); a job that is already running is
/// never interrupted.
pub fn submit_ranked(label: &'static str, urgency: Urgency, job: Job) {
    enter(label, false, urgency, job);
}

/// [`submit_ranked`] for a job that holds its thread for **seconds** — a whole map
/// cell, a building cell, a terrain bake.
///
/// **A running job cannot be preempted**, so without this a burst of heavy jobs
/// takes every thread and a 50 ms basemap tile waits behind them. Measured cold
/// over Groningen on `deck-1` (2026-09-23): 367 of 375 basemap tiles made no
/// progress for eight seconds while seven threads held map cells, and the basemap
/// — the part of the world that should arrive like an ordinary image tile map —
/// took 45 s to finish.
///
/// Heavy jobs therefore share at most [`heavy_lane_width`] threads and wait for
/// the rest. This guarantees ordinary jobs a thread; *which* waiting job gets the
/// next free one is [`Urgency`]'s business. What it costs is heavy throughput
/// while the reserve sits idle, which is the right way round: a cell that lands a
/// little later is hidden by the basemap under it, and a missing basemap is not
/// hidden by anything.
pub fn submit_heavy_ranked(label: &'static str, urgency: Urgency, job: Job) {
    enter(label, true, urgency, job);
}

/// Run `work` where it will not hold up a frame, and await its answer.
///
/// For CPU work inside a future on the `IoTaskPool` — a parse after a fetch, the
/// usual shape. **On the web that pool is the page's main thread**, the one that
/// also draws every frame and runs every future between them, so a 2 MB JSON
/// parse there is a frame lost and every other in-flight job's next step held
/// back with it (measured over Groningen, 2026-10-13: a basemap tile spent 5.6 s
/// getting its payload and 0.4 s being drawn). There the work goes to the pool at
/// [`NORMAL`] urgency. **Natively the io pool is sixteen real threads**, so the
/// work runs in place, as it always did.
///
/// `None` only if the worker dropped the job without answering — a panic in
/// `work` — which callers treat as the miss they would have had for bad input.
pub async fn compute<T: Send + 'static>(
    label: &'static str,
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    #[cfg(target_arch = "wasm32")]
    {
        let (tx, rx) = futures_channel::oneshot::channel();
        submit(
            label,
            Box::new(move || {
                let _ = tx.send(work());
            }),
        );
        rx.await.ok()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = label;
        Some(work())
    }
}

fn enter(label: &'static str, heavy: bool, urgency: Urgency, job: Job) {
    // Acquire/Release against `set_paused`: a job admitted here must be visible
    // to the drain, and a job parked here must be visible to whoever unparks.
    if PAUSED.load(Ordering::Acquire) {
        PARKED
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Queued {
                label,
                heavy,
                urgency,
                job,
            });
        return;
    }
    // Counted from here, not from when the job reaches a thread: one waiting is
    // still work the pool owes, and `outstanding()` is what the shot harness
    // quiesces on.
    OUTSTANDING.fetch_add(1, Ordering::AcqRel);
    {
        let mut sched = SCHED.lock().unwrap_or_else(|e| e.into_inner());
        let seq = sched.next_seq;
        sched.next_seq += 1;
        sched.waiting.push(Waiting {
            label,
            heavy,
            urgency,
            seq,
            queued: now(),
            job,
        });
    }
    pump();
}

/// Start waiting jobs on every free thread.
///
/// **The pool never holds more than it can run.** It is first come, first served
/// and cannot be reordered from outside, so anything queued *inside* it is
/// scheduled by arrival order whatever the caller wanted. Capping what we hand it
/// at its width keeps the queue here, where [`pick`] can choose.
fn pump() {
    let workers = imp::worker_count().max(1);
    loop {
        let next = {
            let mut sched = SCHED.lock().unwrap_or_else(|e| e.into_inner());
            if sched.running >= workers {
                return;
            }
            let heavy_room = sched.heavy_running < heavy_lane_width(workers);
            let Some(i) = pick(&sched.waiting, sched.next_seq, heavy_room) else {
                return;
            };
            let next = sched.waiting.swap_remove(i);
            sched.running += 1;
            if next.heavy {
                sched.heavy_running += 1;
            }
            next
        };
        dispatch(next);
    }
}

fn dispatch(w: Waiting) {
    let Waiting {
        label,
        heavy,
        queued,
        job,
        ..
    } = w;
    #[cfg(target_arch = "wasm32")]
    let () = queued;
    imp::submit(Box::new(move || {
        // A guard rather than a decrement after `job()`: a panicking job would
        // otherwise leak a count forever and `outstanding()` would never reach
        // zero again, which is precisely the condition a caller waits on.
        struct Done(bool);
        impl Drop for Done {
            fn drop(&mut self) {
                // The thread and the lane slot go back, and the next job starts,
                // *before* the count drops: a harness that sees `outstanding()`
                // reach zero must not find a job still waiting for this thread.
                // And in the guard for the same reason the count is — a
                // panicking job must still give its thread back.
                {
                    let mut sched = SCHED.lock().unwrap_or_else(|e| e.into_inner());
                    sched.running = sched.running.saturating_sub(1);
                    if self.0 {
                        sched.heavy_running = sched.heavy_running.saturating_sub(1);
                    }
                }
                pump();
                OUTSTANDING.fetch_sub(1, Ordering::AcqRel);
            }
        }
        let _done = Done(heavy);
        #[cfg(not(target_arch = "wasm32"))]
        {
            let started = std::time::Instant::now();
            let wait = started.duration_since(queued);
            job();
            stats::record(
                label,
                wait.as_micros() as u64,
                started.elapsed().as_micros() as u64,
            );
        }
        #[cfg(target_arch = "wasm32")]
        {
            job();
            stats::record(label, 0, 0);
        }
    }));
}

/// Jobs of ours the pool still owes an answer for — queued or running. Zero means
/// the pool holds nothing of ours, which is the only state in which something else
/// spawned onto it is guaranteed a thread promptly.
pub fn outstanding() -> usize {
    OUTSTANDING.load(Ordering::Acquire)
}

/// Stop (or resume) admitting jobs to the pool.
///
/// **Why this exists.** It was written when [`submit`] *was* `AsyncComputeTaskPool`
/// on native — and so is Bevy's screenshot readback, which `collect_screenshots`
/// spawns and detaches onto that pool. Our jobs are blocking closures (a full MVT
/// rasterise, ~120 ms each — `docs/notes/tile-raster-cost.md`), so each one owns a
/// thread outright with no yield point. On an 8-core host the pool is 3 threads
/// wide, a teleport queues hundreds of jobs, and the streamers keep refilling it:
/// the readback future was FIFO'd behind all of them and might never be polled.
/// That is a `--shot` that renders 194 healthy frames and never writes a PNG
/// (`docs/notes/headless-shots-software-renderer.md`).
///
/// The pool split (see the module header) removed that particular collision, and
/// this is **still needed**, for a reason the split does not address: the shot
/// harness has to know our own work is *finished*, not merely running elsewhere.
/// A tile decode that lands between the settle verdict and the shutter changes the
/// picture being captured. So this quiesces our jobs deliberately, where the split
/// merely stops them blocking somebody else's.
///
/// Pausing is deliberately *parking*, not dropping. The streamers de-duplicate on
/// "a fetch is in flight", so a discarded decode is never re-queued — the tile would
/// be missing for the rest of the run, which in a multi-shot `--shots` launch means
/// every later image too.
pub fn set_paused(paused: bool) {
    PAUSED.store(paused, Ordering::Release);
    if paused {
        return;
    }
    // Loop rather than drain once: `submit` may be reached from a job already
    // running on the pool, so a batch can arrive between the take and the store
    // being observed.
    loop {
        let batch = std::mem::take(&mut *PARKED.lock().unwrap_or_else(|e| e.into_inner()));
        if batch.is_empty() {
            break;
        }
        for q in batch {
            enter(q.label, q.heavy, q.urgency, q.job);
        }
    }
}

/// Start the pool. `hint` is the caller's idea of a good width (typically
/// `navigator.hardwareConcurrency - 1`, leaving the main thread a core to render on).
pub fn start(hint: usize) {
    init(hint);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    fn waiting(urgency: Urgency, seq: u64, heavy: bool) -> Waiting {
        Waiting {
            label: "pick_test",
            heavy,
            urgency,
            seq,
            queued: now(),
            job: Box::new(|| {}),
        }
    }

    /// The point of the queue: a basemap tile submitted *after* a pile of far work
    /// still gets the next thread, and equals keep arrival order.
    #[test]
    fn the_most_urgent_waiting_job_starts_first_and_equals_keep_their_order() {
        let queue = vec![
            waiting(FAR, 0, false),
            waiting(NORMAL, 1, false),
            waiting(BASEMAP, 5, false),
            waiting(BASEMAP, 3, false),
        ];
        assert_eq!(pick(&queue, 6, true), Some(3), "older basemap job first");
        assert_eq!(pick(&queue[..3], 6, true), Some(2));
        assert_eq!(pick(&[], 0, true), None);
    }

    /// Without ageing a steady supply of basemap tiles would hold a detail job
    /// back for as long as the camera keeps moving.
    #[test]
    fn a_job_that_has_waited_long_enough_overtakes_newer_urgent_ones() {
        let queue = vec![
            waiting(FAR, 0, false),
            waiting(BASEMAP, 3 * AGING_JOBS, false),
        ];
        assert_eq!(pick(&queue, 3 * AGING_JOBS + 1, true), Some(0));
        let fresh = vec![waiting(FAR, 0, false), waiting(BASEMAP, 1, false)];
        assert_eq!(pick(&fresh, 2, true), Some(1), "not yet aged");
    }

    /// A full heavy lane passes heavy jobs over rather than blocking the queue
    /// behind them, and urgency never lets one take the reserved thread.
    #[test]
    fn a_full_heavy_lane_skips_heavy_jobs_whatever_their_urgency() {
        let queue = vec![waiting(BASEMAP, 0, true), waiting(FAR, 1, false)];
        assert_eq!(pick(&queue, 2, false), Some(1));
        assert_eq!(pick(&queue, 2, true), Some(0));
        assert_eq!(pick(&queue[..1], 1, false), None);
    }

    #[test]
    fn a_cell_s_urgency_follows_its_ring() {
        assert_eq!(urgency_for_ring(0), NEAR);
        assert_eq!(urgency_for_ring(1), NEAR);
        assert_eq!(urgency_for_ring(3), NORMAL);
        assert_eq!(urgency_for_ring(4), FAR);
        assert_eq!(urgency_for_ring(crate::platform::fetch_gate::ANY), FAR);
    }

    /// The reserve exists on every pool that can spare a thread, and a pool of one
    /// still runs heavy work rather than none.
    #[test]
    fn the_heavy_lane_leaves_a_thread_for_everything_else() {
        assert_eq!(heavy_lane_width(1), 1);
        assert_eq!(heavy_lane_width(2), 1);
        assert_eq!(heavy_lane_width(3), 2);
        assert_eq!(heavy_lane_width(7), 5);
        for n in 2..=16 {
            assert!(heavy_lane_width(n) < n, "{n} threads left no reserve");
        }
    }

    /// More heavy jobs than the lane is wide must all still run: the ones that
    /// queue outside the pool are started by the jobs finishing ahead of them, and
    /// a slot that was never handed back would strand every one behind it.
    #[test]
    fn heavy_jobs_beyond_the_lane_all_run() {
        let (tx, rx) = mpsc::channel();
        let n = 3 * heavy_lane_width(imp::worker_count().max(1)) + 2;
        for i in 0..n {
            let tx = tx.clone();
            submit_heavy_ranked(
                "heavy_test",
                NORMAL,
                Box::new(move || {
                    std::thread::sleep(Duration::from_millis(2));
                    let _ = tx.send(i);
                }),
            );
        }
        let mut seen: Vec<usize> = (0..n)
            .map(|_| {
                rx.recv_timeout(Duration::from_secs(10))
                    .expect("heavy job ran")
            })
            .collect();
        seen.sort_unstable();
        assert_eq!(seen, (0..n).collect::<Vec<_>>());
    }

    /// The accounting is what the shot harness's `worker_*` columns are built
    /// on; a counter that silently stays zero would make every one of them read
    /// as "no streaming happened".
    #[test]
    fn a_finished_job_lands_in_its_label_s_totals() {
        let before = stats::snapshot()
            .into_iter()
            .find(|(l, _)| *l == "stats_test")
            .map_or_else(Default::default, |(_, t)| t);

        let (tx, rx) = mpsc::channel();
        submit(
            "stats_test",
            Box::new(move || {
                std::thread::sleep(Duration::from_millis(5));
                let _ = tx.send(());
            }),
        );
        rx.recv_timeout(Duration::from_secs(10)).expect("job runs");
        // The job signals before its closure returns; give `record` (which runs
        // after) a moment rather than asserting on a race.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let now = stats::snapshot()
                .into_iter()
                .find(|(l, _)| *l == "stats_test")
                .map_or_else(Default::default, |(_, t)| t);
            let delta = now.since(before);
            if delta.jobs >= 1 {
                assert!(
                    delta.busy_us >= 5_000,
                    "a 5 ms job recorded only {} µs busy",
                    delta.busy_us
                );
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the job finished but never appeared in stats::snapshot()"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
