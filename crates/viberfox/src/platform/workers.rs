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
//! Native builds have no such gap: `AsyncComputeTaskPool` is already a real thread
//! pool, so [`submit`] forwards to it and the call sites read the same.

/// A unit of CPU-bound work. `Send` because it crosses to another core/worker.
pub type Job = Box<dyn FnOnce() + Send + 'static>;

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use super::Job;
    use bevy::tasks::AsyncComputeTaskPool;

    /// Native already has real threads — Bevy's compute pool is exactly this pool.
    pub fn init(_workers: usize) {}

    pub fn submit(job: Job) {
        AsyncComputeTaskPool::get()
            .spawn(async move { job() })
            .detach();
    }

    pub fn worker_count() -> usize {
        AsyncComputeTaskPool::get().thread_num()
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
                .name(format!("vibe-cpu-{i}"))
                .spawn(move || loop {
                    let job = {
                        let mut q = pool.queue.lock().unwrap_or_else(|e| e.into_inner());
                        loop {
                            match q.pop_front() {
                                Some(job) => break job,
                                None => {
                                    q = pool
                                        .idle
                                        .wait(q)
                                        .unwrap_or_else(|e| e.into_inner());
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
/// Pool width, for diagnostics/HUD. Native callers must wait until Bevy's task
/// pools exist (it reads `AsyncComputeTaskPool`), so it is not called from [`start`].
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
/// **Why this exists.** On native, [`submit`] *is* `AsyncComputeTaskPool` — and so
/// is Bevy's screenshot readback, which `collect_screenshots` spawns and detaches
/// onto the same pool. Our jobs are blocking closures (a full MVT rasterise, ~120 ms
/// each — `docs/notes/tile-raster-cost.md`), so each one owns a pool thread outright
/// with no yield point. On an 8-core host the pool is 3 threads wide, a teleport
/// queues hundreds of jobs, and the streamers keep refilling it: the readback future
/// is FIFO'd behind all of them and may simply never be polled. That is a `--shot`
/// that renders 194 healthy frames and never writes a PNG
/// (`docs/notes/headless-shots-software-renderer.md`).
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
