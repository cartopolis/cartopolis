//! How long the map takes to load, measured wherever it is used.
//!
//! **Every load time this project had was a capture's.** `shot_harness` logs
//! `streamer quiet … wall_s` as each layer finishes, which is how the cold and
//! warm starts in `docs/notes/level-of-detail-tile-tree.md` were measured — on
//! the Deck, from a scripted viewpoint. On the phone, where "it sometimes takes
//! too long" is reported, nothing measured anything, so there was no telling a
//! cold cache from a slow service from one layer holding up the rest.
//!
//! So a *load* is measured from the moment any streamer in
//! [`StreamRegistry`] turns busy to the moment the last one goes quiet (and
//! stays quiet for [`QUIET_S`], so a layer that blinks off between two cells does
//! not end it). Each finished load records what started it, how long it took,
//! which layers finished last, and how much of it came over the network rather
//! than out of the device cache. It is logged as a `map load` line and the last
//! [`KEEP`] go into the diagnostics report, which is the one instrument a phone
//! has (`systems::dev::diagnostics`).
//!
//! Wall-clock, never `Time`: virtual time is clamped per frame, so it hides the
//! long frames a slow load is made of (`docs/notes/headless-shots-software-renderer.md`).

use std::collections::VecDeque;

use bevy::prelude::*;

use crate::systems::stream_watch::StreamRegistry;

/// How long every streamer must stay quiet before a load counts as finished.
const QUIET_S: f64 = 1.0;

/// Loads kept for the report.
const KEEP: usize = 8;

/// Loads shorter than this are kept but not logged: walking streams a cell at a
/// time and would otherwise write a line per street.
const LOG_MIN_S: f64 = 1.0;

/// What set a load off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    /// The first load after the app started.
    Launch,
    /// A jump: a search result, a saved place, a link.
    Jump,
    /// Anything else — the camera moving, a layer switched on.
    Moving,
}

impl Cause {
    fn label(self) -> &'static str {
        match self {
            Cause::Launch => "launch",
            Cause::Jump => "jump",
            Cause::Moving => "moving",
        }
    }
}

/// One finished load.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadRecord {
    pub cause: Cause,
    /// Seconds since the app started, when the load began.
    pub began_s: f64,
    /// From the first streamer turning busy to the last one going quiet.
    pub total_s: f64,
    /// Every layer that took part, by when it last went quiet, slowest first.
    pub finished: Vec<(&'static str, f64)>,
    /// A jump arrived before this load had finished, so it was cut short.
    pub cut_short: bool,
    pub requests: u64,
    pub kb: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
}

impl LoadRecord {
    /// One line: `launch 13.4s (lod22 13.4s, surfaces 11.0s, map 3.2s) 61 requests 9.8 MB, cache 812 hits 61 misses`.
    #[must_use]
    pub fn summary(&self) -> String {
        let slowest: Vec<String> = self
            .finished
            .iter()
            .take(4)
            .map(|(name, s)| format!("{name} {s:.1}s"))
            .collect();
        format!(
            "{} {:.1}s{} ({}) {} requests {:.1} MB, cache {} hits {} misses",
            self.cause.label(),
            self.total_s,
            if self.cut_short { " cut short" } else { "" },
            slowest.join(", "),
            self.requests,
            self.kb as f64 / 1024.0,
            self.cache_hits,
            self.cache_misses,
        )
    }
}

/// Network and cache totals at a moment, for the difference a load made.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Counters {
    pub requests: u64,
    pub bytes: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
}

impl Counters {
    fn now() -> Self {
        let fetch = crate::platform::http::stats::snapshot();
        let (cache_hits, cache_misses) = crate::platform::storage::stats::reads()
            .into_iter()
            .fold((0, 0), |(h, m), (_, hits, misses)| (h + hits, m + misses));
        Self {
            requests: fetch.requests,
            bytes: fetch.bytes,
            cache_hits,
            cache_misses,
        }
    }
}

struct Episode {
    cause: Cause,
    began_s: f64,
    base: Counters,
    /// Each layer that has been busy, and when it last went quiet (`None`
    /// while it is still busy).
    layers: Vec<(&'static str, Option<f64>)>,
    /// When everything last went quiet, if it is quiet now.
    quiet_since: Option<f64>,
}

/// The measuring itself, over plain seconds so it can be tested without a clock.
#[derive(Default)]
pub struct LoadClock {
    episode: Option<Episode>,
    launched: bool,
    jump_pending: bool,
}

impl LoadClock {
    /// Note a jump; the next load (or the one in progress, restarted) is its.
    pub fn jumped(&mut self) {
        self.jump_pending = true;
    }

    /// Advance to `now_s` with `busy` the streamers working this frame. Returns
    /// a load that finished on this step, if one did.
    pub fn step(
        &mut self,
        now_s: f64,
        busy: &[&'static str],
        counters: Counters,
    ) -> Option<LoadRecord> {
        let mut done = None;
        if std::mem::take(&mut self.jump_pending) {
            // A jump starts a new load whether or not the old one had finished:
            // what loads after it is the destination, and the old record would
            // otherwise absorb it.
            if let Some(ep) = self.episode.take() {
                done = Some(Self::close(ep, now_s, counters, true));
            }
            self.episode = Some(Self::open(Cause::Jump, now_s, counters));
            self.launched = true;
        }
        if self.episode.is_none() && !busy.is_empty() {
            let cause = if self.launched {
                Cause::Moving
            } else {
                Cause::Launch
            };
            self.launched = true;
            self.episode = Some(Self::open(cause, now_s, counters));
        }
        let ep = self.episode.as_mut()?;
        for name in busy {
            match ep.layers.iter_mut().find(|(n, _)| n == name) {
                Some((_, quiet)) => *quiet = None,
                None => ep.layers.push((*name, None)),
            }
        }
        for (name, quiet) in &mut ep.layers {
            if quiet.is_none() && !busy.contains(&*name) {
                *quiet = Some(now_s);
            }
        }
        if busy.is_empty() {
            let since = *ep.quiet_since.get_or_insert(now_s);
            if now_s - since >= QUIET_S {
                let ep = self.episode.take().expect("an episode is open");
                // Closed at the moment it went quiet, not after the wait that
                // confirmed it.
                return done.or(Some(Self::close(ep, since, counters, false)));
            }
        } else {
            ep.quiet_since = None;
        }
        done
    }

    /// The load in progress, as a line for the report.
    #[must_use]
    pub fn in_progress(&self, now_s: f64) -> Option<String> {
        let ep = self.episode.as_ref()?;
        let waiting: Vec<&str> = ep
            .layers
            .iter()
            .filter(|(_, quiet)| quiet.is_none())
            .map(|(name, _)| *name)
            .collect();
        Some(format!(
            "{} loading for {:.1}s, waiting on {}",
            ep.cause.label(),
            now_s - ep.began_s,
            if waiting.is_empty() {
                "nothing (settling)".to_string()
            } else {
                waiting.join(", ")
            }
        ))
    }

    fn open(cause: Cause, now_s: f64, base: Counters) -> Episode {
        Episode {
            cause,
            began_s: now_s,
            base,
            layers: Vec::new(),
            quiet_since: None,
        }
    }

    fn close(ep: Episode, end_s: f64, now: Counters, cut_short: bool) -> LoadRecord {
        let mut finished: Vec<(&'static str, f64)> = ep
            .layers
            .iter()
            .map(|(name, quiet)| (*name, quiet.unwrap_or(end_s).min(end_s) - ep.began_s))
            .collect();
        finished.sort_by(|a, b| b.1.total_cmp(&a.1));
        LoadRecord {
            cause: ep.cause,
            began_s: ep.began_s,
            total_s: end_s - ep.began_s,
            finished,
            cut_short,
            requests: now.requests.saturating_sub(ep.base.requests),
            kb: now.bytes.saturating_sub(ep.base.bytes) / 1024,
            cache_hits: now.cache_hits.saturating_sub(ep.base.cache_hits),
            cache_misses: now.cache_misses.saturating_sub(ep.base.cache_misses),
        }
    }
}

/// The recent loads, for the diagnostics report.
#[derive(Resource)]
pub struct LoadTimings {
    started: web_time::Instant,
    clock: LoadClock,
    recent: VecDeque<LoadRecord>,
}

impl Default for LoadTimings {
    fn default() -> Self {
        Self {
            started: web_time::Instant::now(),
            clock: LoadClock::default(),
            recent: VecDeque::new(),
        }
    }
}

impl LoadTimings {
    /// The report's section: the load in progress, then the finished ones,
    /// newest first.
    #[must_use]
    pub fn report(&self) -> String {
        let now_s = self.started.elapsed().as_secs_f64();
        let mut out = String::from("map loads (newest first):\n");
        if let Some(line) = self.clock.in_progress(now_s) {
            out.push_str(&format!("  now: {line}\n"));
        }
        for r in self.recent.iter().rev() {
            out.push_str(&format!(
                "  {:.0}s ago: {}\n",
                now_s - r.began_s,
                r.summary()
            ));
        }
        if self.recent.is_empty() && self.clock.episode.is_none() {
            out.push_str("  (none yet)\n");
        }
        out
    }
}

/// Measure this frame. Runs after every streamer has reported.
pub fn track_loads(
    mut timings: ResMut<LoadTimings>,
    registry: Option<Res<StreamRegistry>>,
    geo: Option<Res<crate::systems::geo_nav::GeoNavState>>,
) {
    let Some(registry) = registry else {
        return;
    };
    if geo.is_some_and(|g| g.pending_teleport.is_some()) {
        timings.clock.jumped();
    }
    let now_s = timings.started.elapsed().as_secs_f64();
    let busy = registry.busy();
    let Some(record) = timings.clock.step(now_s, &busy, Counters::now()) else {
        return;
    };
    if record.total_s >= LOG_MIN_S {
        tracing::info!(load = %record.summary(), "map load");
    }
    timings.recent.push_back(record);
    while timings.recent.len() > KEEP {
        timings.recent.pop_front();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(clock: &mut LoadClock, frames: &[(f64, &[&'static str])]) -> Vec<LoadRecord> {
        frames
            .iter()
            .filter_map(|(t, busy)| clock.step(*t, busy, Counters::default()))
            .collect()
    }

    #[test]
    fn a_load_runs_from_the_first_busy_layer_to_the_last_quiet_one() {
        let mut clock = LoadClock::default();
        let done = run(
            &mut clock,
            &[
                (0.0, &[]),
                (1.0, &["map"]),
                (2.0, &["map", "lod22"]),
                (4.0, &["lod22"]),
                (9.0, &[]),
                (9.5, &[]),
                (10.0, &[]),
            ],
        );
        assert_eq!(done.len(), 1);
        let r = &done[0];
        assert_eq!(r.cause, Cause::Launch);
        assert_eq!(r.total_s, 8.0, "1 s to 9 s, not to the end of the wait");
        assert_eq!(r.finished, vec![("lod22", 8.0), ("map", 3.0)]);
    }

    #[test]
    fn a_layer_that_blinks_off_does_not_end_the_load() {
        let mut clock = LoadClock::default();
        let done = run(
            &mut clock,
            &[
                (0.0, &["map"]),
                (1.0, &[]),
                (1.5, &["surfaces"]),
                (3.0, &[]),
                (4.0, &[]),
            ],
        );
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].total_s, 3.0);
    }

    #[test]
    fn only_the_first_load_is_the_launch() {
        let mut clock = LoadClock::default();
        let done = run(
            &mut clock,
            &[
                (0.0, &["map"]),
                (1.0, &[]),
                (2.0, &[]),
                (5.0, &["map"]),
                (6.0, &[]),
                (7.0, &[]),
            ],
        );
        assert_eq!(
            done.iter().map(|r| r.cause).collect::<Vec<_>>(),
            [Cause::Launch, Cause::Moving]
        );
    }

    #[test]
    fn a_jump_cuts_the_load_in_progress_short_and_starts_its_own() {
        let mut clock = LoadClock::default();
        let mut done = run(&mut clock, &[(0.0, &["map"]), (2.0, &["map"])]);
        clock.jumped();
        done.extend(run(&mut clock, &[(3.0, &["map"]), (8.0, &[]), (9.0, &[])]));
        assert_eq!(done.len(), 2);
        assert_eq!((done[0].cause, done[0].cut_short), (Cause::Launch, true));
        assert_eq!((done[1].cause, done[1].total_s), (Cause::Jump, 5.0));
    }

    #[test]
    fn the_counters_are_what_the_load_itself_used() {
        let mut clock = LoadClock::default();
        let at = |requests, hits| Counters {
            requests,
            bytes: requests * 2048,
            cache_hits: hits,
            cache_misses: 0,
        };
        assert!(clock.step(0.0, &["map"], at(10, 100)).is_none());
        assert!(clock.step(1.0, &[], at(25, 130)).is_none());
        let r = clock.step(2.0, &[], at(25, 130)).expect("finished");
        assert_eq!((r.requests, r.kb, r.cache_hits), (15, 30, 30));
    }
}
