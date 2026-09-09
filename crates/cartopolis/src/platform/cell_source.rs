//! The per-cell fetch every **additive** data layer was writing out by hand.
//!
//! Six modules load something keyed by a map cell straight into the job that
//! needs it, rather than through a stream of their own — the BGT's street
//! objects and its paving, the crop parcels, the LoD3 models, the navigation
//! marks and the level crossings. Each is a decoration whose absence is simply
//! the map as it was before that layer existed, and each contained the same
//! eight steps in the same order:
//!
//! 1. a **gate**: is this cell even a candidate (covered by our host, inside the
//!    register's extent)?
//! 2. a **cache key**, hand-written as a `format!` with a version segment in it;
//! 3. a **cache read**, parsed and returned if it hits;
//! 4. a **breaker** check, for the sources owned by somebody else;
//! 5. a **request**, with or without [`http::OPTIONAL_LAYER_TIMEOUT`];
//! 6. a failure arm answering **empty rather than erroring**, because every
//!    consumer's fallback is the generated layer it already had;
//! 7. a **store** of what came back;
//! 8. a **parse**.
//!
//! Written out six times they drifted, and the drifts were omissions rather than
//! differences: `lod3` had no breaker at all, and the rule that the cache is read
//! *before* the breaker — so an outage cannot blank cells that were already
//! complete when it started — was stated and followed in two of the six.
//! `docs/notes/optional-layers-on-the-critical-path.md` records that rule for the
//! pair it was written for; it is now the only version of it there is.
//!
//! [`CellSource`] is that sequence, once, as data. What a cell *is* and what its
//! bytes mean stay with the caller: the descriptor holds four function pointers
//! and the policy, and nothing here knows what a tree or a crossing is.
//!
//! # What this deliberately does not absorb
//!
//! **The streamer-driven cells** — the LoD2.2 shells, the BAG attributes, the
//! far buildings, the flood defences, the tall structures, the street detail, the
//! marker layers, the ground tiles, the globe quadtree. Those already share
//! [`CellStream`](super::stream::CellStream), which is the same idea for the
//! other shape, and their fetches answer `Result` rather than `Option` on
//! purpose: a failure there feeds an attempt count, a backoff and a cooldown, and
//! flattening it to "empty, as before" would silently delete the retry policy.
//! An additive layer wants the failure forgotten; a streamed one wants it
//! remembered. That is a real difference and it keeps the two seams apart.
//!
//! **`systems::municipal`**, for a plainer reason: its key and its URL are
//! functions of *which register and which source*, and every field here is a bare
//! function pointer so the whole descriptor can be a `const`. Carrying a context
//! would mean a boxed closure per source, which costs more than the twenty lines
//! it saves. It is also the module with the better idea about cache keys — see
//! its reader fingerprint — and that idea is not smuggled in here.
//!
//! # The cache keys are preserved exactly, and that is deliberate
//!
//! It is tempting to make the key generic too: one scheme, one version segment,
//! one place to bump. It would also invalidate every cell every installed client
//! holds on disk, on the day it shipped, for no gain to the person running it. So
//! `key` stays a caller-supplied function returning exactly the string that
//! module returned before, hand-bumped `v1`/`v2` included.
//!
//! # Two shapes, because one step varies
//!
//! Most sources are one `GET`, and [`CellSource::load`] is the whole of it. Two
//! are not: `nav_marks` pages two collections and `crossings` normalises a third
//! party's GeoJSON before storing. What varies is only step 5, so
//! [`CellSource::load_with`] takes that step as a future and keeps the other
//! seven — the gate, cache, breaker and store rules are then one rule for all six
//! rather than the same rule written six times.

use std::time::Duration;

use super::{http, storage};

/// One additive per-cell dataset: where it lives, when to ask for it, and how to
/// read it.
///
/// Declared as a `const` or `static` beside the module that parses it. Every
/// field is a plain function pointer so the whole thing can be a constant — a
/// closure would need an allocation and a `Box<dyn Fn>` per source.
pub struct CellSource<T: 'static> {
    /// Short name for the log line, so two sources' messages are tellable apart.
    pub label: &'static str,
    /// Whether this cell is a candidate at all. `None` means "always ask".
    ///
    /// This is the coverage manifest, the register's extent, or a cheap read of
    /// the undecoded tile body — whatever the caller can answer without a
    /// request. It runs **first**, so a cell outside the data costs neither a
    /// request nor a storage lookup.
    pub gate: Option<fn(i64, i64) -> bool>,
    /// The storage key, verbatim as that module has always written it.
    pub key: fn(i64, i64) -> String,
    /// Where to ask, when the cache misses.
    ///
    /// `None` for a source with no single URL to give — `nav_marks` pages two
    /// collections and goes through [`load_with`](Self::load_with), which never
    /// consults this. An `Option` rather than a `fn` that panics: a shipped
    /// Android build must not be able to reach an `unreachable!`, and "this
    /// source has no one URL" is a fact about the source that the type can hold.
    pub url: Option<fn(i64, i64) -> String>,
    /// Held open for a minute after this *service* fails. `None` for our own
    /// host, whose failure is our problem rather than a third party's.
    pub breaker: Option<&'static http::Breaker>,
    /// `Some(OPTIONAL_LAYER_TIMEOUT)` where the fetch sits on the critical path
    /// of something that matters — see that constant's own note.
    pub timeout: Option<Duration>,
    /// Stored text to the caller's type. `None` for anything unreadable, which is
    /// treated exactly as a miss: the layer is additive, so its absence is the
    /// map as it was before the layer existed.
    pub parse: fn(&str) -> Option<T>,
}

impl<T: Default + 'static> CellSource<T> {
    /// The whole sequence for a source that is one `GET`.
    pub async fn load(&self, cx: i64, cy: i64) -> T {
        self.load_text(cx, cy)
            .await
            .and_then(|text| (self.parse)(&text))
            .unwrap_or_default()
    }

    /// The same sequence with step 5 supplied by the caller — for a source that
    /// pages several collections, or normalises a third party's document before
    /// it is worth storing.
    ///
    /// `fresh` is only awaited on a cache miss that passed both the gate and the
    /// breaker, so a caller may build an expensive request in it freely. It
    /// returns the text to **store**, which for these sources is the compacted
    /// form rather than what came off the wire.
    pub async fn load_with<F>(&self, cx: i64, cy: i64, fresh: F) -> T
    where
        F: std::future::Future<Output = Option<String>>,
    {
        self.load_text_with(cx, cy, fresh)
            .await
            .and_then(|text| (self.parse)(&text))
            .unwrap_or_default()
    }
}

impl<T: 'static> CellSource<T> {
    /// Whether a cache miss on this cell would actually issue a request.
    ///
    /// The gate and the breaker, in the order [`load_text_with`](Self::load_text_with)
    /// applies them — and it is the same call, so this cannot drift from what the
    /// loader does. Exposed because **"no request was issued" is not observable
    /// from the other side of an `async fn` that answers the same empty cell
    /// either way**: `nav_marks` and `crossings` each assert their register's
    /// extent and their service's cooldown through it, which is the only way to
    /// test either without a network.
    ///
    /// The breaker's state is a parameter rather than read from `self.breaker`
    /// so a test can inject one. Reading the descriptor's own would mean tripping
    /// a process-wide static to assert a rule about one cell.
    #[must_use]
    pub fn would_fetch(&self, cx: i64, cy: i64, breaker_open: bool) -> bool {
        self.gate.is_none_or(|gate| gate(cx, cy)) && !breaker_open
    }

    /// Steps 1–7 without the parse, for the one caller that wants the text
    /// itself (`lod3` hands its cell to `lod22`'s parser rather than owning one).
    pub async fn load_text(&self, cx: i64, cy: i64) -> Option<String> {
        // The URL is built *inside* the future, so it is only formatted on a
        // cache miss that passed the gate and the breaker.
        self.load_text_with(cx, cy, async move {
            fetch_one(self.url?(cx, cy), self.timeout).await
        })
        .await
    }

    async fn load_text_with<F>(&self, cx: i64, cy: i64, fresh: F) -> Option<String>
    where
        F: std::future::Future<Output = Option<String>>,
    {
        // The gate runs **first**, before the key is even built: a cell outside
        // the data must cost neither a request nor a storage lookup. It is
        // checked again inside `would_fetch` below, which is not redundant — that
        // is the predicate the modules assert against, and it has to be the same
        // call the loader makes or the assertion means nothing.
        if !self.gate.is_none_or(|gate| gate(cx, cy)) {
            return None;
        }
        let key = (self.key)(cx, cy);
        // The cache is read **before** the breaker is consulted, not after: a
        // cell already on disk costs the service nothing, so an outage must not
        // blank cells that were complete before it started.
        if let Some(cached) = storage::get_string(storage::Namespace::Cache, &key).await {
            return Some(cached);
        }
        let breaker_open = self
            .breaker
            .is_some_and(|breaker| breaker.is_open_at(web_time::Instant::now()));
        if !self.would_fetch(cx, cy, breaker_open) {
            return None;
        }
        let Some(text) = fresh.await else {
            if let Some(breaker) = self.breaker {
                breaker.trip();
                tracing::debug!(source = self.label, cx, cy, "held off after a failure");
            }
            return None;
        };
        // Nothing is written on a failure, deliberately: a poisoned key is never
        // re-fetched, so half a cell cached under one is worse than a cell asked
        // for again.
        storage::put(storage::Namespace::Cache, &key, text.as_bytes()).await;
        Some(text)
    }
}

/// One `GET`, as text, with everything that can go wrong flattened to `None`.
async fn fetch_one(url: String, timeout: Option<Duration>) -> Option<String> {
    let mut request = http::Request::get(url);
    if let Some(timeout) = timeout {
        request = request.timeout(timeout);
    }
    http::fetch_string(request).await.ok()
}

#[cfg(test)]
mod tests {
    use super::CellSource;

    /// A gate that refuses must stop the sequence before the storage lookup, not
    /// merely before the request — `nav_marks` and `crossings` state that as a
    /// rule ("a foreign or dry cell must cost neither a request nor a storage
    /// lookup") and it is now one place rather than two.
    ///
    /// Asserted through the observable consequence, since neither the storage nor
    /// the network is reachable from a unit test: a refused gate answers `None`
    /// without ever calling `url`, which would panic if it were reached.
    #[test]
    fn a_refused_gate_answers_without_asking_anything() {
        const SOURCE: CellSource<String> = CellSource {
            label: "test",
            gate: Some(|_, _| false),
            key: |_, _| unreachable!("the key was built for a gated-out cell"),
            url: None,
            breaker: None,
            timeout: None,
            parse: |text| Some(text.to_string()),
        };
        let answer =
            futures_lite::future::block_on(SOURCE.load_with(0, 0, async { Some(String::new()) }));
        assert_eq!(answer, String::new(), "a gated-out cell must answer empty");
    }

    /// The descriptor has to be constructible as a `const`, or every source pays
    /// a lazy static to hold one.
    #[test]
    fn a_source_is_a_constant() {
        const SOURCE: CellSource<u32> = CellSource {
            label: "test",
            gate: None,
            key: |cx, cy| format!("t/{cx}/{cy}.json"),
            url: Some(|cx, cy| format!("https://example.invalid/{cx}/{cy}.json")),
            breaker: None,
            timeout: None,
            parse: |text| text.trim().parse().ok(),
        };
        assert_eq!((SOURCE.key)(3, 4), "t/3/4.json");
        assert_eq!((SOURCE.parse)("7"), Some(7));
    }
}
