//! Per-system CPU time, accumulated in process and read per window.
//!
//! # Why this rather than a trace file
//!
//! `bevy/trace` puts a span around every system run, and the obvious way to read
//! those is a chrome trace. That was tried and is the wrong tool here: one `city`
//! scenario wrote **2.5 GB** (4.8 GB before the filter was narrowed), which then
//! has to be aggregated somewhere else, and a whole-run total cannot answer the
//! only question worth asking — what does a *settled* frame cost, as against a
//! frame with tiles still arriving. Splitting that by hand found
//! `update_map_surfaces` at 170 ms/frame while streaming and absent once settled,
//! which is a fact the file could not state and a reader could easily misread.
//!
//! So the counting happens here, the totals are snapshotted when a perf window
//! opens and closes, and the difference is that window's cost. It is the same
//! shape as every other column in `perf::Window` — `workers::stats::snapshot` and
//! the diagnostics store are read exactly this way — and it produces no artefact
//! at all.
//!
//! # What a number here is worth
//!
//! Bevy's span is `info_span!("system", name = …)`, so the system's identity is a
//! **field** and every span is called `system`. Reading the span's own name gives
//! one row called `system` covering three million calls, which is what the first
//! attempt at this produced.
//!
//! Time is accumulated between `on_enter` and `on_exit`, so it is the span's busy
//! time rather than its wall time — a system that yields does not bill the gap to
//! itself. Bevy runs systems across a thread pool, so this is CPU time summed over
//! threads and **not** a share of the frame: two systems each billing 4 ms may
//! have run at the same moment on different cores.
//!
//! One caveat the reader owes themselves: this workspace builds its own crates at
//! `opt-level = 1` and its dependencies at `3`, so a row from `cartopolis` and a
//! row from `bevy_render` are not measured on equal terms.

use crate::platform::sync::{LazyLock, Mutex};
use std::collections::HashMap;
use std::time::Instant;

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id};
use tracing::Subscriber;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

/// Sharded so the pool threads are not all queueing on one lock. A `city` run
/// exits ~3 M spans, and a single mutex for all of them would be measuring the
/// instrument.
const SHARDS: usize = 32;

/// Nanoseconds and call count for one system.
#[derive(Clone, Copy, Default, Debug)]
pub struct Cost {
    pub nanos: u128,
    pub calls: u64,
}

impl Cost {
    /// This total minus an earlier one — what happened between two snapshots.
    ///
    /// Saturating, because a snapshot taken on another thread mid-update can
    /// legitimately be a few nanoseconds ahead of this one.
    #[must_use]
    pub fn since(self, then: Self) -> Self {
        Self {
            nanos: self.nanos.saturating_sub(then.nanos),
            calls: self.calls.saturating_sub(then.calls),
        }
    }
}

type Table = HashMap<Box<str>, Cost>;

static ACC: LazyLock<Vec<Mutex<Table>>> =
    LazyLock::new(|| (0..SHARDS).map(|_| Mutex::new(Table::new())).collect());

fn shard_of(name: &str) -> usize {
    // FNV-1a, so the shard is stable across runs and needs no hasher state.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in name.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    (h as usize) % SHARDS
}

fn record(name: &str, nanos: u128) {
    let mut table = match ACC[shard_of(name)].lock() {
        Ok(t) => t,
        // Poisoning means some other thread panicked while holding it; the totals
        // are still meaningful and dropping a measurement is worse than the risk.
        Err(e) => e.into_inner(),
    };
    let slot = table.entry(name.into()).or_default();
    slot.nanos += nanos;
    slot.calls += 1;
}

/// Every system's total so far, merged across shards.
#[must_use]
pub fn snapshot() -> Table {
    let mut out = Table::new();
    for shard in ACC.iter() {
        let table = match shard.lock() {
            Ok(t) => t,
            Err(e) => e.into_inner(),
        };
        for (name, cost) in table.iter() {
            let slot = out.entry(name.clone()).or_default();
            slot.nanos += cost.nanos;
            slot.calls += cost.calls;
        }
    }
    out
}

/// `later - earlier`, keeping only systems that actually ran, sorted by cost.
#[must_use]
pub fn delta(earlier: &Table, later: &Table) -> Vec<(String, Cost)> {
    let mut rows: Vec<(String, Cost)> = later
        .iter()
        .map(|(name, cost)| {
            let then = earlier.get(name).copied().unwrap_or_default();
            (name.to_string(), cost.since(then))
        })
        .filter(|(_, c)| c.calls > 0)
        .collect();
    rows.sort_by(|a, b| b.1.nanos.cmp(&a.1.nanos));
    rows
}

/// Pulls `name` off bevy's `info_span!("system", name = …)`.
#[derive(Default)]
struct NameVisitor(Option<String>);

impl Visit for NameVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "name" && self.0.is_none() {
            self.0 = Some(format!("{value:?}").trim_matches('"').to_string());
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "name" {
            self.0 = Some(value.to_string());
        }
    }
}

/// What the span carries between `on_new_span` and `on_exit`.
struct Tracked {
    name: Box<str>,
    entered: Option<Instant>,
}

/// The layer. Installed under the `perf` feature only.
pub struct SystemTimings;

impl<S> Layer<S> for SystemTimings
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        // Only bevy's system spans. Everything else in `bevy_ecs` — the executor
        // wakes, the command flushes — is volume without a name to hang it on.
        if attrs.metadata().name() != "system" {
            return;
        }
        let mut visitor = NameVisitor::default();
        attrs.record(&mut visitor);
        let Some(name) = visitor.0 else {
            return;
        };
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(Tracked {
                name: name.into_boxed_str(),
                entered: None,
            });
        }
    }

    fn on_enter(&self, id: &Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            if let Some(tracked) = span.extensions_mut().get_mut::<Tracked>() {
                tracked.entered = Some(Instant::now());
            }
        }
    }

    fn on_exit(&self, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        let mut ext = span.extensions_mut();
        let Some(tracked) = ext.get_mut::<Tracked>() else {
            return;
        };
        let Some(entered) = tracked.entered.take() else {
            return;
        };
        let nanos = entered.elapsed().as_nanos();
        let name = tracked.name.clone();
        drop(ext);
        record(&name, nanos);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_delta_is_what_happened_between_two_snapshots() {
        let mut before = Table::new();
        before.insert(
            "a".into(),
            Cost {
                nanos: 100,
                calls: 2,
            },
        );
        let mut after = Table::new();
        after.insert(
            "a".into(),
            Cost {
                nanos: 250,
                calls: 5,
            },
        );
        // A system that only appears in the later snapshot is a system that first
        // ran inside the window, which is ordinary — not a missing baseline.
        after.insert(
            "b".into(),
            Cost {
                nanos: 70,
                calls: 1,
            },
        );

        let rows = delta(&before, &after);
        assert_eq!(rows.len(), 2, "both systems ran during the window");
        assert_eq!(rows[0].0, "a", "sorted by cost, dearest first");
        assert_eq!(rows[0].1.nanos, 150);
        assert_eq!(rows[0].1.calls, 3);
        assert_eq!(rows[1].1.nanos, 70);
    }

    /// A system that did not run inside the window must not appear at all — a row
    /// of zeroes reads as "measured and free" rather than "not measured".
    #[test]
    fn a_system_that_did_not_run_is_absent_rather_than_zero() {
        let mut both = Table::new();
        both.insert(
            "idle".into(),
            Cost {
                nanos: 900,
                calls: 4,
            },
        );
        assert!(delta(&both, &both.clone()).is_empty());
    }

    #[test]
    fn shards_are_stable_and_in_range() {
        for name in ["a", "bevy_render::renderer::render_system", ""] {
            assert_eq!(shard_of(name), shard_of(name));
            assert!(shard_of(name) < SHARDS);
        }
    }
}
