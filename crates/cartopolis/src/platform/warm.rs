//! What a streamer keeps in RAM after a cell leaves the ring.
//!
//! # The gap this fills
//!
//! Two caches already sit in front of the streamers and both hold the **source**
//! bytes: `tile_source`'s 32 MiB in-memory LRU and the 512 MiB disk tree
//! `utils::cache` bounds. Everything *derived* from those bytes — the rasterised
//! ground texture and its mip chain, the tessellated surfaces, the shore field,
//! the merged building meshes — is dropped the moment a cell is retired, and
//! rebuilt from scratch when the camera comes back.
//!
//! `docs/notes/tile-raster-cost.md` measures what that is worth: a fully warm
//! *disk* cache buys **2 s of a 27.8 s** fill over Groningen, because the time is
//! scan conversion (≈48 ms per 256² tile, ≈68 ms at 512², plus ≈19 ms of shore
//! field per cell) and not fetching. Caching more source bytes cannot move that
//! number. Caching the finished form is the only thing that can.
//!
//! # What this is not
//!
//! It is **not** a way to hold GPU resources alive. Everything in here is
//! CPU-side data waiting to be turned into an asset again, so it costs system
//! memory and nothing else — `upload_meter::resident_bytes` and the ceiling
//! `load_control` steers toward are untouched by it, which is what makes it safe
//! to switch on beside the device-loss guards rather than against them.
//!
//! Keeping the `Handle<Mesh>`/`Handle<Image>` instead would be faster still (no
//! upload at all on a return) and is deliberately not what this does: those bytes
//! are exactly the quantity that lost an Adreno 840 a device on 2026-08-19 while
//! every rate guard was in force.
//!
//! # Why a byte bound and not an entry count
//!
//! The same rule `docs/notes/upload-bursts.md` states for the upload budgets, and
//! for the same reason: the things stored differ in size by orders of magnitude.
//! A z14 ground tile is a fixed 1,365 kB mip chain, while a tessellated cell over
//! open water is a few kilobytes and one over a city centre is megabytes. A cap in
//! entries would be a cap on nothing.
//!
//! # Why the caller states its own ceiling
//!
//! Same shape as [`stream::Policy`](super::stream::Policy): the number is stated
//! at the construction site, beside the reason it is that number, rather than
//! centrally where nobody can see what it is a budget *for*.
//! [`crate::quality::Budget::warm_bytes`] is the tier's whole allowance and each
//! streamer takes a stated share of it, so a phone is never asked to hold a
//! desktop's working set.

use std::collections::HashMap;
use std::hash::Hash;

/// How much system memory one stored value occupies.
///
/// Implemented by the caller because only it knows what its value owns — a `Vec`
/// of meshes has to be walked, and `size_of` would report the handful of bytes
/// its headers take. An implementation that under-reports does not corrupt
/// anything; it just makes the ceiling mean less than it says, which is why the
/// two in-tree implementations count every heap buffer they reach.
pub trait WarmSize {
    fn warm_bytes(&self) -> usize;
}

/// A byte-bounded, least-recently-used store of finished cell data.
///
/// Owned by the streamer's own Bevy resource rather than being a process-global
/// like [`tile_source`](crate::systems::tile_source): every access is from a
/// system on the main thread, so there is no lock to take and no reason to make
/// one streamer's working set evict another's.
pub struct WarmStore<K, V> {
    cap: usize,
    bytes: usize,
    entries: HashMap<K, V>,
    /// Recency, least-recently-used first. A `Vec` for the same reason
    /// `tile_source` uses one: the store holds hundreds of entries, not
    /// millions, so a linear scan over small keys beats the bookkeeping an
    /// intrusive list needs.
    order: Vec<K>,
    /// The generation these entries belong to — a `MapStream::fetch_generation`
    /// or `anchor_generation`, whichever the owner re-tears on.
    ///
    /// Holding it here rather than in the key is what makes a stale generation
    /// *free* to discard: the alternative, a key that carries its generation,
    /// keeps the old entries resident and evicts live ones to make room for
    /// them.
    generation: u32,
    hits: u32,
}

impl<K: Eq + Hash + Clone, V: WarmSize> WarmStore<K, V> {
    /// A store bounded at `cap` bytes. `cap == 0` disables it entirely — every
    /// [`put`](Self::put) is dropped on the floor and every [`take`](Self::take)
    /// misses, which is the phone tier's setting and the shot harness's.
    pub fn new(cap: usize) -> Self {
        Self {
            cap,
            bytes: 0,
            entries: HashMap::new(),
            order: Vec::new(),
            generation: 0,
            hits: 0,
        }
    }

    /// Reclaim a cell's finished form, if it is still held.
    ///
    /// **Removes rather than borrows**, and that is the honest shape: the value
    /// is going back into use, so leaving a copy behind would mean holding the
    /// same bytes twice — once in the store and once in the meshes built from
    /// them. A cell that leaves the ring again is [`put`](Self::put) back.
    pub fn take(&mut self, key: &K) -> Option<V> {
        match self.entries.remove(key) {
            Some(value) => {
                self.bytes = self.bytes.saturating_sub(value.warm_bytes());
                self.order.retain(|k| k != key);
                self.hits += 1;
                Some(value)
            }
            None => None,
        }
    }

    /// Is this key held, without counting the question as a hit or a miss?
    ///
    /// For a caller that has to decide *whether* to take before it has what
    /// taking needs — `lod22` picks the nearest held cell out of its whole ring
    /// and then takes that one, and asking after every cell through
    /// [`take`](Self::take) once per cell would count a ring's worth of failed
    /// lookups against the hit tally every frame and make it useless.
    pub fn holds(&self, key: &K) -> bool {
        self.entries.contains_key(key)
    }

    /// Keep a retired cell's finished form, evicting the least recently used
    /// entries until the store fits its ceiling again.
    ///
    /// A value larger than the whole ceiling is **not** stored — it would empty
    /// the store to hold one entry and then be evicted by the next `put`, so the
    /// store would thrash while reporting itself full.
    pub fn put(&mut self, key: K, value: V) {
        if self.cap == 0 {
            return;
        }
        let size = value.warm_bytes();
        if size > self.cap {
            return;
        }
        // Replacing an existing entry has to release the old bytes first, or the
        // tally drifts upward until the store evicts everything to satisfy a
        // ceiling it is no longer over.
        if let Some(old) = self.entries.remove(&key) {
            self.bytes = self.bytes.saturating_sub(old.warm_bytes());
            self.order.retain(|k| k != &key);
        }
        while self.bytes + size > self.cap && !self.order.is_empty() {
            let oldest = self.order.remove(0);
            if let Some(dropped) = self.entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(dropped.warm_bytes());
            }
        }
        self.bytes += size;
        self.order.push(key.clone());
        self.entries.insert(key, value);
    }

    /// Discard everything held for an older generation.
    ///
    /// Called with the owner's current generation every pass. A re-anchor moves
    /// every cell's world position and a source switch changes what its pixels
    /// are, so in both cases what is held is not merely stale but wrong — and
    /// serving it would draw the previous city's ground under the new one.
    pub fn set_generation(&mut self, generation: u32) {
        if self.generation != generation {
            self.generation = generation;
            self.clear();
        }
    }

    /// Drop everything, keeping the hit tally (which is a per-run instrument,
    /// not a per-generation one).
    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.bytes = 0;
    }

    /// Whether this store is switched off entirely (a zero ceiling).
    ///
    /// Exposed so a caller can skip *producing* the value as well as storing it:
    /// `map_stream` has to clone a 1.4 MB mip chain to keep one, and on a rung
    /// that holds nothing that copy would be pure waste.
    pub fn disabled(&self) -> bool {
        self.cap == 0
    }

    /// Bytes held, by the values' own reckoning.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Entries held.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Cells served from memory that would otherwise have been rebuilt.
    ///
    /// Reported in `--dump-state` so a capture can state whether a run was warm.
    /// There is deliberately no companion miss count: nothing reads one, and an
    /// instrument with no reader is dead weight the `unused` lint is right about.
    pub fn hits(&self) -> u32 {
        self.hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A value whose size is whatever the test says it is.
    struct Sized(usize);
    impl WarmSize for Sized {
        fn warm_bytes(&self) -> usize {
            self.0
        }
    }

    fn store(cap: usize) -> WarmStore<u32, Sized> {
        WarmStore::new(cap)
    }

    /// The point of the type: a cell put back is handed out again without being
    /// rebuilt.
    #[test]
    fn a_stored_cell_comes_back() {
        let mut s = store(1000);
        s.put(1, Sized(100));
        assert!(s.take(&1).is_some());
        assert_eq!(s.hits(), 1);
        // …and only once, because taking it is handing it over.
        assert!(s.take(&1).is_none());
    }

    /// The ceiling is in bytes, so one large entry evicts several small ones —
    /// which is the whole reason it is not a count.
    #[test]
    fn the_ceiling_is_bytes_not_entries() {
        let mut s = store(1000);
        for i in 0..10 {
            s.put(i, Sized(100));
        }
        assert_eq!(s.len(), 10);
        assert_eq!(s.bytes(), 1000);
        s.put(99, Sized(500));
        assert!(s.bytes() <= 1000, "over the ceiling at {}", s.bytes());
        assert!(s.take(&99).is_some(), "the new entry is the one held");
    }

    /// Least recently used goes first, and a `take` counts as a use — so a cell
    /// the camera keeps returning to outlives one it passed through once.
    #[test]
    fn eviction_is_least_recently_used() {
        let mut s = store(300);
        s.put(1, Sized(100));
        s.put(2, Sized(100));
        s.put(3, Sized(100));
        // Touch 1, so 2 is now the oldest.
        let one = s.take(&1).expect("held");
        s.put(1, one);
        s.put(4, Sized(100));
        assert!(s.take(&2).is_none(), "the least recently used survived");
        assert!(s.take(&1).is_some());
        assert!(s.take(&3).is_some());
        assert!(s.take(&4).is_some());
    }

    /// Re-putting a key must release the bytes the old value held, or the tally
    /// climbs until the store evicts everything to meet a ceiling it is under.
    #[test]
    fn replacing_a_key_does_not_leak_bytes() {
        let mut s = store(1000);
        for _ in 0..20 {
            s.put(7, Sized(100));
        }
        assert_eq!(s.len(), 1);
        assert_eq!(s.bytes(), 100);
    }

    /// A cap of zero is off, not "one entry": a phone must be able to opt out
    /// without the call sites branching.
    #[test]
    fn a_zero_cap_stores_nothing() {
        let mut s = store(0);
        s.put(1, Sized(1));
        assert!(s.is_empty());
        assert!(s.take(&1).is_none());
    }

    /// A value bigger than the whole store is refused rather than emptying it —
    /// otherwise every such put clears the store and is evicted by the next one.
    #[test]
    fn an_oversized_value_is_refused_without_emptying_the_store() {
        let mut s = store(1000);
        s.put(1, Sized(400));
        s.put(2, Sized(4000));
        assert!(s.take(&1).is_some(), "the store was emptied for a refusal");
        assert!(s.take(&2).is_none());
    }

    /// A re-anchor moves every cell's world position, so what is held is wrong
    /// rather than merely old.
    #[test]
    fn a_new_generation_discards_everything() {
        let mut s = store(1000);
        s.set_generation(3);
        s.put(1, Sized(100));
        s.set_generation(3);
        assert!(!s.is_empty(), "the same generation must not clear");
        s.set_generation(4);
        assert!(s.is_empty());
        assert_eq!(s.bytes(), 0);
    }
}
