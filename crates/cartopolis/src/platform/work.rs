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

use super::fetch_gate::Rank;

/// One queued job and how urgent it is.
///
/// `seq` is what keeps the ordering **stable**: jobs of equal rank come out in
/// the order they went in, which is the behaviour every caller had before ranks
/// existed and which several of them rely on (`map_stream` builds its clipmap
/// ring coarse-first and expects that order back).
struct Queued<J> {
    rank: Rank,
    seq: u64,
    job: J,
}

/// A bounded, optionally rate-limited job queue. `J` is the caller's job
/// descriptor (a tile key, a cell coordinate, an Overpass request…).
///
/// The queue is ordered by [`Rank`] — nearest first — rather than by arrival.
/// That matters because a streamer's desired set is re-derived as the camera
/// moves, so a queue that had been filled from an earlier, further-away position
/// would otherwise outrank the cells the view is waiting on now. `globe` used to
/// work around exactly that by draining and re-queueing its whole list every
/// selection pass ([`drain_pending`](Self::drain_pending)); a caller that states
/// ranks needs no such pass.
pub struct Dispatcher<J> {
    pending: VecDeque<Queued<J>>,
    /// Monotonic arrival counter, for the tie-break above.
    next_seq: u64,
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
            next_seq: 0,
            in_flight: 0,
            max_in_flight: max_in_flight.max(1),
            min_interval: min_interval_secs.max(0.0),
            last_dispatch: None,
        }
    }

    /// Enqueue a job at a stated urgency. De-duplication (not re-queuing
    /// in-flight/known work) is the caller's concern, as it was with the old
    /// channel.
    pub fn queue_ranked(&mut self, job: J, rank: Rank) {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.pending.push_back(Queued { rank, seq, job });
    }

    /// Enqueue a job with no distance to state — see
    /// [`fetch_gate::ANY`](super::fetch_gate::ANY).
    pub fn queue(&mut self, job: J) {
        self.queue_ranked(job, super::fetch_gate::ANY);
    }

    /// The rank of the job that would come out next, or `None` on an empty queue.
    ///
    /// Separate from [`next`](Self::next) so a caller can ask the global
    /// [`fetch_gate`](super::fetch_gate) whether that job may start *before*
    /// taking it out — a job pulled and then refused would have to be pushed
    /// back, losing its place.
    pub fn peek_rank(&self) -> Option<Rank> {
        self.best().map(|i| self.pending[i].rank)
    }

    /// Index of the best queued job: lowest rank, earliest arrival among equals.
    fn best(&self) -> Option<usize> {
        self.pending
            .iter()
            .enumerate()
            .min_by_key(|(_, q)| (q.rank, q.seq))
            .map(|(i, _)| i)
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

    /// Drop every queued copy of one job that has not started yet, leaving the
    /// in-flight count alone. Reports whether anything was removed.
    ///
    /// The counterpart to [`CellStream::forget`](super::stream::CellStream::forget):
    /// a cell that leaves the live set is dropped from `requested`, and without
    /// this its queue entry would survive to be dispatched a second time after a
    /// later `request` re-queued it — spending a slot the pairing with
    /// [`completed`](Self::completed) can never give back.
    pub fn remove(&mut self, job: &J) -> bool
    where
        J: PartialEq,
    {
        let before = self.pending.len();
        self.pending.retain(|q| &q.job != job);
        self.pending.len() != before
    }

    /// Take every job that has not started yet, leaving the in-flight count alone.
    ///
    /// For a caller that re-decides its whole work list each pass rather than
    /// accumulating one: the globe quadtree re-selects and re-sorts its desired
    /// tiles (coarse first, nearest first) every `QT_SELECT_INTERVAL`, and a queue
    /// left over from an earlier pass would outrank the fresh order — spending the
    /// fetch budget on tiles the view has since moved past.
    pub fn drain_pending(&mut self) -> Vec<J> {
        self.pending.drain(..).map(|q| q.job).collect()
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
        let job = self.pending.remove(self.best()?)?.job;
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

#[cfg(test)]
mod tests {
    use super::Dispatcher;
    use crate::platform::fetch_gate::ANY;

    /// No clock is involved in any of these, so a zero `now` is honest rather
    /// than a placeholder: `min_interval` is 0.0 throughout.
    fn dispatcher() -> Dispatcher<&'static str> {
        Dispatcher::new(8, 0.0)
    }

    #[test]
    fn the_nearest_job_goes_first_whatever_order_it_arrived_in() {
        let mut d = dispatcher();
        d.queue_ranked("far", 4);
        d.queue_ranked("near", 0);
        d.queue_ranked("middle", 2);
        assert_eq!(d.next(0.0), Some("near"));
        assert_eq!(d.next(0.0), Some("middle"));
        assert_eq!(d.next(0.0), Some("far"));
    }

    /// Equal ranks keep arrival order — `map_stream` fills its clipmap ring
    /// coarse-first and gets that order back, which is what stops a fine tile
    /// landing over a hole.
    #[test]
    fn equal_ranks_come_out_in_the_order_they_went_in() {
        let mut d = dispatcher();
        for job in ["first", "second", "third"] {
            d.queue_ranked(job, 3);
        }
        assert_eq!(d.next(0.0), Some("first"));
        assert_eq!(d.next(0.0), Some("second"));
        assert_eq!(d.next(0.0), Some("third"));
    }

    /// An unranked caller is unchanged: plain first-in-first-out.
    #[test]
    fn an_unranked_queue_is_still_first_in_first_out() {
        let mut d = dispatcher();
        d.queue("a");
        d.queue("b");
        assert_eq!(d.peek_rank(), Some(ANY));
        assert_eq!(d.next(0.0), Some("a"));
        assert_eq!(d.next(0.0), Some("b"));
        assert_eq!(d.next(0.0), None);
    }

    /// A later, nearer request must be able to overtake a queue filled from an
    /// earlier camera position — the case `globe` drains its whole queue to get.
    #[test]
    fn a_later_nearer_request_overtakes_a_stale_queue() {
        let mut d = dispatcher();
        for i in 0..5 {
            d.queue_ranked("stale", 5 + i);
        }
        d.queue_ranked("fresh", 0);
        assert_eq!(d.next(0.0), Some("fresh"));
    }

    #[test]
    fn removing_and_draining_still_reach_the_job() {
        let mut d = dispatcher();
        d.queue_ranked("keep", 1);
        d.queue_ranked("drop", 0);
        assert!(d.remove(&"drop"));
        assert!(!d.remove(&"drop"));
        assert_eq!(d.peek_rank(), Some(1));
        assert_eq!(d.drain_pending(), vec!["keep"]);
        assert_eq!(d.queued(), 0);
    }
}
