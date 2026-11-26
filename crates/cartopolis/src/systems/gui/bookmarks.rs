//! Places: the state behind the Places panel (top bar ▸ Places, drawn by
//! `lib_places`) — saved locations in a directory tree — and the machinery
//! behind it: naming a new place, taking its photo, applying what the panel
//! asked for.
//!
//! A new place is named by reverse-geocoding its coordinates through Nominatim
//! (same usage policy as the search bar: identifying User-Agent, background
//! thread, low volume); the name is user-editable afterwards. Jumping to a
//! place teleports through the normal geo_nav path, preserving the camera mode.
//! Everything persists in the user store (`data/user.json`).
//!
//! **The tree.** Places live in nested folders ([`crate::systems::user_store::PlaceFolder`])
//! — an ordinary directory structure. A folder carries a colour, and every
//! place inside it is drawn in that colour on the map (`crate::systems::place_pins`),
//! which is what makes a group readable at a glance instead of a list to
//! scroll. The tree is flattened into [`Row`]s ([`rows`]) and drawn as a flat
//! list with indentation.

use crate::platform::sync::{Arc, Mutex};
use crate::resources::MapAnchor;

use bevy::prelude::*;
use bevy::tasks::IoTaskPool;

use crate::grid::world_to_lat_lng;
use crate::grid::AnchoredCamera;
use crate::platform::http;
use crate::resources::CameraState;
use crate::systems::geo_nav::{nominatim_base_url, CameraPose, GeoNavState, Teleport};
use crate::systems::user_store::UserStore;

/// What a rename or a move is targeting — the two id spaces are separate, so it
/// has to say which tree the id belongs to.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Item {
    Place(u64),
    Folder(u64),
}

/// Places UI state + the reverse-geocode result channel.
#[derive(Resource, Default)]
pub struct BookmarksState {
    pub panel_open: bool,
    /// Finished reverse lookups: (bookmark id, address).
    resolved: Arc<Mutex<Vec<(u64, String)>>>,
    /// The capture in flight, and which place it is for. One at a time: the
    /// platform arms all put a modal in front of the user, so a second request
    /// could not be answered anyway.
    capture: Option<PendingPhoto>,
    /// Why the last photo failed, shown in the panel until the next attempt.
    photo_error: Option<String>,
    /// The photo open full size (`lib_picture_viewer`): the place and its
    /// photo's stem. Outlives the panel: a photo opened from it stays up
    /// when the panel closes behind it.
    viewing: Option<(u64, String)>,
}

/// A photo the user asked for and has not got yet.
struct PendingPhoto {
    place: u64,
    stage: PhotoStage,
}

/// The two waits between "take a photo" and a photo on the place.
enum PhotoStage {
    /// The camera app, the file dialog or the browser's picker is up.
    Capturing(crate::platform::photo::Capture),
    /// The bytes are in and are being decoded, straightened and re-encoded.
    ///
    /// On the `IoTaskPool` rather than inline, because this is the one expensive
    /// step: a 12 MP phone photo is a few hundred milliseconds of decode and
    /// resample, and doing it in the system would drop that as a single stalled
    /// frame on exactly the frame the user comes back from the camera. (On wasm
    /// the pool is the main thread, so there it is deferred rather than moved.)
    Preparing(Arc<Mutex<Option<Result<crate::utils::photo::Prepared, String>>>>),
}

impl BookmarksState {
    /// Resolve a bookmark's address on a background thread. The reverse lookup
    /// uses only lat/lng — altitude never enters it, so a bookmark taken from
    /// orbit resolves the same as one taken at street level. What it resolves to
    /// is the point on the globe directly under the camera; over open ocean (or
    /// on a network failure) Nominatim has no address, so we fall back to the
    /// coordinates rather than leaving the name stuck on the placeholder.
    pub fn request_address(&self, id: u64, lat: f64, lng: f64) {
        let resolved = self.resolved.clone();
        // Detached fetch on the IoTaskPool (ADR-033 seam) — no OS thread, so it
        // works on wasm; the `resolved` channel and its drain are unchanged.
        IoTaskPool::get()
            .spawn(async move {
                let url = format!(
                    "{}/reverse?format=jsonv2&lat={lat}&lon={lng}",
                    nominatim_base_url()
                );
                let name = http::fetch_string(http::Request::get(url))
                    .await
                    .ok()
                    .and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
                    .and_then(|json| {
                        json.get("display_name")
                            .and_then(|v| v.as_str())
                            .map(short_address)
                    })
                    .unwrap_or_else(|| coord_label(lat, lng));
                if let Ok(mut g) = resolved.lock() {
                    g.push((id, name));
                }
            })
            .detach();
    }

    /// The place a photo is being waited for, if any.
    pub(crate) fn capturing_for(&self) -> Option<u64> {
        self.capture.as_ref().map(|c| c.place)
    }

    /// Why the last photo failed, until it is dismissed or retried.
    pub(crate) fn photo_error(&self) -> Option<&str> {
        self.photo_error.as_deref()
    }

    /// The photo open full size, as its place's name and its stem — or
    /// `None`, closing it, when the place is gone or its photo was removed
    /// or replaced underneath the viewer.
    #[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
    pub(crate) fn viewing_photo(&mut self, store: &UserStore) -> Option<(String, String)> {
        let (place, stem) = self.viewing.clone()?;
        let shown = store
            .data
            .bookmarks
            .iter()
            .find(|b| b.id == place && b.photo.as_deref() == Some(stem.as_str()))
            .map(|b| (b.name.clone(), stem));
        if shown.is_none() {
            self.viewing = None;
        }
        shown
    }

    /// Close the full-size photo (the viewer's ✕).
    #[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
    pub(crate) fn close_photo(&mut self) {
        self.viewing = None;
    }

    /// Do what the panel asked for this frame: it collects an [`Actions`] and
    /// hands it here. `current` is where the camera is now ([`current_view`]),
    /// for a new place and for "Update to the current view".
    ///
    /// Returns true when the panel jumped to a place, and so should close:
    /// jumping is the last thing anyone wants the Places list for, and on a
    /// phone the panel *is* the screen — it would stay open covering exactly
    /// the view it was asked to fly to. Only a jump does this: renaming,
    /// recolouring, moving a row into a folder and taking a photo are all
    /// list work, and a list that shut itself after every edit would be
    /// unusable.
    pub(crate) fn apply_actions(
        &mut self,
        store: &mut UserStore,
        nav: &mut GeoNavState,
        current: Option<CurrentView>,
        act: Actions,
    ) -> bool {
        if act.new_place {
            if let Some((lat, lng, alt, pose)) = current.clone() {
                let id = store.add_bookmark(PLACEHOLDER_NAME.to_string(), lat, lng, alt, pose);
                // Named by reverse geocoding, like every other new place — the
                // placeholder is what shows until the address lands.
                self.request_address(id, lat, lng);
            }
        }
        // Read before `apply` takes `act`.
        let jumped = act.teleport.is_some();
        apply_photo_actions(self, store, &act);
        apply(store, nav, act.update.zip(current), act);
        jumped
    }
}

/// Where the camera is now and how it looks: what a new place saves, and
/// what "Update to the current view" repoints a place at. `None` before the
/// world anchor exists.
pub(crate) type CurrentView = (f64, f64, f64, CameraPose);

/// The camera's [`CurrentView`].
pub(crate) fn current_view(
    camera: &AnchoredCamera,
    camera_state: &CameraState,
    map_anchor: &MapAnchor,
) -> Option<CurrentView> {
    camera
        .world_position_f64()
        .zip(map_anchor.geo)
        .map(|(world, anchor)| {
            let (lat, lng) = world_to_lat_lng(anchor, world.x, world.z);
            (lat, lng, world.y, CameraPose::capture(camera_state))
        })
}

/// First few segments of a Nominatim display_name — enough to recognise the
/// place without the province/country tail.
pub(crate) fn short_address(display_name: &str) -> String {
    let parts: Vec<&str> = display_name.split(',').map(str::trim).take(3).collect();
    parts.join(", ")
}

/// Fallback name when the reverse lookup finds no address (open ocean, or a
/// failed request): the signed coordinates as N/S, E/W.
fn coord_label(lat: f64, lng: f64) -> String {
    let ns = if lat >= 0.0 { 'N' } else { 'S' };
    let ew = if lng >= 0.0 { 'E' } else { 'W' };
    format!("{:.3}°{ns}, {:.3}°{ew}", lat.abs(), lng.abs())
}

/// Fold finished reverse lookups into the bookmark names.
pub fn poll_bookmark_addresses(bookmarks: Res<BookmarksState>, mut store: ResMut<UserStore>) {
    let resolved: Vec<(u64, String)> = bookmarks
        .resolved
        .lock()
        .map(|mut g| g.drain(..).collect())
        .unwrap_or_default();
    for (id, name) in resolved {
        if let Some(b) = store.data.bookmarks.iter_mut().find(|b| b.id == id) {
            // Don't clobber a name the user already edited.
            if b.name == PLACEHOLDER_NAME {
                b.name = name;
                store.dirty = true;
            }
        }
    }
}

/// Name a bookmark carries until its address resolves.
pub const PLACEHOLDER_NAME: &str = "Resolving address…";

/// One line of the flattened tree. Owned copies of what the row draws, so the
/// panel holds no borrow of the store and can queue mutations freely — and
/// what it compares from frame to frame.
#[derive(Clone, PartialEq)]
pub(crate) enum Row {
    Folder {
        id: u64,
        depth: usize,
        name: String,
        color: [f32; 3],
        collapsed: bool,
        /// Places in this folder at any depth — shown on a collapsed folder so a
        /// closed directory still says how much is in it.
        count: usize,
    },
    Place {
        id: u64,
        depth: usize,
        name: String,
        color: [f32; 3],
        lat: f64,
        lng: f64,
        altitude: f64,
        /// Stem of the place's photo, if it has one.
        photo: Option<String>,
    },
}

/// The tree as the panel draws it ([`flatten`] from the top).
pub(crate) fn rows(store: &UserStore) -> Vec<Row> {
    let mut rows = Vec::new();
    flatten(store, None, 0, &mut rows);
    rows
}

/// Flatten the directory tree into draw order: each folder, then its child
/// folders, then its places. A collapsed folder contributes only its own row.
fn flatten(store: &UserStore, parent: Option<u64>, depth: usize, out: &mut Vec<Row>) {
    // Guard against a cycle in a hand-edited/corrupted user.json: without it a
    // parent loop recurses until the stack runs out.
    if depth > 32 {
        return;
    }
    for f in store.data.folders.iter().filter(|f| f.parent == parent) {
        out.push(Row::Folder {
            id: f.id,
            depth,
            name: f.name.clone(),
            color: f.color,
            collapsed: f.collapsed,
            count: store
                .data
                .bookmarks
                .iter()
                .filter(|b| {
                    b.folder == Some(f.id)
                        || b.folder.is_some_and(|id| store.is_descendant(id, f.id))
                })
                .count(),
        });
        if !f.collapsed {
            flatten(store, Some(f.id), depth + 1, out);
        }
    }
    for b in store.data.bookmarks.iter().filter(|b| b.folder == parent) {
        out.push(Row::Place {
            id: b.id,
            depth,
            name: b.name.clone(),
            color: store.pin_color(b.folder),
            lat: b.lat,
            lng: b.lng,
            altitude: b.altitude,
            photo: b.photo.clone(),
        });
    }
}

/// Everything the panel wants done, collected from its taps and applied by
/// [`BookmarksState::apply_actions`].
#[derive(Default)]
pub(crate) struct Actions {
    pub(crate) new_folder: bool,
    /// Save where the camera is now as a place. It was the status HUD's ☆, which
    /// is a strange place for it to have been: the HUD is a readout, it is hidden
    /// by default now, and pressing ☆ opened *this* panel anyway to show what had
    /// just been saved. So the button lives where its result appears.
    pub(crate) new_place: bool,
    pub(crate) delete_place: Option<u64>,
    pub(crate) delete_folder: Option<u64>,
    pub(crate) update: Option<u64>,
    pub(crate) teleport: Option<Teleport>,
    /// What to call the teleport's destination in the location history.
    pub(crate) teleport_label: Option<String>,
    /// "Route here" on a place: its name and where it is.
    pub(crate) route: Option<(String, f64, f64)>,
    pub(crate) rename: Option<(Item, String)>,
    pub(crate) recolor: Option<(u64, [f32; 3])>,
    pub(crate) toggle_collapse: Option<u64>,
    /// (what was moved, which folder it goes into — `None` = top level).
    pub(crate) drop: Option<(Item, Option<u64>)>,
    /// Start a photo capture for this place.
    pub(crate) take_photo: Option<u64>,
    /// Forget this place's photo.
    pub(crate) clear_photo: Option<u64>,
    /// Open the full-size photo (`lib_picture_viewer`): the place, its name
    /// and its photo stem.
    pub(crate) view_photo: Option<(u64, String, String)>,
    /// Abandon the capture in flight.
    pub(crate) cancel_photo: bool,
    /// Clear the last photo failure from the panel.
    pub(crate) dismiss_photo_error: bool,
}

impl Actions {
    /// Fly to a saved place, keeping the *current* camera mode and moving it
    /// to the place's spot, rather than snapping back to the mode it was saved
    /// in (`pose: None` skips `CameraPose::apply`, so `apply_teleport`
    /// repositions the mode you're in — in Follow that means the avatar walks
    /// there).
    pub(crate) fn jump(&mut self, name: &str, (lat, lng, altitude): (f64, f64, f64)) {
        self.teleport = Some(Teleport {
            lat,
            lng,
            altitude: Some(altitude),
            pose: None,
            silent: false,
            look: false,
        });
        self.teleport_label = Some(name.to_string());
    }
}

/// Apply the frame's queued mutations to the store.
fn apply(
    store: &mut UserStore,
    nav: &mut GeoNavState,
    update: Option<(u64, (f64, f64, f64, CameraPose))>,
    act: Actions,
) {
    if act.new_folder {
        store.add_folder("New folder".to_string(), None);
    }
    if let Some((id, (lat, lng, alt, pose))) = update {
        // Keep the name; repoint the place at where we are now.
        if let Some(b) = store.data.bookmarks.iter_mut().find(|b| b.id == id) {
            b.lat = lat;
            b.lng = lng;
            b.altitude = alt;
            b.pose = pose;
            store.dirty = true;
            tracing::info!(id, lat, lng, "updated place to current view");
        }
    }
    if let Some((item, name)) = act.rename {
        let name = name.trim().to_string();
        if !name.is_empty() {
            match item {
                Item::Place(id) => {
                    if let Some(b) = store.data.bookmarks.iter_mut().find(|b| b.id == id) {
                        b.name = name;
                        store.dirty = true;
                    }
                }
                Item::Folder(id) => {
                    if let Some(f) = store.folder_mut(id) {
                        f.name = name;
                        store.dirty = true;
                    }
                }
            }
        }
    }
    if let Some((id, color)) = act.recolor {
        if let Some(f) = store.folder_mut(id) {
            f.color = color;
            store.dirty = true;
        }
    }
    if let Some(id) = act.toggle_collapse {
        if let Some(f) = store.folder_mut(id) {
            f.collapsed = !f.collapsed;
            store.dirty = true;
        }
    }
    if let Some((item, target)) = act.drop {
        match item {
            Item::Place(id) => store.move_place(id, target),
            Item::Folder(id) => {
                if !store.move_folder(id, target) {
                    tracing::debug!(id, ?target, "refused a folder drop that would cycle");
                }
            }
        }
    }
    if let Some(id) = act.delete_place {
        // Through the store rather than by retaining in place: deleting a place
        // has to take its photo files with it (`UserStore::remove_bookmark`).
        store.remove_bookmark(id);
    }
    if let Some(id) = act.delete_folder {
        store.remove_folder(id);
    }
    if let Some(tp) = act.teleport {
        // Name the visit, so it lists as itself in the address bar's recents
        // rather than as five decimals.
        nav.pending_label = act.teleport_label;
        nav.pending_teleport = Some(tp);
    }
    if let Some((name, lat, lng)) = act.route {
        // Queued rather than applied: this panel does not own `RouteState`, and
        // `geo_nav::drive_geocode` spends the request (see `geo_nav::RouteRequest`).
        nav.route_to(name, lat, lng);
    }
}

/// Apply the photo actions the panel collected.
///
/// Separate from [`apply`] because these are the only actions that touch
/// [`BookmarksState`] as well as the store — a capture is UI state that outlives
/// the frame that started it.
fn apply_photo_actions(bookmarks: &mut BookmarksState, store: &mut UserStore, act: &Actions) {
    if act.dismiss_photo_error {
        bookmarks.photo_error = None;
    }
    if act.cancel_photo {
        abandon_capture(bookmarks);
    }
    if let Some(place) = act.take_photo {
        // Only one capture at a time (see `BookmarksState::capture`), so asking
        // for a second photo abandons the first rather than queueing behind it.
        abandon_capture(bookmarks);
        bookmarks.photo_error = None;
        match crate::platform::photo::start() {
            Ok(capture) => {
                bookmarks.capture = Some(PendingPhoto {
                    place,
                    stage: PhotoStage::Capturing(capture),
                });
            }
            Err(message) => {
                tracing::warn!(error = %message, "could not start a photo capture");
                bookmarks.photo_error = Some(message);
            }
        }
    }
    if let Some(id) = act.clear_photo {
        store.clear_bookmark_photo(id);
    }
    if let Some((place, _, stem)) = &act.view_photo {
        // Shown by `lib_picture_viewer`, which reads it each frame
        // (`BookmarksState::viewing_photo`) and closes it when the photo goes.
        bookmarks.viewing = Some((*place, stem.clone()));
    }
}

/// Fill an empty store with a few saved places so `--ui places` captures the
/// panel doing its job rather than its "No places yet" card.
///
/// The other scripted panels seed themselves for the same reason (`--ui route`
/// plans a route, `--ui search` types a query). Two of the places carry a
/// photo stem that no file backs: the row shows its photo marker from the
/// stem alone, which is the alignment case — rows with a photo mixed in with
/// rows without — this panel gets wrong when it gets it wrong.
pub fn seed_demo_places(store: &mut UserStore) {
    if !store.data.bookmarks.is_empty() {
        return;
    }
    let folder = store.add_folder("Groningen".to_string(), None);
    // Long enough to truncate in a 300 pt panel, which is what a reverse-geocoded
    // name looks like in practice.
    let seeds = [
        ("Laan Corpus den Hoorn, Corpus den Hoorn", None, false),
        (
            "Laan Corpus den Hoorn, Corpus den Hoorn",
            Some(folder),
            true,
        ),
        (
            "Slochterdiep, Harkstede, Midden-Groningen",
            Some(folder),
            true,
        ),
        ("Grote Markt, Binnenstad", None, false),
    ];
    for (i, (name, parent, photo)) in seeds.into_iter().enumerate() {
        let id = store.add_bookmark(
            name.to_string(),
            53.219 + i as f64 * 0.004,
            6.566 + i as f64 * 0.006,
            300.0,
            CameraPose::default(),
        );
        store.move_place(id, parent);
        if !photo {
            continue;
        }
        if let Some(b) = store.data.bookmarks.iter_mut().find(|b| b.id == id) {
            b.photo = Some(format!("demo-{id}"));
        }
    }
}

/// End the capture in flight, telling the platform to release whatever it holds
/// for it (on Android, the empty MediaStore row the camera was given).
fn abandon_capture(bookmarks: &mut BookmarksState) {
    if let Some(mut pending) = bookmarks.capture.take() {
        if let PhotoStage::Capturing(capture) = &mut pending.stage {
            capture.cancel();
        }
    }
}

/// Drive the capture in flight to its end.
///
/// Registered on its own rather than inside the panel, and deliberately *not*
/// gated on the panel being open: the user is looking at a camera app while this
/// runs, may well come back to a closed panel, and a photo that only lands if the
/// right panel happens to be open would be a bug nobody could describe.
pub fn poll_place_photos(mut bookmarks: ResMut<BookmarksState>, mut store: ResMut<UserStore>) {
    let Some(pending) = bookmarks.capture.as_mut() else {
        return;
    };
    let place = pending.place;

    match &mut pending.stage {
        PhotoStage::Capturing(capture) => match capture.poll() {
            crate::platform::photo::Outcome::Pending => {}
            crate::platform::photo::Outcome::Cancelled => {
                bookmarks.capture = None;
            }
            crate::platform::photo::Outcome::Failed(message) => {
                tracing::warn!(error = %message, "photo capture failed");
                bookmarks.capture = None;
                bookmarks.photo_error = Some(message);
            }
            crate::platform::photo::Outcome::Ready(bytes) => {
                let slot = Arc::new(Mutex::new(None));
                let sink = slot.clone();
                IoTaskPool::get()
                    .spawn(async move {
                        let prepared = crate::utils::photo::prepare(&bytes);
                        if let Ok(mut sink) = sink.lock() {
                            *sink = Some(prepared);
                        }
                    })
                    .detach();
                pending.stage = PhotoStage::Preparing(slot);
            }
        },
        PhotoStage::Preparing(slot) => {
            let Some(prepared) = slot.lock().ok().and_then(|mut slot| slot.take()) else {
                return;
            };
            bookmarks.capture = None;
            match prepared.and_then(|prepared| store.set_bookmark_photo(place, &prepared)) {
                Ok(()) => tracing::info!(place, "attached a photo to a place"),
                Err(message) => {
                    tracing::warn!(place, error = %message, "could not attach the photo");
                    bookmarks.photo_error = Some(message);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_address_keeps_the_first_three_parts() {
        assert_eq!(
            short_address("Grote Markt, Binnenstad, Groningen, Nederland"),
            "Grote Markt, Binnenstad, Groningen"
        );
    }

    #[test]
    fn the_fallback_name_signs_the_coordinates() {
        assert_eq!(coord_label(53.2194, -6.5671), "53.219°N, 6.567°W");
        assert_eq!(coord_label(-1.0, 2.0), "1.000°S, 2.000°E");
    }
}
