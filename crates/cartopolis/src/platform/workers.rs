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
    /// readback is detached onto that pool too — and CLAUDE.md records the general
    /// rule that anything spawned there inherits it. A pool of our own is the fix
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
    use std::collections::VecDeque;
    use std::sync::{Arc, Condvar, Mutex, OnceLock};

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
                .spawn(move || loop {
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

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

/// While set, [`submit`] parks jobs in [`PARKED`] instead of dispatching them.
static PAUSED: AtomicBool = AtomicBool::new(false);
/// Jobs submitted while paused, released in order by `set_paused(false)`.
static PARKED: Mutex<Vec<Job>> = Mutex::new(Vec::new());
/// Jobs handed to the pool and not yet finished — queued *or* running. Read by
/// [`outstanding`].
static OUTSTANDING: AtomicUsize = AtomicUsize::new(0);

/// Hand one CPU-bound job to the pool (or hold it, while paused — see
/// [`set_paused`]).
pub fn submit(job: Job) {
    // Acquire/Release against `set_paused`: a job admitted here must be visible
    // to the drain, and a job parked here must be visible to whoever unparks.
    if PAUSED.load(Ordering::Acquire) {
        PARKED.lock().unwrap_or_else(|e| e.into_inner()).push(job);
        return;
    }
    dispatch(job);
}

fn dispatch(job: Job) {
    OUTSTANDING.fetch_add(1, Ordering::AcqRel);
    imp::submit(Box::new(move || {
        // A guard rather than a decrement after `job()`: a panicking job would
        // otherwise leak a count forever and `outstanding()` would never reach
        // zero again, which is precisely the condition a caller waits on.
        struct Done;
        impl Drop for Done {
            fn drop(&mut self) {
                OUTSTANDING.fetch_sub(1, Ordering::AcqRel);
            }
        }
        let _done = Done;
        job();
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
        for job in batch {
            dispatch(job);
        }
    }
}

/// Start the pool. `hint` is the caller's idea of a good width (typically
/// `navigator.hardwareConcurrency - 1`, leaving the main thread a core to render on).
pub fn start(hint: usize) {
    init(hint);
}
