//! Where the geo services are: what the connected server said at
//! `GET /api/services` (`cartopolis_core::services`), asked once per session.
//!
//! Each resolver (`geo_nav::nominatim_base_url`, `osm_buildings::overpass_url`
//! and `tdbag_url`, `terrain_tiles::template`, `height::atlas_height_url`) reads
//! its `CARTO_*` override first, then [`directory`], then its built-in default.
//!
//! **Only a value that differs from the built-in default counts as the server's
//! word.** A server that configures nothing answers with exactly those defaults,
//! and the web build's own defaults for terrain and height are relative paths to
//! its own origin; taking the server's absolute spelling of the same place would
//! change nothing but the string every cache is keyed on.
//!
//! A layer that read its address before the answer arrived (3DBAG's coverage
//! manifest is read once) keeps the address it read until the next session.
//!
//! A `static` rather than a resource for the reason `model_assets::ENDPOINT` is
//! one: the readers are fetch helpers on the `IoTaskPool` with no world to reach.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

use bevy::tasks::IoTaskPool;
use cartopolis_core::services::{self as core, ServiceDirectory, SERVICES_PATH};

use crate::platform::http;

static DIRECTORY: RwLock<Option<ServiceDirectory>> = RwLock::new(None);

/// Bumped by every [`fetch`] and [`clear`], so an answer that arrives after its
/// session ended is dropped instead of outliving it.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Ask the server at `base` (its API base, no trailing slash) where its services
/// are. Called when a session comes up; a server that does not answer (an older
/// one, 404) leaves every default in place.
pub fn fetch(base: &str) {
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let url = format!("{}{SERVICES_PATH}", base.trim_end_matches('/'));
    IoTaskPool::get()
        .spawn(async move {
            let body = match http::fetch_string(http::Request::get(url.clone())).await {
                Ok(body) => body,
                Err(e) => {
                    tracing::info!("{url}: no service directory ({e}); using the defaults");
                    return;
                }
            };
            let directory = match serde_json::from_str::<ServiceDirectory>(&body) {
                Ok(d) => d,
                Err(e) => {
                    // An older server's web host answers its own page here.
                    tracing::info!(
                        "{url}: no readable service directory ({e}); using the defaults"
                    );
                    return;
                }
            };
            if GENERATION.load(Ordering::SeqCst) != generation {
                return;
            }
            tracing::info!(?directory, "service directory");
            if let Ok(mut slot) = DIRECTORY.write() {
                *slot = Some(directory);
            }
        })
        .detach();
}

/// Forget the server's directory. Called from session teardown.
pub fn clear() {
    GENERATION.fetch_add(1, Ordering::SeqCst);
    if let Ok(mut slot) = DIRECTORY.write() {
        *slot = None;
    }
}

/// The server's value for one service, when it said something other than the
/// built-in default (see the module note).
fn said(pick: fn(&ServiceDirectory) -> &String, default: &str) -> Option<String> {
    let slot = DIRECTORY.read().ok()?;
    let value = pick(slot.as_ref()?);
    (value != default).then(|| value.clone())
}

pub fn geocoder() -> Option<String> {
    said(|d| &d.geocoder, core::DEFAULT_GEOCODER_URL)
}

pub fn osm_query() -> Option<String> {
    said(|d| &d.osm_query, core::DEFAULT_OSM_QUERY_URL)
}

pub fn map_data() -> Option<String> {
    said(|d| &d.map_data, core::DEFAULT_MAP_DATA_URL)
}

pub fn terrain() -> Option<String> {
    said(|d| &d.terrain, core::DEFAULT_TERRAIN_URL)
}

pub fn height() -> Option<String> {
    said(|d| &d.height, core::DEFAULT_HEIGHT_URL)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test, because the directory is process-wide and tests run in parallel.
    #[test]
    fn only_a_value_off_the_default_is_the_servers_word() {
        clear();
        assert_eq!(geocoder(), None, "no server, no word");
        *DIRECTORY.write().unwrap() = Some(ServiceDirectory {
            geocoder: "https://geo.example.org".into(),
            map_data: String::new(),
            ..ServiceDirectory::default()
        });
        assert_eq!(geocoder().as_deref(), Some("https://geo.example.org"));
        assert_eq!(
            map_data().as_deref(),
            Some(""),
            "an empty value turns a layer off"
        );
        assert_eq!(terrain(), None, "the default spelled out is not a change");
        clear();
        assert_eq!(geocoder(), None);
    }
}
