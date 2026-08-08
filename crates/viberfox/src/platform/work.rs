//! Frame-gated work dispatch (ADR-033) — the wasm-safe replacement for a worker
//! thread plus a `std::thread::sleep` rate-limit.
//!
//! The streaming systems already run every frame, so the schedule *is* the clock:
//! each frame the system asks the dispatcher for the next job it is allowed to
//! start — under a concurrency cap, and past a minimum spacing between dispatches —
//! spawns it on the `IoTaskPool`, and calls [`Dispatcher::completed`] as results
//! drain. No OS thread and no blocking sleep, so the same throttle works in the
//! browser.
//!
//! Time is passed in by the caller (Bevy `Time::elapsed_secs_f64`); the dispatcher
//! never touches `std::time::Instant`, which panics on wasm.

use std::collections::VecDeque;

/// A bounded, optionally rate-limited job queue. `J` is the caller's job
/// descriptor (a tile key, a cell coordinate, an Overpass request…).
pub struct Dispatcher<J> {
    pending: VecDeque<J>,
    in_flight: usize,
    max_in_flight: usize,
    min_interval: f64,
    last_dispatch: Option<f64>,
}

impl<J> Dispatcher<J> {
    /// `max_in_flight` caps concurrent tasks; `min_interval_secs` is the minimum
    /// spacing between dispatches (0.0 for "as fast as the concurrency cap allows",
    /// e.g. our own tile/3DBAG servers; > 0 to respect a shared upstream's rate
    /// policy, e.g. Overpass).
    pub fn new(max_in_flight: usize, min_interval_secs: f64) -> Self {
        Self {
            pending: VecDeque::new(),
            in_flight: 0,
            max_in_flight: max_in_flight.max(1),
            min_interval: min_interval_secs.max(0.0),
            last_dispatch: None,
        }
    }

    /// Enqueue a job. De-duplication (not re-queuing in-flight/known work) is the
    /// caller's concern, as it was with the old channel.
    pub fn queue(&mut self, job: J) {
        self.pending.push_back(job);
    }

    /// Jobs waiting to start (not counting in-flight ones).
    pub fn queued(&self) -> usize {
        self.pending.len()
    }

    // No `busy()` here on purpose. The `--shot` harness's settle test reads
    // `CellStream::busy`, which is the same question asked one layer up and
    // cannot delegate: the stream counts its own in-flight *keys* and its
    // despawn queue, neither of which the dispatcher can see. A second answer
    // here would be a subset of the real one, silently.

    /// Take every job that has not started yet, leaving the in-flight count alone.
    ///
    /// For a caller that re-decides its whole work list each pass rather than
    /// accumulating one: the globe quadtree re-selects and re-sorts its desired
    /// tiles (coarse first, nearest first) every `QT_SELECT_INTERVAL`, and a queue
    /// left over from an earlier pass would outrank the fresh order — spending the
    /// fetch budget on tiles the view has since moved past.
    pub fn drain_pending(&mut self) -> Vec<J> {
        self.pending.drain(..).collect()
    }

    /// Count a job as in flight again after it was completed.
    ///
    /// One caller: [`CellStream::defer`](super::stream::CellStream::defer), where a
    /// result is drained (freeing its slot) and then handed back because the frame's
    /// build cap was already spent. It is still outstanding work holding a decoded
    /// payload, so it has to keep counting against the bound — otherwise the stream
    /// admits fresh fetches on top of results it has not consumed, and the
    /// concurrency cap stops bounding memory, which is half of what it is for.
    pub fn reoccupy(&mut self) {
        self.in_flight += 1
    }

    /// The next job allowed to start at time `now`, or `None` if at the concurrency
    /// cap or still inside the throttle interval. Records the dispatch; the caller
    /// spawns the returned job and calls [`Dispatcher::completed`] when its result
    /// is drained. Loop on it to fill the concurrency budget each frame.
    pub fn next(&mut self, now: f64) -> Option<J> {
        if self.in_flight >= self.max_in_flight {
            return None;
        }
        if let Some(last) = self.last_dispatch {
            if now - last < self.min_interval {
                return None;
            }
        }
        let job = self.pending.pop_front()?;
        self.in_flight += 1;
        self.last_dispatch = Some(now);
        Some(job)
    }

    /// Mark one dispatched job as finished — call once per result drained, to free
    /// a concurrency slot.
    pub fn completed(&mut self) {
        self.in_flight = self.in_flight.saturating_sub(1);
    }
}

impl<J> Default for Dispatcher<J> {
    fn default() -> Self {
        Self::new(1, 0.0)
    }
}
