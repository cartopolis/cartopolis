//! Persistent per-user data: last location/camera and bookmarks, stored as
//! JSON at `data/user.json`. The start of the user-information store that will
//! later grow OpenSim-style inventory — deliberately simple for now.
//!
//! Locations are stored as geo coordinates (lat, lng, altitude), not BigSpace
//! cells: geo is anchor-independent, so a saved spot stays valid even if the
//! region anchor or grid layout changes between runs. On startup, once the
//! world anchor exists, the saved camera state is restored and the last
//! location is fed through the normal teleport path.

use std::path::PathBuf;

use bevy::prelude::*;
use big_space::prelude::GridCell;
use serde::{Deserialize, Serialize};

use crate::resources::CameraState;
use crate::systems::free_camera::FreeCamera;
use crate::systems::geo_nav::{camera_geo, CameraPose, GeoNavState, Teleport};
use crate::systems::map_stream::MapStream;

/// Seconds between autosave snapshots of the current location.
const AUTOSAVE_INTERVAL: f32 = 5.0;

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct UserData {
    pub last_location: Option<SavedLocation>,
    /// The user's Home location — set from the status panel's Set Home button
    /// and jumped to by the nav bar's Home button. Camera pose included (like a
    /// bookmark) so Home returns to the exact view, not just the spot.
    #[serde(default)]
    pub home: Option<SavedLocation>,
    pub bookmarks: Vec<Bookmark>,
    /// The directory tree the bookmarks hang in (see [`PlaceFolder`]).
    /// `#[serde(default)]` keeps pre-folder files loadable — every bookmark in
    /// one is simply at the top level.
    #[serde(default)]
    pub folders: Vec<PlaceFolder>,
    /// ADR-016: session tokens keyed by server address, so an external sim only
    /// asks for a password once. Not stored for solo mode (ephemeral port).
    #[serde(default)]
    pub session_tokens: std::collections::HashMap<String, String>,
    /// The local player's chosen avatar body + clothing. `#[serde(default)]`
    /// keeps pre-appearance files loadable (they get Man/Casual).
    #[serde(default)]
    pub appearance: AvatarAppearance,
    /// The Layers panel's toggles, persisted like the last location so the map
    /// comes back the way it was left. `None` on files from before the field
    /// existed — startup then keeps the defaults.
    #[serde(default)]
    pub layers: Option<LayerPrefs>,
}

/// Snapshot of the Layers panel's toggles (see [`crate::systems::data_layers::
/// DataLayers::prefs`] / `apply_prefs`). Overlay layers are keyed by their
/// `LAYER_DEFS` key rather than index, so reordering or adding layers never
/// shuffles a saved file's state onto the wrong layer.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct LayerPrefs {
    pub buildings: bool,
    pub tdbag: bool,
    pub osm_buildings: bool,
    pub map: bool,
    pub trees: bool,
    pub roads_textured: bool,
    pub water_shading: bool,
    /// Street names over the map (`systems::street_labels`). Defaulted so a file
    /// written before the layer existed still parses — and it defaults to *on*,
    /// which is what `DataLayers::default` gives a fresh install.
    #[serde(default = "yes")]
    pub street_labels: bool,
    /// Street furniture (`systems::street_furniture`). Defaulted so a file written
    /// before the layer existed still parses — and it defaults to *on*, which is
    /// what `DataLayers::default` gives a fresh install.
    #[serde(default = "yes")]
    pub furniture: bool,
    pub vehicles: bool,
    /// The height layer (`systems::height`). Defaulted, so a file written before it
    /// existed still parses — and defaults to *off*, because it is a request to a
    /// third party's service that only answers over the Netherlands.
    #[serde(default)]
    pub height: bool,
    #[serde(default)]
    pub overlays: std::collections::HashMap<String, bool>,
}

/// The local player's avatar choice. Each clothing slot holds the display label of
/// a [`crate::systems::avatar`] character style (e.g. "Casual", "Business",
/// "Witch") available for `sex`; the renderer mixes the matching slot mesh from
/// each style's character glTF. `head` covers face/hair/headwear. A "preset" just
/// sets all four slots to one style. Only the local avatar reads this — remotes
/// use the default until appearance is replicated (a future protocol/ADR).
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct AvatarAppearance {
    #[serde(default)]
    pub sex: AvatarSex,
    #[serde(default = "default_style")]
    pub head: String,
    #[serde(default = "default_style")]
    pub top: String,
    #[serde(default = "default_style")]
    pub legs: String,
    #[serde(default = "default_style")]
    pub feet: String,
    /// Optional non-weapon accessory: the style whose accessory mesh to show (e.g.
    /// "Adventurer" for its backpack), or `None`.
    #[serde(default)]
    pub accessory: Option<String>,
}

fn default_style() -> String {
    "Casual".to_string()
}

/// `#[serde(default)]` for a `bool` that should come back **on** when the file
/// predates it.
fn yes() -> bool {
    true
}

impl Default for AvatarAppearance {
    fn default() -> Self {
        Self {
            sex: AvatarSex::default(),
            head: default_style(),
            top: default_style(),
            legs: default_style(),
            feet: default_style(),
            accessory: None,
        }
    }
}

impl AvatarAppearance {
    /// The single style shared by all four clothing slots, or `None` if it's a mix.
    pub fn uniform_style(&self) -> Option<&str> {
        (self.head == self.top && self.top == self.legs && self.legs == self.feet)
            .then_some(self.head.as_str())
    }
}

/// Body model. `Man`/`Woman` map to the two Quaternius Universal Base Characters
/// (CC0) the client ships; more body types can be added later.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum AvatarSex {
    #[default]
    Man,
    Woman,
}

impl AvatarSex {
    pub const ALL: [AvatarSex; 2] = [AvatarSex::Man, AvatarSex::Woman];
    pub fn label(self) -> &'static str {
        match self {
            AvatarSex::Man => "Man",
            AvatarSex::Woman => "Woman",
        }
    }
}


/// Where the user was when Viberfox closed, camera included.
#[derive(Serialize, Deserialize, Clone)]
pub struct SavedLocation {
    pub lat: f64,
    pub lng: f64,
    /// Camera altitude in metres.
    pub altitude: f64,
    /// Camera orientation. `#[serde(default)]` keeps files written before poses
    /// existed loadable (they restore position only).
    #[serde(default)]
    pub pose: CameraPose,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Bookmark {
    pub id: u64,
    /// Defaults to the reverse-geocoded address; user-renamable.
    pub name: String,
    pub lat: f64,
    pub lng: f64,
    pub altitude: f64,
    /// Camera orientation captured when bookmarked, so a jump restores the
    /// exact view. `#[serde(default)]` for pre-pose bookmark files.
    #[serde(default)]
    pub pose: CameraPose,
    /// Enclosing [`PlaceFolder`], or `None` at the top level. Determines the
    /// colour its map pin is drawn in. `#[serde(default)]` for pre-folder files.
    #[serde(default)]
    pub folder: Option<u64>,
}

/// A directory in the Places tree. Folders nest (`parent`), hold bookmarks
/// (`Bookmark::folder`), and carry a colour that every pin inside them is drawn
/// in — that is the whole point of grouping: a folder is one glance on the map,
/// not a list you have to read.
///
/// The colour is *not* inherited down the tree. A nested folder is its own
/// group with its own colour; making children inherit would mean a folder's
/// colour silently changes when it is dragged somewhere else.
#[derive(Serialize, Deserialize, Clone)]
pub struct PlaceFolder {
    pub id: u64,
    pub name: String,
    /// Pin colour for every place in this folder, as sRGB 0–1.
    pub color: [f32; 3],
    /// Enclosing folder, or `None` at the top level.
    #[serde(default)]
    pub parent: Option<u64>,
    /// Panel state: contents hidden. Persisted so the tree looks the same next run.
    #[serde(default)]
    pub collapsed: bool,
}

/// Pin colour for a place that is in no folder — the classic red map pin.
pub const DEFAULT_PIN_COLOR: [f32; 3] = [0.906, 0.243, 0.212];

/// Colours handed to new folders, in order, so a fresh tree is legible without
/// anyone opening the colour picker. Wraps around once exhausted.
pub const FOLDER_PALETTE: [[f32; 3]; 8] = [
    [0.20, 0.51, 0.89], // blue
    [0.18, 0.65, 0.38], // green
    [0.95, 0.62, 0.10], // amber
    [0.66, 0.36, 0.85], // purple
    [0.13, 0.70, 0.72], // teal
    [0.91, 0.36, 0.58], // pink
    [0.83, 0.42, 0.20], // rust
    [0.45, 0.55, 0.62], // slate
];

/// The persistent user store. Mutate `data` and set `dirty`; the autosave
/// system writes it out (and always on exit).
#[derive(Resource)]
pub struct UserStore {
    pub data: UserData,
    pub dirty: bool,
    path: PathBuf,
    /// Last JSON written, to skip no-op disk writes.
    last_written: String,
}

impl UserStore {
    pub fn load() -> Self {
        let path = PathBuf::from("data").join("user.json");
        let (data, last_written) = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<UserData>(&text) {
                Ok(data) => (data, text),
                Err(e) => {
                    tracing::warn!(error = %e, "user.json unreadable; starting fresh");
                    (UserData::default(), String::new())
                }
            },
            Err(_) => (UserData::default(), String::new()),
        };
        Self { data, dirty: false, path, last_written }
    }

    /// Write to disk if the serialized form changed.
    pub fn save(&mut self) {
        let Ok(json) = serde_json::to_string_pretty(&self.data) else {
            return;
        };
        if json == self.last_written {
            self.dirty = false;
            return;
        }
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::write(&self.path, &json) {
            Ok(()) => {
                self.last_written = json;
                self.dirty = false;
            }
            Err(e) => tracing::warn!(error = %e, path = %self.path.display(), "user.json write failed"),
        }
    }

    /// Session token previously issued by `server`, if any (ADR-016).
    pub fn session_token(&self, server: &str) -> Option<String> {
        self.data.session_tokens.get(server).cloned()
    }

    /// Remember the session token for `server`; saved on the next autosave.
    pub fn set_session_token(&mut self, server: &str, token: &str) {
        if self.data.session_tokens.get(server).map(String::as_str) != Some(token) {
            self.data
                .session_tokens
                .insert(server.to_string(), token.to_string());
            self.dirty = true;
        }
    }

    /// Add a bookmark and return its id.
    pub fn add_bookmark(
        &mut self,
        name: String,
        lat: f64,
        lng: f64,
        altitude: f64,
        pose: CameraPose,
    ) -> u64 {
        let id = self
            .data
            .bookmarks
            .iter()
            .map(|b| b.id)
            .max()
            .unwrap_or(0)
            + 1;
        self.data
            .bookmarks
            .push(Bookmark { id, name, lat, lng, altitude, pose, folder: None });
        self.dirty = true;
        id
    }

    /// Create a folder under `parent` and return its id. Its colour is the next
    /// one in [`FOLDER_PALETTE`], so folders differ on the map without anyone
    /// picking a colour first.
    pub fn add_folder(&mut self, name: String, parent: Option<u64>) -> u64 {
        let id = self.data.folders.iter().map(|f| f.id).max().unwrap_or(0) + 1;
        let color = FOLDER_PALETTE[self.data.folders.len() % FOLDER_PALETTE.len()];
        self.data
            .folders
            .push(PlaceFolder { id, name, color, parent, collapsed: false });
        self.dirty = true;
        id
    }

    /// Delete a folder, keeping everything in it: its child folders and places
    /// move up to its parent. Deleting a directory must never be a way to lose
    /// bookmarks by accident — the 🗑 next to a place is how you delete a place.
    pub fn remove_folder(&mut self, id: u64) {
        let Some(parent) = self.folder(id).map(|f| f.parent) else {
            return;
        };
        for f in self.data.folders.iter_mut().filter(|f| f.parent == Some(id)) {
            f.parent = parent;
        }
        for b in self.data.bookmarks.iter_mut().filter(|b| b.folder == Some(id)) {
            b.folder = parent;
        }
        self.data.folders.retain(|f| f.id != id);
        self.dirty = true;
    }

    pub fn folder(&self, id: u64) -> Option<&PlaceFolder> {
        self.data.folders.iter().find(|f| f.id == id)
    }

    pub fn folder_mut(&mut self, id: u64) -> Option<&mut PlaceFolder> {
        self.data.folders.iter_mut().find(|f| f.id == id)
    }

    /// Colour of the pin for a place in `folder` — the folder's colour, or
    /// [`DEFAULT_PIN_COLOR`] at the top level (or if the folder went missing).
    pub fn pin_color(&self, folder: Option<u64>) -> [f32; 3] {
        folder
            .and_then(|id| self.folder(id))
            .map_or(DEFAULT_PIN_COLOR, |f| f.color)
    }

    /// Move a place into `folder` (`None` = top level).
    pub fn move_place(&mut self, place: u64, folder: Option<u64>) {
        // Dropping into a folder that no longer exists lands at the top level
        // rather than orphaning the place under a dangling id.
        let folder = folder.filter(|id| self.folder(*id).is_some());
        if let Some(b) = self.data.bookmarks.iter_mut().find(|b| b.id == place) {
            if b.folder != folder {
                b.folder = folder;
                self.dirty = true;
            }
        }
    }

    /// Re-parent a folder (`None` = top level), refusing anything that would
    /// make the tree a cycle — dropping a folder onto itself or onto one of its
    /// own descendants. Returns whether the move happened.
    pub fn move_folder(&mut self, folder: u64, parent: Option<u64>) -> bool {
        if parent == Some(folder) || parent.is_some_and(|p| self.is_descendant(p, folder)) {
            return false;
        }
        let parent = parent.filter(|id| self.folder(*id).is_some());
        match self.folder_mut(folder) {
            Some(f) if f.parent != parent => {
                f.parent = parent;
                self.dirty = true;
                true
            }
            _ => false,
        }
    }

    /// Is `id` inside `ancestor` (at any depth)? Walks parents, bounded by the
    /// folder count so a corrupted file with a cycle in it can't hang the UI.
    pub fn is_descendant(&self, id: u64, ancestor: u64) -> bool {
        let mut at = self.folder(id).and_then(|f| f.parent);
        for _ in 0..self.data.folders.len() {
            match at {
                Some(p) if p == ancestor => return true,
                Some(p) => at = self.folder(p).and_then(|f| f.parent),
                None => return false,
            }
        }
        false
    }

    /// Set (or replace) the Home location — the spot the nav bar's Home button
    /// returns to. Pose included so Home restores the exact view.
    pub fn set_home(&mut self, lat: f64, lng: f64, altitude: f64, pose: CameraPose) {
        self.data.home = Some(SavedLocation { lat, lng, altitude, pose });
        self.dirty = true;
    }
}

/// Once the world anchor exists, put the session where it should begin, through
/// the normal teleport path (which restores the camera pose — mode and
/// orientation — as well as the spot).
///
/// Two possible starts, and the order matters: a place named by the launch
/// (`?place=…` on the web, `VIBERFOX_PLACE` on desktop) wins over the persisted
/// last location, because a link that names a place has to land there. Only one
/// of them is ever applied, so the two can't fight over the first frames.
pub fn restore_start_location(
    mut done: Local<bool>,
    store: Res<UserStore>,
    stream: Res<MapStream>,
    mut nav: ResMut<GeoNavState>,
) {
    if *done {
        return;
    }
    if stream.geo_anchor().is_none() {
        return; // anchor not up yet; try again next frame
    }
    *done = true;

    if let Some(fix) = crate::systems::geo_nav::launch_place() {
        tracing::info!(
            lat = fix.lat,
            lng = fix.lng,
            view = fix.look.is_some(),
            "starting at the launch place"
        );
        nav.pending_teleport = Some(Teleport::fix(fix));
        return;
    }

    let Some(loc) = &store.data.last_location else {
        return;
    };

    nav.pending_teleport = Some(Teleport {
        lat: loc.lat,
        lng: loc.lng,
        altitude: Some(loc.altitude),
        pose: Some(loc.pose.clone()),
    });
    tracing::info!(lat = loc.lat, lng = loc.lng, mode = %loc.pose.mode, "restoring last location");
}

/// Snapshot the current location every few seconds and persist the store when
/// it changed — and always flush on AppExit (File ▸ Exit or window close).
/// Startup restore of the Layers panel's toggles, once. Unlike the location
/// restore this waits for nothing — the toggles have no anchor dependency, and
/// the streamers all read them live, so applying on the first frame means the
/// first thing streamed in already matches the saved preferences.
pub fn restore_layer_prefs(
    mut done: Local<bool>,
    store: Res<UserStore>,
    mut dl: ResMut<crate::systems::data_layers::DataLayers>,
) {
    if *done {
        return;
    }
    *done = true;
    if let Some(p) = &store.data.layers {
        dl.apply_prefs(p);
    }
}

pub fn autosave_user_store(
    time: Res<Time>,
    mut next_at: Local<f32>,
    mut exits: MessageReader<AppExit>,
    mut store: ResMut<UserStore>,
    stream: Res<MapStream>,
    globe: Res<crate::systems::globe::GlobeState>,
    camera_state: Res<CameraState>,
    cameras: Query<(&GridCell, &Transform), With<FreeCamera>>,
    dl: Res<crate::systems::data_layers::DataLayers>,
) {
    let exiting = exits.read().next().is_some();
    let due = time.elapsed_secs() >= *next_at;
    if !exiting && !due && !store.dirty {
        return;
    }
    if due {
        *next_at = time.elapsed_secs() + AUTOSAVE_INTERVAL;
    }

    store.data.layers = Some(dl.prefs());

    if let (Some(anchor), Ok((cell, tf))) = (stream.geo_anchor(), cameras.single()) {
        // Both through the shared camera fix (`camera_geo` / `altitude_m`): the
        // flat-frame lat/lng and the local `y` are only meaningful below the
        // space tier, and storing them from orbit is what saved a mid-ocean
        // last location that the next startup restored and re-anchored to.
        let (lat, lng) = camera_geo(anchor, &globe, cell, tf);
        let altitude = globe.altitude_m(cell, tf);
        store.data.last_location = Some(SavedLocation {
            lat,
            lng,
            altitude,
            pose: CameraPose::capture(&camera_state),
        });
    }
    store.save();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A user.json written before camera poses existed must still load — with
    /// no `pose` field — instead of failing and wiping the user's bookmarks.
    #[test]
    fn legacy_user_data_without_pose_loads() {
        let json = r#"{
            "last_location": {"lat": 53.2, "lng": 6.5, "altitude": 100.0},
            "bookmarks": [
                {"id": 1, "name": "Home", "lat": 53.21, "lng": 6.56, "altitude": 80.0}
            ]
        }"#;
        let data: UserData = serde_json::from_str(json).expect("legacy JSON must load");
        assert_eq!(data.bookmarks.len(), 1);
        // Missing pose defaults to empty (→ keeps the current view on jump).
        assert_eq!(data.bookmarks[0].pose.mode, "");
        assert_eq!(data.last_location.unwrap().pose.mode, "");
        // Missing session_tokens defaults to empty (ADR-016 rollout).
        assert!(data.session_tokens.is_empty());
        // Missing appearance defaults to a casual man (all slots "Casual").
        assert_eq!(data.appearance.sex, AvatarSex::Man);
        assert_eq!(data.appearance.uniform_style(), Some("Casual"));
        assert_eq!(data.appearance.accessory, None);
        // Missing folders: an empty tree, every bookmark at the top level.
        assert!(data.folders.is_empty());
        assert_eq!(data.bookmarks[0].folder, None);
    }

    fn test_store() -> UserStore {
        UserStore {
            data: UserData::default(),
            dirty: false,
            path: PathBuf::from("/tmp/viberfox-test-user.json"),
            last_written: String::new(),
        }
    }

    /// Deleting a folder must keep everything in it — children move up to the
    /// deleted folder's parent. A directory delete that silently took a dozen
    /// saved places with it would be unrecoverable (the store overwrites the file).
    #[test]
    fn removing_a_folder_lifts_its_contents_to_the_parent() {
        let mut store = test_store();
        let outer = store.add_folder("Outer".into(), None);
        let inner = store.add_folder("Inner".into(), Some(outer));
        let deep = store.add_folder("Deep".into(), Some(inner));
        let place = store.add_bookmark("Place".into(), 1.0, 2.0, 60.0, CameraPose::default());
        store.move_place(place, Some(inner));

        store.remove_folder(inner);

        assert!(store.folder(inner).is_none(), "the folder itself is gone");
        assert_eq!(store.folder(deep).unwrap().parent, Some(outer), "child folder lifted");
        assert_eq!(store.data.bookmarks.len(), 1, "the place survives the delete");
        assert_eq!(store.data.bookmarks[0].folder, Some(outer), "place lifted");
    }

    /// Drag-and-drop must not be able to detach a subtree from the root by
    /// dropping a folder into its own descendant — that loses every place under
    /// it from the panel, since the tree is only ever walked down from the root.
    #[test]
    fn a_folder_cannot_be_dropped_into_its_own_descendant() {
        let mut store = test_store();
        let outer = store.add_folder("Outer".into(), None);
        let inner = store.add_folder("Inner".into(), Some(outer));
        let deep = store.add_folder("Deep".into(), Some(inner));

        assert!(!store.move_folder(outer, Some(deep)), "cycle refused");
        assert!(!store.move_folder(outer, Some(outer)), "self-parent refused");
        assert_eq!(store.folder(outer).unwrap().parent, None, "tree unchanged");

        // The legal direction still works, and lifting to the top level too.
        assert!(store.move_folder(deep, Some(outer)));
        assert_eq!(store.folder(deep).unwrap().parent, Some(outer));
        assert!(store.move_folder(inner, None));
        assert_eq!(store.folder(inner).unwrap().parent, None);
    }

    /// A pin takes its folder's colour; a top-level place (or one whose folder
    /// vanished) falls back to the default red.
    #[test]
    fn pin_colour_comes_from_the_folder() {
        let mut store = test_store();
        let id = store.add_folder("Blue".into(), None);
        store.folder_mut(id).unwrap().color = [0.1, 0.2, 0.3];
        assert_eq!(store.pin_color(Some(id)), [0.1, 0.2, 0.3]);
        assert_eq!(store.pin_color(None), DEFAULT_PIN_COLOR);
        assert_eq!(store.pin_color(Some(9999)), DEFAULT_PIN_COLOR);
    }

    #[test]
    fn session_token_get_set_is_idempotent() {
        let mut store = test_store();
        assert_eq!(store.session_token("host:4747"), None);
        store.set_session_token("host:4747", "abc");
        assert_eq!(store.session_token("host:4747"), Some("abc".to_string()));
        assert!(store.dirty, "storing a new token marks the store dirty");
        // Re-storing the same token does not re-dirty.
        store.dirty = false;
        store.set_session_token("host:4747", "abc");
        assert!(!store.dirty, "unchanged token is a no-op");
    }
}
