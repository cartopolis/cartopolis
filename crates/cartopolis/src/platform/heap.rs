//! How much of the web build's memory is in use.
//!
//! A WebAssembly module's linear memory only grows, and when it can grow no
//! further an allocation fails, Rust aborts, and the whole client dies with
//! `RuntimeError: unreachable` under `rust_oom`. That happened on 2026-10-20 a
//! few seconds after a scripted flight came down from 40 km, in Chrome, and it
//! can surface as a hang in Firefox (`docs/notes/wasm-performance.md`). So the
//! web build counts its live heap through this allocator: two atomic adds per
//! allocation over the system one. It reports the live bytes, the most ever live,
//! and the size of the linear memory itself, which is the high-water mark plus
//! whatever fragmentation keeps from being reused.
//!
//! Only the web build installs it (`lib.rs`). Natively the operating system
//! reports a process's memory far better than this would.

#[cfg(target_arch = "wasm32")]
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

/// The system allocator, counting.
#[cfg(target_arch = "wasm32")]
pub struct Counting;

#[cfg(target_arch = "wasm32")]
fn grew(by: usize) {
    let now = LIVE.fetch_add(by, Relaxed) + by;
    PEAK.fetch_max(now, Relaxed);
}

// SAFETY: every call forwards to `System` with the caller's own arguments; the
// counters are bookkeeping beside it and never touch the memory.
#[cfg(all(target_arch = "wasm32", not(heap_labels)))]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            grew(layout.size());
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            grew(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            if new_size >= layout.size() {
                grew(new_size - layout.size());
            } else {
                LIVE.fetch_sub(layout.size() - new_size, Relaxed);
            }
        }
        p
    }
}

// SAFETY: each allocation is the caller's layout grown by a header in front, at
// the caller's alignment, so the pointer returned is aligned and has `size` bytes
// behind it; every free reconstructs the same grown layout from the caller's.
#[cfg(all(target_arch = "wasm32", heap_labels))]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe { labelled(layout, false) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        unsafe { labelled(layout, true) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let h = header(layout);
        let base = unsafe { ptr.sub(h) };
        let id = unsafe { *base } as usize;
        labels::LIVE_BY[id.min(labels::SLOTS - 1)].fetch_sub(layout.size(), Relaxed);
        LIVE.fetch_sub(layout.size(), Relaxed);
        let grown = unsafe { Layout::from_size_align_unchecked(layout.size() + h, layout.align()) };
        unsafe { System.dealloc(base, grown) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_layout = unsafe { Layout::from_size_align_unchecked(new_size, layout.align()) };
        let p = unsafe { self.alloc(new_layout) };
        if !p.is_null() {
            unsafe { std::ptr::copy_nonoverlapping(ptr, p, layout.size().min(new_size)) };
            unsafe { self.dealloc(ptr, layout) };
        }
        p
    }
}

#[cfg(all(target_arch = "wasm32", heap_labels))]
unsafe fn labelled(layout: Layout, zeroed: bool) -> *mut u8 {
    let h = header(layout);
    let Ok(grown) = Layout::from_size_align(layout.size() + h, layout.align()) else {
        return std::ptr::null_mut();
    };
    let base = unsafe {
        if zeroed {
            System.alloc_zeroed(grown)
        } else {
            System.alloc(grown)
        }
    };
    if base.is_null() {
        return base;
    }
    let id = labels::current();
    unsafe { *base = id };
    let now = labels::LIVE_BY[id as usize].fetch_add(layout.size(), Relaxed) + layout.size();
    labels::PEAK_BY[id as usize].fetch_max(now, Relaxed);
    grew(layout.size());
    unsafe { base.add(h) }
}

/// Live heap bytes, the most ever live, and the linear memory's size: all `0`
/// where the counting allocator is not installed.
#[must_use]
pub fn usage() -> (usize, usize, usize) {
    #[cfg(target_arch = "wasm32")]
    let memory = core::arch::wasm32::memory_size(0) * 65_536;
    #[cfg(not(target_arch = "wasm32"))]
    let memory = 0;
    (LIVE.load(Relaxed), PEAK.load(Relaxed), memory)
}

/// **Which part of the client holds the heap**, in a `heap_labels` build (`CARTO_HEAP_LABELS=1`, `build.rs`).
///
/// Live heap bytes are counted by label: the label of whatever was running
/// when an allocation was made. A worker job carries its submission label
/// (`map_tiles`, `height patch`, …: `platform::workers`), the render world runs as
/// `render`, and the rest of the page's thread is `main`. Each allocation gets a
/// small header holding its label, so a free is charged to the label that
/// allocated, wherever it happens. That costs 16 bytes or more per allocation,
/// which is why it is a feature for diagnostic builds and not the shipped one:
/// `CARTO_HEAP_LABELS=1 trunk build …`.
#[cfg(heap_labels)]
pub mod labels {
    //! Without a thread-local: rustc's LLVM crashed compiling one into this
    //! allocator for wasm (a SIGSEGV in `WebAssemblyMCInstLower`, nightly of
    //! 2026-10). A thread is told apart by where its stack is instead: the page's
    //! own shadow stack lies below `__heap_base`, and each Web Worker's is a
    //! block inside the heap, so a worker is found by the slot whose recorded
    //! stack address is the nearest one above the current frame.

    use std::sync::atomic::{
        AtomicPtr, AtomicU8, AtomicUsize, Ordering::Acquire, Ordering::Relaxed, Ordering::Release,
    };

    pub const SLOTS: usize = 64;
    /// Each slot's name, claimed by a compare-and-swap on the pointer, then its
    /// length published. Not a `OnceLock`: a contended one waits with
    /// `Atomics.wait`, which the page's thread may not call, and that killed the
    /// labelled build on its first race with a worker.
    static NAME_PTR: [AtomicPtr<u8>; SLOTS] =
        [const { AtomicPtr::new(std::ptr::null_mut()) }; SLOTS];
    static NAME_LEN: [AtomicUsize; SLOTS] = [const { AtomicUsize::new(0) }; SLOTS];
    pub(super) static LIVE_BY: [AtomicUsize; SLOTS] = [const { AtomicUsize::new(0) }; SLOTS];
    /// The most each label held since the last [`top_peaks`]: a burst between two
    /// reports is the one that runs the heap out, and the live figure misses it.
    pub(super) static PEAK_BY: [AtomicUsize; SLOTS] = [const { AtomicUsize::new(0) }; SLOTS];

    /// The page's thread's label.
    static MAIN: AtomicU8 = AtomicU8::new(0);
    /// Per worker: a stack address recorded when its label was set, and the label.
    const WORKERS: usize = 32;
    static PROBE: [AtomicUsize; WORKERS] = [const { AtomicUsize::new(0) }; WORKERS];
    static LABEL: [AtomicU8; WORKERS] = [const { AtomicU8::new(0) }; WORKERS];
    /// How far below its recorded address a worker's frames may still be.
    const STACK_REACH: usize = 8 << 20;

    #[cfg(target_arch = "wasm32")]
    fn heap_base() -> usize {
        unsafe extern "C" {
            static __heap_base: u8;
        }
        core::ptr::addr_of!(__heap_base) as usize
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn heap_base() -> usize {
        usize::MAX
    }

    /// An address in the caller's stack frame.
    #[inline(never)]
    fn here() -> usize {
        let marker = 0u8;
        core::hint::black_box(core::ptr::addr_of!(marker)) as usize
    }

    /// The worker slot this stack belongs to, if one was recorded.
    fn slot_of(at: usize) -> Option<usize> {
        let mut best: Option<(usize, usize)> = None;
        for (i, p) in PROBE.iter().enumerate() {
            let p = p.load(Relaxed);
            if p != 0 && p >= at && p - at < STACK_REACH && best.is_none_or(|(bp, _)| p < bp) {
                best = Some((p, i));
            }
        }
        best.map(|(_, i)| i)
    }

    /// The label allocations made here are charged to.
    pub(super) fn current() -> u8 {
        let at = here();
        if at < heap_base() {
            return MAIN.load(Relaxed);
        }
        slot_of(at).map_or_else(|| id("worker (unlabelled)"), |i| LABEL[i].load(Relaxed))
    }

    /// The slot for `name`, registering it on first use. Allocation-free, so it
    /// is safe to call from anywhere, the allocator's callers included. Past
    /// [`SLOTS`] names, everything new shares the last slot.
    pub fn id(name: &'static str) -> u8 {
        // A published length of 0 means "not yet", so no name may be empty.
        let name = if name.is_empty() { "unnamed" } else { name };
        if name_at(0).is_none() {
            claim(0, "main");
        }
        for i in 0..SLOTS {
            if name_at(i).is_none() && !claim(i, name) {
                // Lost the claim: the winner publishes its length right after.
                while NAME_LEN[i].load(Acquire) == 0 {
                    std::hint::spin_loop();
                }
            }
            if name_at(i) == Some(name) {
                return i as u8;
            }
        }
        (SLOTS - 1) as u8
    }

    /// Take slot `i` for `name` if nobody has; whether this call took it.
    fn claim(i: usize, name: &'static str) -> bool {
        let won = NAME_PTR[i]
            .compare_exchange(
                std::ptr::null_mut(),
                name.as_ptr().cast_mut(),
                Acquire,
                Relaxed,
            )
            .is_ok();
        if won {
            NAME_LEN[i].store(name.len(), Release);
        }
        won
    }

    /// Slot `i`'s name, once it is fully published.
    fn name_at(i: usize) -> Option<&'static str> {
        let len = NAME_LEN[i].load(Acquire);
        if len == 0 {
            return None;
        }
        let ptr = NAME_PTR[i].load(Relaxed);
        // SAFETY: `claim` stores the pointer of a `&'static str` and then, with
        // release ordering, its length; a non-zero length read with acquire
        // therefore sees that pointer, and the bytes live forever.
        Some(unsafe { std::str::from_utf8_unchecked(std::slice::from_raw_parts(ptr, len)) })
    }

    /// Charge this thread's allocations to `name` until the next call.
    pub fn set(name: &'static str) {
        let id = id(name);
        let at = here();
        if at < heap_base() {
            MAIN.store(id, Relaxed);
            return;
        }
        let slot = slot_of(at).or_else(|| {
            PROBE
                .iter()
                .position(|p| p.compare_exchange(0, at + 4096, Relaxed, Relaxed).is_ok())
        });
        if let Some(i) = slot {
            LABEL[i].store(id, Relaxed);
        }
    }

    /// Run `f` with its allocations charged to `name`, then charge to `after`.
    pub fn scoped<R>(name: &'static str, f: impl FnOnce() -> R) -> R {
        set(name);
        let out = f();
        set("worker (idle)");
        out
    }

    /// The `n` labels whose peak since the last call was highest, as
    /// `label=MB`, and start the next window.
    pub fn top_peaks(n: usize) -> String {
        let mut rows: Vec<(&'static str, usize)> = (0..SLOTS)
            .filter_map(|i| {
                let peak = PEAK_BY[i].swap(LIVE_BY[i].load(Relaxed), Relaxed);
                name_at(i).map(|name| (name, peak))
            })
            .filter(|(_, b)| *b >= 1 << 20)
            .collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1));
        rows.iter()
            .take(n)
            .map(|(name, b)| format!("{name}={}MB", b >> 20))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The `n` labels holding the most, as `label=MB`.
    pub fn top(n: usize) -> String {
        let mut rows: Vec<(&'static str, usize)> = (0..SLOTS)
            .filter_map(|i| name_at(i).map(|name| (name, LIVE_BY[i].load(Relaxed))))
            .filter(|(_, b)| *b >= 1 << 20)
            .collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1));
        rows.iter()
            .take(n)
            .map(|(name, b)| format!("{name}={}MB", b >> 20))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// The header in front of a labelled allocation: at least 16 bytes, and the
/// allocation's own alignment if that is larger, so the pointer handed out keeps
/// it.
#[cfg(all(target_arch = "wasm32", heap_labels))]
fn header(layout: Layout) -> usize {
    layout.align().max(16)
}
