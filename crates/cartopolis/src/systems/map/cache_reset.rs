//! Throwing the downloaded map away, from Settings ▸ General ▸ Storage.
//!
//! Everything this client streams — basemap tiles, the aerial photography, 3DBAG
//! shells, the BAG and BGT registers, Overpass answers, transit timetables,
//! downloaded models — is cached under one namespace of `platform::storage`, on
//! the reasoning that it is all re-fetchable. Which it is, right up until a body
//! is cached *wrong*: a truncated `.mvt`, a tile fetched from a source that has
//! since been re-cut, a register page from before an extractor changed shape. The
//! streamers cache bytes and re-read them forever, so a bad entry is not a bad
//! frame, it is a permanent one, and the only repair a person outside a shell has
//! is to delete the file. This is that repair.
//!
//! # It rebuilds the whole map, because anything less looked like it did nothing
//!
//! Until 2026-09-25 this emptied the store, forgot the in-memory tile bodies and
//! retore the basemap — and stopped there, on the judgement that rebuilding the
//! buildings, the terrain, the paving and the interiors in place was a city's worth
//! of mesh churn. So the one part of the picture a person presses this *for* —
//! buildings standing somewhere odd, ground that looks wrong — stayed exactly as
//! it was until the camera happened to move far enough to refetch it, which read
//! as "clearing the cache doesn't do much". A button nobody can see working is not
//! a repair.
//!
//! So it ends in a **re-anchor onto the anchor already in use**. That is the
//! teardown every layer already knows — a teleport's — through the bounded despawn
//! queue and the per-frame upload ceiling, so the churn is a teleport's and no
//! worse: the device-loss guards that make a teleport safe on a phone make this
//! safe too. The frame does not move (the anchor is the same snapped tile centre),
//! so nothing about the camera or the avatar has to be carried across. What a
//! re-anchor resets for free is most of what used to survive a clear: every
//! layer's parsed cells, the warm stores (`platform::warm` clears on a new
//! generation), the terrain field and its tile bookkeeping.
//!
//! # In order, and the order is the substance
//!
//! 1. **The store** — `platform::storage::clear`, on another thread (natively) or a
//!    chain of IndexedDB requests (on the web). Nothing is rebuilt until it has
//!    finished: rebuilding while the walk is still deleting lets a streamer read a
//!    file the walk has not reached yet, and re-draw the very thing being cleared.
//! 2. **The coverage manifests** — `coverage::refresh_all` and
//!    `osm_buildings::refresh_coverage`. They were once per process, and they decide
//!    what gets asked for: a grading run's stamp, the cells a tree serves. Rebuilding
//!    from launch-time manifests re-downloads the old answers under the old keys.
//!    Waited for, but only for [`REFRESH_WAIT_S`] — an unreachable host keeps the
//!    answers it had, and must not leave the button stuck on "Clearing…".
//! 3. **What is held in memory** — `tile_source`'s 32 MiB of tile bodies, which
//!    would otherwise answer the re-fetch instantly, and the terrain tiles, whose
//!    failure counts would otherwise send a tile that failed before the clear
//!    straight to the fallback after it.
//! 4. **The re-anchor.**
//!
//! # Why this is a resource and a system rather than a button that does the work
//!
//! None of those steps can be awaited inside an egui pass, so the panel writes a
//! request, this system walks the steps a frame at a time, and the panel reads
//! [`CacheReset::running`] back to draw its status.

use bevy::prelude::*;

use crate::platform::storage::{self, ClearProgress, Namespace};
use crate::systems::map_stream::MapStream;

/// The longest a reset waits for the coverage manifests before rebuilding anyway.
/// They are a few kilobytes from our own host; this is for a host that is down.
const REFRESH_WAIT_S: f64 = 10.0;

/// Where a reset has got to.
#[derive(Default)]
enum Phase {
    #[default]
    Idle,
    /// The store is emptying.
    Clearing,
    /// The store is empty; the manifests are being asked again since `since`.
    Refreshing { since: f64 },
}

/// The request, and what came of it. Written by `settings::render_settings_panel`,
/// consumed by [`apply_cache_reset`], read back by the panel to draw its status.
#[derive(Resource, Default)]
pub struct CacheReset {
    /// Set by the button. Cleared the moment the reset starts, so a held frame
    /// cannot start a second one.
    pub requested: bool,
    /// The clear in progress, or the last one that finished. `None` before the
    /// first reset of the session — which is what lets the panel tell "never asked"
    /// apart from "asked, and it freed nothing".
    pub progress: Option<std::sync::Arc<ClearProgress>>,
    phase: Phase,
}

impl CacheReset {
    /// Is a reset still running? Until the rebuild has been asked for, not merely
    /// until the files are gone.
    pub fn running(&self) -> bool {
        !matches!(self.phase, Phase::Idle)
    }

    /// What the last finished reset freed, in bytes. `None` while one is running
    /// or before any has been asked for.
    pub fn freed_bytes(&self) -> Option<u64> {
        if self.running() {
            return None;
        }
        self.progress
            .as_ref()
            .filter(|p| p.finished())
            .map(|p| p.freed_bytes())
    }
}

/// Walk a requested reset through its steps (see the module note).
///
/// Cheap to run unconditionally — a match on an idle phase in the steady state.
pub fn apply_cache_reset(
    mut reset: ResMut<CacheReset>,
    mut stream: ResMut<MapStream>,
    mut terrain_tiles: ResMut<crate::systems::terrain_tiles::TerrainTiles>,
    time: Res<Time>,
) {
    let now = f64::from(time.elapsed_secs());
    match reset.phase {
        Phase::Idle => {
            if !reset.requested {
                return;
            }
            // Taken rather than left for later: a step added below must not leave
            // the flag set and the reset restarting on every frame.
            reset.requested = false;
            reset.progress = Some(storage::clear(Namespace::Cache));
            reset.phase = Phase::Clearing;
            tracing::info!("cache reset: clearing the store");
        }
        Phase::Clearing => {
            if !reset.progress.as_ref().is_none_or(|p| p.finished()) {
                return;
            }
            crate::systems::coverage::refresh_all(now);
            crate::systems::osm_buildings::refresh_coverage(now, true);
            reset.phase = Phase::Refreshing { since: now };
        }
        Phase::Refreshing { since } => {
            crate::systems::coverage::poke_all(now);
            crate::systems::osm_buildings::refresh_coverage(now, false);
            let answered = crate::systems::coverage::all_refreshed()
                && crate::systems::osm_buildings::coverage_refreshed();
            if !answered && now - since < REFRESH_WAIT_S {
                return;
            }
            crate::systems::tile_source::forget_cached_tiles();
            *terrain_tiles = crate::systems::terrain_tiles::TerrainTiles::default();
            // Onto the anchor already in use: the frame stays put, and every layer
            // rebuilds from nothing through the teardown a teleport uses.
            if let Some((lat, lng, _)) = stream.geo_anchor() {
                stream.request_reanchor(lat, lng);
            }
            reset.phase = Phase::Idle;
            tracing::info!(
                manifests_answered = answered,
                "cache reset: store cleared, rebuilding the map"
            );
        }
    }
}
