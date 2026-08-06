//! The bookkeeping every cell streamer was writing out by hand.
//!
//! Six systems stream something keyed by a map cell — ground tiles
//! (`map_stream`), vector surfaces (`map_geometry`), the globe quadtree
//! (`globe`), 3DBAG LoD2.2 cells (`lod22`), POI marker cells (`data_layers`) and
//! the far building cells (`osm_buildings`) — and all six had the same five or six
//! fields with the same meaning: a `requested` set, a `failed` map with a backoff,
//! an `Arc<Mutex<Vec<…>>>` results sink, a concurrency bound, a despawn queue with
//! a per-frame cap, and an anchor generation.
//!
//! Six copies meant six sets of slightly different bugs, and the interesting ones
//! were the *omissions*: `lod22` freed all 25 of its merged 3DBAG meshes in one
//! frame on a re-anchor and `data_layers` freed every marker entity of every
//! enabled layer in one frame, which is the mass-despawn burst CLAUDE.md names as
//! the `VK_ERROR_DEVICE_LOST` pattern — while the other four had a per-frame cap
//! for exactly that reason. `map_stream` had no concurrency bound at all, so a
//! fresh anchor released all 424 of its tile fetches at once. Adopting one type
//! fixes those as a consequence of using it rather than as three separate patches.
//!
//! [`CellStream`] owns the bookkeeping and nothing else: what a cell *is*, what
//! fetching it means and what to do with the result stay with the caller. It
//! composes with the other two ADR-033 seams rather than replacing them —
//! [`Dispatcher`](super::work::Dispatcher) still answers "may a job start this
//! frame", and the caller still splits its own task between the `IoTaskPool` and
//! [`workers`](super::workers).
//!
//! **Time is the caller's, in seconds** (Bevy `Time::elapsed_secs_f64`), the same
//! convention `Dispatcher` uses and for the same reason: `std::time::Instant`
//! panics on wasm, and threading one clock through beats two notions of "now".

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::sync::{Arc, Mutex};

use bevy::prelude::{Commands, Entity};

use super::work::Dispatcher;

/// How a stream admits work and retries failures. All of it is per-streamer
/// policy, so it is stated at the construction site next to the reason.
#[derive(Debug, Clone, Copy)]
pub struct Policy {
    /// Concurrent in-flight jobs. Also bounds how many finished results can be
    /// waiting in the sink, which is what keeps a burst from turning a frame-time
    /// problem into a memory one (each undrained tile holds a full mip chain).
    pub max_in_flight: usize,
    /// Minimum spacing between dispatches, for a shared upstream with a rate
    /// policy (Overpass). 0.0 = as fast as `max_in_flight` allows.
    pub min_interval_secs: f64,
    /// Attempts before a cell is [`exhausted`](CellStream::exhausted).
    pub max_attempts: u32,
    /// Wait after the *first* failure before retrying. 0.0 = next frame.
    pub backoff_secs: f64,
    /// How many times the backoff may double over successive failures (0 = a flat
    /// `backoff_secs` every time).
    pub backoff_doublings: u32,
    /// Once exhausted: wait this long, then start over with a clean attempt
    /// count. 0.0 = give up permanently (until the cell is forgotten).
    pub cooldown_secs: f64,
    /// Entities freed per frame. The mass-despawn device-loss guard; every
    /// streamer needs one, which is most of why this type exists.
    pub despawns_per_frame: usize,
}

/// One failed cell's history.
#[derive(Debug, Clone, Copy)]
struct Failure {
    attempts: u32,
    /// Clock reading of the last failure.
    at: f64,
}

/// Streaming bookkeeping for cells keyed by `K`, whose finished form is `R`.
pub struct CellStream<K, R> {
    policy: Policy,
    dispatch: Dispatcher<K>,
    /// Cells queued, in flight, or done — so a slow fetch is not requested twice.
    requested: HashSet<K>,
    /// Cells with a task actually running. A *set*, not a counter: a counter can
    /// only be kept honest by pairing every dispatch with exactly one completion,
    /// and the paths that drop a cell mid-flight (it left the ring, the anchor
    /// moved) are precisely the ones that forget to. A set cannot leak a slot.
    in_flight: HashSet<K>,
    failed: HashMap<K, Failure>,
    results: Arc<Mutex<Vec<(K, R)>>>,
    despawn_queue: Vec<Entity>,
    /// The `MapStream::anchor_generation` these cells belong to.
    generation: u32,
}

impl<K, R> CellStream<K, R>
where
    K: Eq + Hash + Clone,
{
    pub fn new(policy: Policy) -> Self {
        Self {
            dispatch: Dispatcher::new(policy.max_in_flight, policy.min_interval_secs),
            policy,
            requested: HashSet::new(),
            in_flight: HashSet::new(),
            failed: HashMap::new(),
            results: Arc::new(Mutex::new(Vec::new())),
            despawn_queue: Vec::new(),
            generation: 0,
        }
    }

    /// The sink a spawned task pushes its finished cell into. Cloned into the
    /// task, so it outlives a re-anchor — [`reset`](Self::reset) swaps in a fresh
    /// one and the orphaned pushes go nowhere.
    pub fn sink(&self) -> Arc<Mutex<Vec<(K, R)>>> {
        self.results.clone()
    }

    /// Ask for a cell. `true` if it was queued — i.e. it was not already known and
    /// is not inside a failure backoff. Call it for every wanted cell every frame:
    /// this *is* the retry pump, so a streamer needs no second loop over `failed`.
    pub fn request(&mut self, key: K, now: f64) -> bool {
        if self.requested.contains(&key) {
            return false;
        }
        if let Some(f) = self.failed.get(&key).copied() {
            if f.attempts >= self.policy.max_attempts {
                // Exhausted. A zero cooldown means permanently so; otherwise wait
                // it out and start again with a clean count, which is how a
                // rate-limited source heals instead of leaving a permanent hole.
                if self.policy.cooldown_secs <= 0.0 || now - f.at < self.policy.cooldown_secs {
                    return false;
                }
                self.failed.remove(&key);
            } else if now - f.at < self.backoff_for(f.attempts) {
                return false;
            }
        }
        self.requested.insert(key.clone());
        self.dispatch.queue(key);
        true
    }

    /// Drop every cell that is queued but has not started, so the next round of
    /// [`request`](Self::request) calls decides the order on its own.
    ///
    /// For a streamer that re-derives its whole desired set each pass instead of
    /// accumulating one — the globe quadtree re-selects and re-sorts (coarse first,
    /// nearest first) every `QT_SELECT_INTERVAL`, and a queue left from an earlier
    /// pass would outrank the fresh order and spend the fetch budget on tiles the
    /// view has moved past. In-flight cells are untouched: they are already costing
    /// a request, and the caller's own cancellation check handles them.
    pub fn clear_queue(&mut self) {
        for key in self.dispatch.drain_pending() {
            self.requested.remove(&key);
        }
    }

    /// The next cell allowed to start at `now`, or `None` at the concurrency cap /
    /// inside the throttle. Loop on it to fill the budget each frame.
    pub fn next_job(&mut self, now: f64) -> Option<K> {
        let key = self.dispatch.next(now)?;
        self.in_flight.insert(key.clone());
        Some(key)
    }

    /// Everything the tasks have finished since the last call, freeing a
    /// concurrency slot for each. Anything the caller cannot use this frame goes
    /// back with [`defer`](Self::defer).
    pub fn take_results(&mut self) -> Vec<(K, R)> {
        let taken: Vec<(K, R)> = self
            .results
            .lock()
            .map(|mut g| std::mem::take(&mut *g))
            .unwrap_or_default();
        for (key, _) in &taken {
            self.release(key);
        }
        taken
    }

    /// Put results back, un-freeing their slots: they are still outstanding work,
    /// so they must keep counting against the bound (the globe defers whatever
    /// exceeds its per-frame build cap).
    pub fn defer(&mut self, items: Vec<(K, R)>) {
        for (key, _) in &items {
            if self.in_flight.insert(key.clone()) {
                // Re-occupy the slot `take_results` freed for it.
                self.dispatch.reoccupy();
            }
        }
        if let Ok(mut g) = self.results.lock() {
            g.extend(items);
        }
    }

    /// This cell is done and good: clear any failure history, keep it `requested`
    /// so it is not fetched again.
    pub fn succeeded(&mut self, key: &K) {
        self.failed.remove(key);
    }

    /// This cell failed. Drops it from `requested` so the backoff (rather than a
    /// separate retry loop) decides when [`request`](Self::request) lets it back
    /// in. Returns the attempt count.
    pub fn failed(&mut self, key: &K, now: f64) -> u32 {
        self.requested.remove(key);
        let f = self.failed.entry(key.clone()).or_insert(Failure {
            attempts: 0,
            at: now,
        });
        f.attempts += 1;
        f.at = now;
        f.attempts
    }

    /// Whether a cell has spent its attempt budget — the point at which a caller
    /// makes its placeholder final rather than waiting for more.
    pub fn exhausted(&self, key: &K) -> bool {
        self.failed
            .get(key)
            .is_some_and(|f| f.attempts >= self.policy.max_attempts)
    }

    /// Forget a cell entirely: it left the live set, so a later return must fetch
    /// again (the byte cache makes that cheap). Frees its slot if it was in
    /// flight; a result that lands afterwards simply finds nothing to release.
    pub fn forget(&mut self, key: &K) {
        self.requested.remove(key);
        self.failed.remove(key);
        self.release(key);
    }

    /// Whether this cell is queued, in flight or already served.
    pub fn is_requested(&self, key: &K) -> bool {
        self.requested.contains(key)
    }

    /// In-flight jobs — the number the concurrency bound is against.
    pub fn in_flight(&self) -> usize {
        self.in_flight.len()
    }

    /// Anything still moving: work outstanding or entities still to free. The
    /// `--shot` harness settles on this.
    pub fn busy(&self) -> bool {
        !self.in_flight.is_empty() || self.dispatch.queued() > 0 || !self.despawn_queue.is_empty()
    }

    /// Queue an entity for bounded teardown. **Not** `commands.entity().despawn()`:
    /// a LOD zoom-out, a stale reclaim or a re-anchor retires a whole ring at once,
    /// and freeing that many meshes and textures in one command buffer is what
    /// hangs a weak GPU (`VK_ERROR_DEVICE_LOST`).
    pub fn retire(&mut self, entity: Entity) {
        self.despawn_queue.push(entity);
    }

    /// Queue several entities (one cell's worth) for bounded teardown.
    pub fn retire_all(&mut self, entities: impl IntoIterator<Item = Entity>) {
        self.despawn_queue.extend(entities);
    }

    /// Drop the pending teardown without freeing anything.
    ///
    /// One caller: `map_stream::apply_reanchor`, which re-collects every live
    /// `StreamTile` from a query — including the ones already queued from a
    /// previous re-anchor. Clearing first is what stops it queueing an entity
    /// twice and double-freeing it.
    pub fn clear_despawn_queue(&mut self) {
        self.despawn_queue.clear();
    }

    /// Free up to `despawns_per_frame` retired entities. Call it **before** any
    /// early return in the streaming system: a source switch or a climb into space
    /// must still clear steadily rather than stall with a full queue.
    pub fn drain_despawns(&mut self, commands: &mut Commands) {
        if self.despawn_queue.is_empty() {
            return;
        }
        let n = self.despawn_queue.len().min(self.policy.despawns_per_frame);
        for e in self.despawn_queue.drain(..n) {
            commands.entity(e).despawn();
        }
    }

    /// Entities still awaiting teardown — for diagnostics.
    pub fn despawn_backlog(&self) -> usize {
        self.despawn_queue.len()
    }

    /// Adopt `generation`, reporting `true` when it changed (i.e. the map
    /// re-anchored and every cell's placement is invalid).
    pub fn sync(&mut self, generation: u32) -> bool {
        if self.generation == generation {
            return false;
        }
        self.generation = generation;
        true
    }

    /// Drop all bookkeeping for a new anchor. The results sink is *replaced*
    /// rather than emptied, so a task still in flight pushes into an orphan and
    /// cannot deliver a cell placed in the old origin's frame. Retired entities
    /// are deliberately left queued — they are real entities that still have to be
    /// freed, under the cap.
    pub fn reset(&mut self) {
        self.requested.clear();
        self.in_flight.clear();
        self.failed.clear();
        self.dispatch = Dispatcher::new(self.policy.max_in_flight, self.policy.min_interval_secs);
        self.results = Arc::new(Mutex::new(Vec::new()));
    }

    /// Backoff before retrying a cell that has failed `attempts` times.
    fn backoff_for(&self, attempts: u32) -> f64 {
        let doublings = attempts
            .saturating_sub(1)
            .min(self.policy.backoff_doublings);
        self.policy.backoff_secs * f64::from(1u32 << doublings)
    }

    fn release(&mut self, key: &K) {
        if self.in_flight.remove(key) {
            self.dispatch.completed();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> Policy {
        Policy {
            max_in_flight: 2,
            min_interval_secs: 0.0,
            max_attempts: 3,
            backoff_secs: 1.0,
            backoff_doublings: 0,
            cooldown_secs: 0.0,
            despawns_per_frame: 2,
        }
    }

    fn stream() -> CellStream<i32, &'static str> {
        CellStream::new(policy())
    }

    #[test]
    fn the_bound_holds_and_a_drained_result_frees_a_slot() {
        let mut s = stream();
        for k in 0..5 {
            assert!(s.request(k, 0.0));
        }
        assert!(!s.request(3, 0.0), "a known cell must not be re-queued");

        let dispatched: Vec<i32> = std::iter::from_fn(|| s.next_job(0.0)).collect();
        assert_eq!(
            dispatched,
            vec![0, 1],
            "the concurrency cap let extra work in"
        );
        assert_eq!(s.in_flight(), 2);

        s.sink().lock().unwrap().push((0, "done"));
        assert_eq!(s.take_results().len(), 1);
        assert_eq!(s.in_flight(), 1);
        assert_eq!(std::iter::from_fn(|| s.next_job(0.0)).count(), 1);
    }

    /// The globe re-decides its desired set every selection pass, so a queue left
    /// from the previous one must not outrank the fresh order. In-flight work is
    /// already costing a request and is left alone.
    #[test]
    fn clearing_the_queue_drops_only_what_has_not_started() {
        let mut s = stream();
        for k in 0..4 {
            s.request(k, 0.0);
        }
        s.next_job(0.0); // 0 starts
        s.clear_queue();

        assert!(s.is_requested(&0), "an in-flight cell was un-requested");
        assert_eq!(s.in_flight(), 1);
        assert!(
            !s.request(0, 0.0),
            "an in-flight cell must not be re-queued"
        );
        for k in 1..4 {
            assert!(
                s.request(k, 0.0),
                "cell {k} was not released by clear_queue"
            );
        }
    }

    /// The globe defers whatever exceeds its per-frame build cap. A deferred cell
    /// is still outstanding, so it must keep holding its slot — otherwise the
    /// stream admits fresh work on top of results it has not consumed, which is
    /// the memory blow-up the bound exists to prevent.
    #[test]
    fn a_deferred_result_keeps_its_slot() {
        let mut s = stream();
        s.request(1, 0.0);
        s.next_job(0.0);
        s.sink().lock().unwrap().push((1, "done"));

        let taken = s.take_results();
        assert_eq!(s.in_flight(), 0);
        s.defer(taken);
        assert_eq!(s.in_flight(), 1);
        assert_eq!(
            s.take_results().len(),
            1,
            "the deferred result must survive"
        );
    }

    #[test]
    fn a_failure_backs_off_then_retries_then_gives_up() {
        let mut s = stream();
        s.request(1, 0.0);
        s.next_job(0.0);

        assert_eq!(s.failed(&1, 10.0), 1);
        assert_eq!(
            s.in_flight(),
            1,
            "the task is still running; only the flag cleared"
        );
        assert!(!s.request(1, 10.5), "retried inside the backoff");
        assert!(s.request(1, 11.0), "never retried after the backoff");
        assert!(!s.exhausted(&1));

        s.failed(&1, 20.0);
        s.failed(&1, 30.0);
        assert!(s.exhausted(&1));
        // Zero cooldown = permanently given up, so the caller can make its
        // placeholder final rather than waiting for a fetch that will not come.
        assert!(!s.request(1, 1_000.0));
    }

    /// The globe's policy: back off after N attempts, then start over — a
    /// rate-limited source has to heal, not leave a permanent hole in the planet.
    #[test]
    fn a_cooldown_gives_a_cell_fresh_attempts() {
        let mut s = CellStream::<i32, ()>::new(Policy {
            max_attempts: 2,
            backoff_secs: 0.0,
            cooldown_secs: 20.0,
            ..policy()
        });
        s.request(1, 0.0);
        s.failed(&1, 0.0);
        s.failed(&1, 1.0);
        assert!(s.exhausted(&1));
        assert!(!s.request(1, 10.0), "retried inside the cooldown");
        assert!(s.request(1, 21.0), "the cooldown never expired");
        assert!(
            !s.exhausted(&1),
            "attempts must start over after a cooldown"
        );
    }

    #[test]
    fn the_backoff_doubles_when_asked_to() {
        let mut s = CellStream::<i32, ()>::new(Policy {
            backoff_secs: 15.0,
            backoff_doublings: 2,
            max_attempts: 9,
            ..policy()
        });
        // 15 s, 30 s, 60 s, then flat at 60 s — Overpass wobbles outlast a retry.
        for (attempt, wait) in [(1u32, 15.0), (2, 30.0), (3, 60.0), (4, 60.0)] {
            let t = f64::from(attempt) * 1_000.0;
            s.request(1, t - 1.0);
            s.failed(&1, t);
            assert!(
                !s.request(1, t + wait - 0.1),
                "attempt {attempt} retried early"
            );
            assert!(s.request(1, t + wait), "attempt {attempt} never retried");
        }
    }

    /// A cell that leaves the live set while its fetch is in the air must free its
    /// slot, or a streamer that pans steadily leaks the whole bound and stops.
    #[test]
    fn forgetting_an_in_flight_cell_frees_its_slot() {
        let mut s = stream();
        s.request(1, 0.0);
        s.request(2, 0.0);
        s.next_job(0.0);
        s.next_job(0.0);
        assert_eq!(s.in_flight(), 2);

        s.forget(&1);
        assert_eq!(s.in_flight(), 1);
        assert!(
            s.request(1, 0.0),
            "a forgotten cell must be requestable again"
        );

        // The late result finds nothing to release, and must not double-free.
        s.sink().lock().unwrap().push((1, "late"));
        s.take_results();
        assert_eq!(s.in_flight(), 1);
    }

    #[test]
    fn a_reset_forgets_the_cells_but_keeps_the_teardown() {
        let mut s = stream();
        s.request(1, 0.0);
        s.next_job(0.0);
        s.failed(&2, 0.0);
        let orphan = s.sink();
        s.retire(Entity::from_raw_u32(7).unwrap());

        assert!(s.sync(1), "a generation change must be reported");
        assert!(!s.sync(1), "…and only once");
        s.reset();

        assert_eq!(s.in_flight(), 0);
        assert!(!s.is_requested(&1));
        assert!(
            s.request(2, 0.0),
            "a reset must clear the failure history too"
        );
        assert_eq!(
            s.despawn_backlog(),
            1,
            "queued entities still have to be freed"
        );

        // An in-flight task pushing into the old sink cannot deliver a cell whose
        // placement belongs to the previous anchor.
        orphan.lock().unwrap().push((1, "stale"));
        assert!(s.take_results().is_empty());
    }
}
