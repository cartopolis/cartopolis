//! Allocation budgets — the part of performance a machine with no GPU can still
//! hold to account.
//!
//! Every prior attempt to measure this tree's cost was a **timer**, and every one
//! of them was reverted (`docs/notes/tile-raster-cost.md`: "The benchmarks were
//! reverted, not committed — nothing in the tree measures this"). A timer cannot
//! survive here for reasons that have nothing to do with discipline: CI runs two
//! jobs on a shared 8-core VM at `CARGO_BUILD_JOBS=3`, so a wall-clock threshold
//! is a flake generator, and a threshold loose enough not to flake is loose enough
//! not to catch anything.
//!
//! An allocation count has neither problem. It does not move with core count, CPU
//! frequency, thermal state or a noisy neighbour — the same input allocates the
//! same number of times on a Fairphone 4 and on a 22-core workstation. So it can
//! be asserted **exactly**, as a normal non-ignored test, in the same container
//! that cannot run a single frame of the renderer.
//!
//! ## What it is a proxy for, and what it is not
//!
//! It is a good proxy for the costs the tile pipeline actually has. Every
//! regression this crate has recorded was allocation-shaped: a `Vec` per triangle,
//! a `String` cloned per tag key *and* value per feature, an untyped
//! `serde_json::Value` walk whose *drop* outweighed its construction (14.2% of all
//! samples in `docs/notes/wasm-performance.md`'s profile — the single largest
//! entry). On a phone it is better than a proxy: allocation is malloc contention
//! plus cache pressure plus, eventually, the low-memory killer.
//!
//! It is **not** a proxy for scan conversion. `render_mvt` is dominated by
//! tiny-skia walking spans inside buffers it already owns (~20× MVT parsing, per
//! the note), and that work is nearly allocation-free — a rasteriser that got
//! twice as slow per pixel would not move these numbers at all. `paths` and
//! `path_points` below exist for exactly that blind spot: they count the geometry
//! *handed to* the rasteriser, which is the input its cost scales with.
//!
//! ## Reading a failure
//!
//! A budget test failing is not automatically a bug — it is a claim that the cost
//! changed, which is sometimes what a commit meant to do. The contract is the one
//! `PROTOCOL_HISTORY` already uses in this workspace: **restate the number in a
//! reviewed commit**, with the reason in the body. What must not happen is the
//! number drifting upward unremarked, which is what "no benchmark at all" has
//! meant here for months.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
    static BYTES: Cell<u64> = const { Cell::new(0) };
    static LIVE: Cell<i64> = const { Cell::new(0) };
    static PEAK: Cell<i64> = const { Cell::new(0) };
    static ARMED: Cell<bool> = const { Cell::new(false) };
}

/// A `GlobalAlloc` that counts, per thread, while armed.
///
/// **Per thread, not globally**, and that is load-bearing rather than tidy: cargo
/// runs a crate's unit tests in parallel on a thread pool, so a process-wide
/// counter would report this test's allocations plus whatever the other 106 tests
/// happened to be doing at the time — a number that changes run to run and could
/// never be asserted exactly. Thread-local counters make each measurement the
/// property of the thread that took it, so the suite stays parallel.
///
/// Every cell is `const`-initialised and holds a `Copy` type with no `Drop`. Both
/// matter inside an allocator: a lazily-initialised TLS slot would allocate on
/// first touch (recursing into the allocator that is trying to record the
/// allocation), and a slot with a destructor can be observed *after* teardown has
/// begun, which is what `try_with` below is for.
pub struct Counting;

fn bump(size: usize) {
    // `try_with`, never `with`: a thread allocating during its own TLS teardown
    // would otherwise panic inside the allocator, which is not a recoverable place
    // to panic from. Losing a count at teardown is harmless — measurement is over
    // by then.
    let _ = ARMED.try_with(|armed| {
        if !armed.get() {
            return;
        }
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        let _ = BYTES.try_with(|c| c.set(c.get() + size as u64));
        let _ = LIVE.try_with(|live| {
            let now = live.get() + size as i64;
            live.set(now);
            let _ = PEAK.try_with(|peak| {
                if now > peak.get() {
                    peak.set(now);
                }
            });
        });
    });
}

fn drop_bytes(size: usize) {
    let _ = ARMED.try_with(|armed| {
        if !armed.get() {
            return;
        }
        let _ = LIVE.try_with(|live| live.set(live.get() - size as i64));
    });
}

// SAFETY: every method forwards to `System` unchanged; the counters are
// thread-local `Cell`s of `Copy` types and cannot themselves allocate.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        bump(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        drop_bytes(layout.size());
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        bump(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    // `realloc` is deliberately *not* overridden. `GlobalAlloc`'s default
    // implementation calls `self.alloc` and `self.dealloc`, so a growing `Vec` is
    // counted as the allocation it is — which is the whole point, since
    // "reallocated the buffer eleven times because nobody reserved capacity" is
    // exactly the regression class these budgets exist to catch. Forwarding to
    // `System.realloc` would hide it behind an in-place extension.
}

/// What one measured region cost.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cost {
    /// Number of calls into the allocator. The headline number: stable to the
    /// unit, and the one that moves when someone adds a `Vec` inside a loop.
    pub allocs: u64,
    /// Total bytes requested, summed over every allocation. Traffic, not
    /// residency — a buffer allocated and freed a thousand times counts a thousand
    /// times, which is the intent.
    pub bytes: u64,
    /// High-water mark of live bytes: the peak the phone's memory manager sees.
    ///
    /// Only counts allocations made *while armed*, so it is the peak of this
    /// region's own footprint, not of the process.
    pub peak_bytes: i64,
}

impl Cost {
    /// Run `f`, returning what it cost alongside its value.
    ///
    /// Not reentrant, and it does not need to be: nesting would make the inner
    /// region's allocations belong to two budgets at once, and every call site is
    /// one test measuring one pipeline stage.
    pub fn measure<T>(f: impl FnOnce() -> T) -> (T, Self) {
        ALLOCS.with(|c| c.set(0));
        BYTES.with(|c| c.set(0));
        LIVE.with(|c| c.set(0));
        PEAK.with(|c| c.set(0));
        ARMED.with(|c| c.set(true));
        let value = f();
        ARMED.with(|c| c.set(false));
        let cost = Self {
            allocs: ALLOCS.with(Cell::get),
            bytes: BYTES.with(Cell::get),
            peak_bytes: PEAK.with(Cell::get),
        };
        (value, cost)
    }

    /// One line for the printing hatches, so a `--nocapture` run reads as a table.
    #[must_use]
    pub fn line(&self, label: &str) -> String {
        format!(
            "{label:<34} {:>9} allocs  {:>9} KiB traffic  {:>8} KiB peak",
            self.allocs,
            self.bytes / 1024,
            self.peak_bytes / 1024
        )
    }
}

/// Assert an allocation count against a budget, with a failure that says what to
/// do about it.
///
/// A **ceiling**, not an equality. An exact count would be the stronger assertion
/// and it is what this reached for first, but it fails on changes that are not
/// regressions — a `HashMap` reaching a growth threshold one insert earlier, a
/// dependency's internal buffering. A ceiling with the measured value named beside
/// it keeps the number reviewable without making every unrelated commit red.
#[track_caller]
pub fn within(cost: Cost, budget: u64, what: &str) {
    assert!(
        cost.allocs <= budget,
        "{what} allocated {} times, budget is {budget}.\n\
         \n\
         If this is a regression, find the new allocation — the usual causes are a \
         collection built inside a per-feature loop, a `String` cloned out of a \
         dictionary, or a reserve that stopped being big enough.\n\
         \n\
         If the change was deliberate and the new cost is understood, raise the \
         budget *in the same commit*, and say in the body what bought the extra \
         allocations. An unexplained rise here is the thing this test exists to \
         prevent.",
        cost.allocs,
    );
}

/// Counters for work handed to the rasteriser, which allocation counts cannot see.
///
/// `render_mvt`'s cost is tiny-skia scan conversion — spans walked inside buffers
/// it already owns — so it barely allocates, and an allocation budget over it
/// would be a test that passes while the rasteriser doubles in cost. What *does*
/// scale with that cost is the geometry fed in: how many paths, and how many
/// points in them. Those are countable exactly, and they are what the overzoom
/// cull and the twice-painted `buildings` layer both move.
pub mod paths {
    use std::cell::Cell;

    thread_local! {
        static PATHS: Cell<u64> = const { Cell::new(0) };
        static POINTS: Cell<u64> = const { Cell::new(0) };
    }

    /// Record one path of `points` vertices submitted to the rasteriser.
    pub fn record(points: usize) {
        PATHS.with(|c| c.set(c.get() + 1));
        POINTS.with(|c| c.set(c.get() + points as u64));
    }

    /// Run `f` and report `(paths, points)` submitted during it.
    pub fn measure<T>(f: impl FnOnce() -> T) -> (T, u64, u64) {
        PATHS.with(|c| c.set(0));
        POINTS.with(|c| c.set(0));
        let value = f();
        (value, PATHS.with(Cell::get), POINTS.with(Cell::get))
    }
}

/// Per-pass wall-clock accumulators for `render_mvt`'s paint loop, recorded only
/// under `cfg(test)` and read only by `#[ignore]`d measurement hatches — never a
/// threshold, per the module docs above. They exist because the one prior
/// per-layer breakdown was reconstructed outside the real loop and summed to
/// ~282 ms against a measured 121 ms total (`docs/notes/tile-raster-cost.md`
/// says: instrument the real `render_mvt` before acting on any attribution).
pub mod passes {
    use std::cell::RefCell;
    use std::time::Duration;

    thread_local! {
        static PASSES: RefCell<Vec<(&'static str, Duration)>> = const { RefCell::new(Vec::new()) };
    }

    /// Accumulate `d` against `label` (summed across repeat runs of the loop).
    pub fn record(label: &'static str, d: Duration) {
        PASSES.with_borrow_mut(|v| match v.iter_mut().find(|(l, _)| *l == label) {
            Some((_, total)) => *total += d,
            None => v.push((label, d)),
        });
    }

    /// Run `f` and report the accumulated per-label durations, in first-recorded
    /// order (which for the paint loop is paint order).
    pub fn measure<T>(f: impl FnOnce() -> T) -> (T, Vec<(&'static str, Duration)>) {
        PASSES.with_borrow_mut(Vec::clear);
        let value = f();
        (value, PASSES.with_borrow(Clone::clone))
    }
}

/// Tests of the instrument itself.
///
/// A measuring tool nobody measured is a liability: every budget in
/// `perf_budgets.rs` is only as trustworthy as the counter under it, and the
/// failure mode that matters — a counter that silently reads zero — makes every
/// budget pass forever. These are cheap and they close that.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_counter_sees_an_allocation() {
        let (v, cost) = Cost::measure(|| vec![0_u8; 4096]);
        assert_eq!(v.len(), 4096);
        assert!(
            cost.allocs >= 1,
            "a 4 KiB Vec must register at least one allocation, got {}",
            cost.allocs
        );
        assert!(
            cost.bytes >= 4096,
            "traffic should include the buffer, got {}",
            cost.bytes
        );
    }

    #[test]
    fn doing_nothing_costs_nothing() {
        let (_, cost) = Cost::measure(|| ());
        assert_eq!(cost, Cost::default(), "an empty region must measure zero");
    }

    #[test]
    fn counting_stops_when_the_region_ends() {
        let (_, cost) = Cost::measure(|| ());
        // Allocate well clear of any measured region; the next measurement must
        // not inherit it.
        let escapee = vec![7_u8; 8192];
        assert_eq!(escapee.len(), 8192);
        let (_, after) = Cost::measure(|| ());
        assert_eq!(cost.allocs, 0);
        assert_eq!(
            after.allocs, 0,
            "allocations outside a measured region must not be counted"
        );
    }

    #[test]
    fn peak_tracks_the_high_water_mark_not_the_total() {
        // Two buffers allocated and dropped one after the other: traffic counts
        // both, peak counts only the larger — which is the distinction the phone's
        // memory manager cares about.
        let (_, cost) = Cost::measure(|| {
            drop(vec![0_u8; 200_000]);
            drop(vec![0_u8; 100_000]);
        });
        assert!(cost.bytes >= 300_000, "traffic sums both: {}", cost.bytes);
        assert!(
            cost.peak_bytes < 300_000,
            "peak must not sum buffers that never coexisted: {}",
            cost.peak_bytes
        );
        assert!(
            cost.peak_bytes >= 200_000,
            "peak must reach the largest live buffer: {}",
            cost.peak_bytes
        );
    }

    /// The counters are per-thread, which is what lets the suite stay parallel.
    /// If this ever fails, every budget in the crate became order-dependent.
    #[test]
    fn a_busy_neighbour_thread_does_not_land_in_our_count() {
        let (_, cost) = Cost::measure(|| {
            std::thread::scope(|s| {
                s.spawn(|| {
                    let mut sink = Vec::new();
                    for i in 0..1000 {
                        sink.push(vec![0_u8; 64 + i % 7]);
                    }
                    sink.len()
                })
                .join()
                .expect("neighbour thread finishes")
            })
        });
        // Our own thread allocated only what `thread::scope` needed — far below
        // the ~1000 the neighbour made.
        assert!(
            cost.allocs < 200,
            "a neighbour thread's 1000+ allocations leaked into this thread's \
             count ({}), so the counters are not thread-local",
            cost.allocs
        );
    }

    #[test]
    fn the_path_counter_records_what_it_is_given() {
        let (_, paths, points) = paths::measure(|| {
            paths::record(10);
            paths::record(5);
        });
        assert_eq!(paths, 2);
        assert_eq!(points, 15);
    }
}
