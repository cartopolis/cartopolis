//! **Which streamers are still working, from a list nobody has to remember to
//! extend.**
//!
//! The `--shot` settle test and the MCP `state` tool both ask "is the world
//! finished, and if not, what is it still doing". They used to answer it from a
//! hand-written `SystemParam` in `shot_harness` naming each streamer — and the
//! list fell behind: `water_levels`, `dikes`, `building_facts` and the data
//! layers' two streams were never on it, so a capture could declare the map
//! settled while their cells were still in the air. **"Every" is load-bearing
//! and was learned the hard way** (`shot_harness::StreamWatch` has the history):
//! the test used to watch only the two tile streams, and a fix elsewhere turned
//! each README capture into a half-built city because the accidental wait had
//! been covering for the streamers the list did not name.
//!
//! So the list is now built where the resources are: a streaming resource
//! implements [`StreamBusy`], and the plugin that initialises it calls
//! [`WatchStreamExt::watch_stream`] on the next line. That adds one tiny system
//! per resource, in [`StreamReport`], which writes the resource's answer into
//! the [`StreamRegistry`]; the harness reads the registry. A new streamer that
//! is initialised without being watched is now a line missing beside its own
//! `init_resource`, not a line missing in a file three directories away.
//!
//! The report runs in [`ClientPhase::Present`](crate::phase::ClientPhase), and
//! `drive_capture` is ordered after [`StreamReport`], so the settle test reads
//! the state every streamer was left in by this frame's `Stream` phase — the
//! same state it read when it held the resources directly.

use std::sync::atomic::{AtomicBool, Ordering};

use bevy::prelude::*;

use crate::phase::ClientPhase;

/// A resource whose work the settle test must wait for.
///
/// Not only streamers: the relief backlog, the route solve and the live
/// weather sample hold a capture the same way and are reported beside them.
pub trait StreamBusy: Resource {
    /// What the settle log, `--dump-state`'s `busy` and the MCP `state` tool
    /// call it. Unique across the registry — [`WatchStreamExt::watch_stream`]
    /// panics at startup on a duplicate.
    const NAME: &'static str;

    /// Is it still working — fetching, meshing, retiring or waiting on an
    /// answer it has asked for? Must be `false` when nothing was asked for, or a
    /// layer that is switched off would hold every capture to `--wait`.
    fn stream_busy(&self) -> bool;
}

/// The systems that copy each watched resource's answer into the
/// [`StreamRegistry`]. Anything that reads the registry and wants this frame's
/// answer runs `.after(StreamReport)`.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StreamReport;

/// Every watched resource by name, with its answer as of the last
/// [`StreamReport`].
///
/// The flags are atomics so the reporters take the registry by `Res` and run in
/// parallel with each other; each writes only its own entry.
#[derive(Resource, Default)]
pub struct StreamRegistry {
    entries: Vec<(&'static str, AtomicBool)>,
}

impl StreamRegistry {
    /// Which watched resources are still working, by name, in registration order.
    ///
    /// **Names, not a single bool.** When a capture gives up at `--wait`, *which*
    /// streamer was still going is the entire diagnosis, and a bare "not settled"
    /// sends you back to re-run with a longer cap just to learn it.
    #[must_use]
    pub fn busy(&self) -> Vec<&'static str> {
        self.entries
            .iter()
            .filter(|(_, busy)| busy.load(Ordering::Relaxed))
            .map(|(name, _)| *name)
            .collect()
    }

    /// Every registered name, in registration order.
    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.entries.iter().map(|(name, _)| *name)
    }

    fn register(&mut self, name: &'static str) {
        assert!(
            self.names().all(|n| n != name),
            "stream `{name}` is watched twice — a second registration would \
             report it twice, and two resources sharing a name cannot be told \
             apart in the settle log"
        );
        self.entries.push((name, AtomicBool::new(false)));
    }

    fn set(&self, name: &'static str, busy: bool) {
        if let Some((_, flag)) = self.entries.iter().find(|(n, _)| *n == name) {
            flag.store(busy, Ordering::Relaxed);
        }
    }
}

/// `app.watch_stream::<R>()`: put `R` on the settle test's list.
pub trait WatchStreamExt {
    fn watch_stream<R: StreamBusy>(&mut self) -> &mut Self;
}

impl WatchStreamExt for App {
    fn watch_stream<R: StreamBusy>(&mut self) -> &mut Self {
        if !self.world().contains_resource::<StreamRegistry>() {
            self.init_resource::<StreamRegistry>();
            self.configure_sets(Update, StreamReport.in_set(ClientPhase::Present));
        }
        self.world_mut()
            .resource_mut::<StreamRegistry>()
            .register(R::NAME);
        self.add_systems(Update, report::<R>.in_set(StreamReport))
    }
}

/// Copy one resource's answer into the registry. `Option`, so a resource that a
/// launch does not initialise reads as quiet rather than failing the system.
fn report<R: StreamBusy>(res: Option<Res<R>>, registry: Res<StreamRegistry>) {
    registry.set(R::NAME, res.is_some_and(|r| r.stream_busy()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Resource, Default)]
    struct Quiet;
    impl StreamBusy for Quiet {
        const NAME: &'static str = "quiet";
        fn stream_busy(&self) -> bool {
            false
        }
    }

    #[derive(Resource, Default)]
    struct Working;
    impl StreamBusy for Working {
        const NAME: &'static str = "working";
        fn stream_busy(&self) -> bool {
            true
        }
    }

    #[derive(Resource, Default)]
    struct AlsoQuiet;
    impl StreamBusy for AlsoQuiet {
        const NAME: &'static str = "quiet";
        fn stream_busy(&self) -> bool {
            false
        }
    }

    #[test]
    fn a_watched_resource_is_reported_by_name() {
        let mut app = App::new();
        app.init_resource::<Quiet>()
            .init_resource::<Working>()
            .watch_stream::<Quiet>()
            .watch_stream::<Working>();
        app.update();
        let registry = app.world().resource::<StreamRegistry>();
        assert_eq!(registry.names().collect::<Vec<_>>(), ["quiet", "working"]);
        assert_eq!(registry.busy(), ["working"]);
    }

    /// A resource the launch never initialised is quiet, not a failed system:
    /// a registered-but-absent streamer must not hold a capture to `--wait`.
    #[test]
    fn an_absent_resource_reads_as_quiet() {
        let mut app = App::new();
        app.watch_stream::<Working>();
        app.update();
        assert!(app.world().resource::<StreamRegistry>().busy().is_empty());
    }

    /// The names are the diagnosis a timed-out capture hands back, so two
    /// resources may not share one.
    #[test]
    #[should_panic(expected = "watched twice")]
    fn a_name_can_only_be_watched_once() {
        let mut app = App::new();
        app.watch_stream::<Quiet>().watch_stream::<AlsoQuiet>();
    }
}
