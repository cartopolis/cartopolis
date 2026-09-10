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
//! # What it reaches, and what it does not
//!
//! Three layers of cache sit in front of the tiles, and a reset that emptied only
//! the deepest would appear to do nothing:
//!
//! * the **blob store** — `platform::storage::clear`, i.e. the files under
//!   `utils::cache::cache_root` or the browser's `cache` object store;
//! * the **in-memory tile bodies** — `tile_source::forget_cached_tiles`, 32 MiB of
//!   already-fetched bytes that would otherwise answer the re-fetch instantly;
//! * the **drawn tiles** — `map_stream::retear_tiles`, because a picture already
//!   on screen is not going to ask for its bytes again.
//!
//! It stops there, and the stopping point is a judgement rather than an oversight.
//! The building streamers, the terrain field, the interiors and the paving each
//! hold their own parsed cells and rebuild on `MapStream::anchor_generation` —
//! bumping that is "a city's worth of mesh churn" (`apply_aerial_source`'s note),
//! which is the documented way to lose the device on a phone. Their *files* are
//! gone, so they refetch as the camera moves or on the next launch. The panel says
//! so rather than implying the world is rebuilt in place.
//!
//! # Why this is a resource and a system rather than a button that does the work
//!
//! The deleting is a directory walk on another thread (natively) or a chain of
//! IndexedDB requests (on the web), and neither can be awaited inside an egui
//! pass. So the panel writes a request, this consumes it, and the answer comes
//! back through a [`storage::ClearProgress`] the panel polls. The retear also
//! needs `ResMut<MapStream>` and a query over the tile entities, neither of which
//! the settings system has any other reason to hold.

use bevy::prelude::*;

use crate::platform::storage::{self, ClearProgress, Namespace};
use crate::systems::map_stream::{MapStream, StreamTile};

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
}

impl CacheReset {
    /// Is a reset still running? A `Some` progress that has not finished.
    pub fn running(&self) -> bool {
        self.progress.as_ref().is_some_and(|p| !p.finished())
    }

    /// What the last finished reset freed, in bytes. `None` while one is running
    /// or before any has been asked for.
    pub fn freed_bytes(&self) -> Option<u64> {
        self.progress
            .as_ref()
            .filter(|p| p.finished())
            .map(|p| p.freed_bytes())
    }
}

/// Start a requested reset: empty the store, forget the bytes in memory, and drop
/// the tiles that were drawn from them.
///
/// Cheap to run unconditionally — one boolean test in the steady state.
pub fn apply_cache_reset(
    mut reset: ResMut<CacheReset>,
    mut stream: ResMut<MapStream>,
    tiles: Query<Entity, With<StreamTile>>,
) {
    if !reset.requested {
        return;
    }
    // Taken rather than left for the end: everything below is infallible and
    // synchronous, but a `return` added later must not leave the flag set and the
    // reset restarting on every frame for the rest of the session.
    reset.requested = false;
    // The store first. It is the slow half and it runs elsewhere, so starting it
    // before the two instant ones costs nothing and narrows the window in which a
    // streamer could write a *fresh* body into a store that is about to be
    // emptied. Narrows, not closes — a fetch already in the air will land in the
    // cleared store and be cached again, which is the correct outcome anyway: it
    // is a body fetched after the user asked for a clean one.
    reset.progress = Some(storage::clear(Namespace::Cache));
    crate::systems::tile_source::forget_cached_tiles();
    crate::systems::map_stream::retear_tiles(&mut stream, tiles.iter());
    tracing::info!("cache reset: store cleared, tiles re-streaming");
}
