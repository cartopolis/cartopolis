//! Persistent per-user data: last location/camera and bookmarks, stored as
//! JSON at `user.json` under [`crate::utils::cache::app_data_root`]. The start of
//! the user-information store that will later grow OpenSim-style inventory —
//! deliberately simple for now.
//!
//! **The path is resolved, not literal, and that is load-bearing.** It used to be a
//! cwd-relative `data/user.json`, which on Android meant the file was never written
//! at all (a NativeActivity process starts in `/`; every autosave failed into a
//! `warn!`) and on desktop meant the profile you got depended on the directory you
//! launched from. `app_data_root` keeps an existing checkout `data/` so nothing
//! moves under a dev box, and answers with a real per-user directory everywhere else.
//!
//! **Two rules keep a bad file from being an unrecoverable one**, because this store
//! is the only copy of the user's bookmarks:
//!
//! - it is written through a temp file and a rename ([`crate::utils::cache::
//!   write_atomic`]), so a process killed mid-save leaves the previous file intact
//!   rather than a half one;
//! - a file that does not parse is *renamed aside* before the store starts fresh.
//!   Without that, "unreadable" was terminal: the store came up empty with an empty
//!   `last_written`, and [`autosave_user_store`] — whose first deadline is time zero
//!   — overwrote the original within a frame of the app starting.
//!
//! Schema changes are the other way this file gets lost. Every leaf carries a serde
//! default, so a field added here (or one written by a newer build and read by an
//! older one) degrades to that default instead of failing the whole document, and
//! [`UserData::version`] exists so a change that *cannot* be expressed as a default
//! has somewhere to branch.
//!
//! Locations are stored as geo coordinates (lat, lng, altitude), not BigSpace
//! cells: geo is anchor-independent, so a saved spot stays valid even if the
//! region anchor or grid layout changes between runs. On startup, once the
//! world anchor exists, the saved camera state is restored and the last
//! location is fed through the normal teleport path.

use crate::resources::MapAnchor;
use std::path::{Path, PathBuf};

use bevy::prelude::*;
use big_space::prelude::GridCell;
use serde::{Deserialize, Serialize};

use crate::components::FreeCamera;
use crate::resources::CameraState;
use crate::systems::geo_nav::{camera_geo, CameraPose, GeoNavState, Teleport};

/// Seconds between autosave snapshots of the current location.
const AUTOSAVE_INTERVAL: f32 = 5.0;

/// Schema revision written into every new file.
///
/// 1 is "the schema as of the day versioning was added"; a file with no `version`
/// at all (0) is everything written before that, which is every file in the wild
/// today. Nothing branches on it yet and nothing should until it has to — the point
/// of the field is that the *hook* exists, because adding one retroactively is
/// impossible: without a number in the file there is no way to tell a document
/// missing a field from one where the field means something new.
///
/// Bump it only for a change serde defaults cannot express — a field that changed
/// meaning, or a value that has to be recomputed — and put the fix in [`migrate`].
const USER_DATA_VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct UserData {
    /// Schema revision this document was written by; see [`USER_DATA_VERSION`].
    /// `0` on every file written before the field existed.
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub last_location: Option<SavedLocation>,
    /// The user's Home location — set from the status panel's Set Home button
    /// and jumped to by the nav bar's Home button. Camera pose included (like a
    /// bookmark) so Home returns to the exact view, not just the spot.
    #[serde(default)]
    pub home: Option<SavedLocation>,
    #[serde(default)]
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
    /// How this client entered the world last time (`systems::session`), so the
    /// sign-in screen comes back prefilled and — with `auto_enter` — need not be
    /// shown at all. Never holds a password; the token map above is what makes a
    /// remembered server silent.
    #[serde(default)]
    pub session: crate::systems::session::SessionPrefs,
    /// The local player's chosen avatar body + clothing. `#[serde(default)]`
    /// keeps pre-appearance files loadable (they get Man/Casual).
    #[serde(default)]
    pub appearance: AvatarAppearance,
    /// The Layers panel's toggles, persisted like the last location so the map
    /// comes back the way it was left. `None` on files from before the field
    /// existed — startup then keeps the defaults.
    #[serde(default)]
    pub layers: Option<LayerPrefs>,
    /// The HUD's sky controls: how the day/night clock is driven and whether the
    /// weather is live. `None` on files from before the field existed.
    #[serde(default)]
    pub sky: Option<SkyPrefs>,
}

/// Snapshot of the sky switchers in the HUD — the sun's Auto/☀/🌙 (plus the dev
/// panel's `Manual`) and the weather's Auto/Manual — with the manual knobs they
/// leave the user in charge of.
///
/// Two of the fields are deliberately conditional, because saving them
/// unconditionally would either churn the file or restore a state the user never
/// chose:
/// - `time_of_day` is `None` unless the sun is in `Manual`. Every other mode
///   *derives* the clock from the real time each frame, so persisting it would
///   rewrite `user.json` on every autosave tick and restore a stale hour that the
///   next `apply_sun_mode` immediately overwrites anyway.
/// - the weather knobs are only authoritative while live weather is off; with it
///   on they are the last met.no sample, which is worth restoring as a starting
///   point (the next fetch replaces it within a frame or two of the anchor
///   settling).
///
/// Every field is defaulted, and the defaults are the *fresh-install* values rather
/// than serde's zeroes wherever those differ (`fog_density`), so a block written by
/// a build that did not have one of these knobs restores as "the app's default"
/// rather than as "off".
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct SkyPrefs {
    #[serde(default)]
    pub sun_mode: crate::resources::SunMode,
    /// The clock in hours, 0–24. `Some` only in [`SunMode::Manual`].
    #[serde(default)]
    pub time_of_day: Option<f32>,
    /// Live met.no weather drives cloud/fog/rain (the HUD's weather "Auto").
    #[serde(default)]
    pub weather_live: bool,
    /// Defaulted to `DevPanelState::default`'s 0.15, not to 0.0: a missing field
    /// means "this build didn't write one", and answering that with a fogless sky
    /// would be a visible change nobody asked for.
    #[serde(default = "default_fog")]
    pub fog_density: f32,
    #[serde(default)]
    pub cloud_cover: f32,
    #[serde(default)]
    pub precipitation: f32,
}

/// Snapshot of the Layers panel's toggles (see [`crate::systems::data_layers::
/// DataLayers::prefs`] / `apply_prefs`). Overlay layers are keyed by their
/// `LAYER_DEFS` key rather than index, so reordering or adding layers never
/// shuffles a saved file's state onto the wrong layer.
///
/// **Every leaf is defaulted, and each default is the value
/// `DataLayers::default()` gives a fresh install on this device.** Two toggles used
/// to carry one and the rest did not, which made adding a layer a schema break: one
/// new `bool` without a default, shipped once, fails the parse of every existing
/// `user.json` — and before the quarantine in [`UserStore::load`] that meant the
/// autosave overwrote the file within a frame and took the user's bookmarks with a
/// map-layer checkbox.
///
/// The five detail layers default to [`detail_default`] rather than to `true`: the
/// fresh-install answer is the render budget's, so on the phone tier it is *off*,
/// and a hard-coded `true` here would quietly hand a phone the desktop's layer set.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct LayerPrefs {
    #[serde(default = "yes")]
    pub buildings: bool,
    #[serde(default = "yes")]
    pub tdbag: bool,
    #[serde(default = "yes")]
    pub osm_buildings: bool,
    #[serde(default = "yes")]
    pub map: bool,
    #[serde(default = "detail_default")]
    pub trees: bool,
    #[serde(default = "detail_default")]
    pub roads_textured: bool,
    #[serde(default = "detail_default")]
    pub water_shading: bool,
    /// Street names over the map (`systems::street_labels`).
    #[serde(default = "detail_default")]
    pub street_labels: bool,
    /// Street furniture (`systems::street_furniture`).
    #[serde(default = "detail_default")]
    pub furniture: bool,
    /// Off on a fresh install: the vehicle layer is opt-in.
    #[serde(default)]
    pub vehicles: bool,
    /// The height layer (`systems::height`). Defaults to *off*, because it is a
    /// request to a third party's service that only answers over the Netherlands.
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

/// `#[serde(default)]` for the five detail layers, whose fresh-install state is the
/// device's render budget rather than a constant (`quality::Budget::detail_layers`
/// — they are the ones the phone tier starts without).
fn detail_default() -> bool {
    crate::quality::budget().detail_layers
}

/// `#[serde(default)]` for [`SkyPrefs::fog_density`]: `DevPanelState::default`'s
/// value, kept in step by hand because that struct is not `Deserialize`.
fn default_fog() -> f32 {
    0.15
}

/// `#[serde(default)]` for a place's or folder's name, so a row that lost its name
/// is a visible entry someone can rename rather than a blank line in the Places
/// list — an invisible row is one nobody knows to delete.
fn unnamed() -> String {
    "Unnamed".to_string()
}

/// `#[serde(default)]` for a folder's colour — black would be a pin nobody can see
/// against the map.
fn default_folder_color() -> [f32; 3] {
    DEFAULT_PIN_COLOR
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

/// A saved place.
///
/// **The coordinates are the one thing here with no serde default**, and that is a
/// decision rather than an oversight: a place is its position, and defaulting a
/// missing `lat`/`lng` to zero would invent a bookmark in the Atlantic that jumps
/// the camera into the ocean when clicked — the same silent-garbage failure
/// [`restore_start_location`] now refuses. Everything else degrades cleanly, so a
/// field added to this struct later is not a schema break.
#[derive(Serialize, Deserialize, Clone)]
pub struct Bookmark {
    /// Zero when a row lost it. Ids are only compared, never used as an index, so
    /// two zeroes are a nuisance in the panel rather than a crash — and the next
    /// [`UserStore::add_bookmark`] hands out `max + 1`, which is still unique.
    #[serde(default)]
    pub id: u64,
    /// Defaults to the reverse-geocoded address; user-renamable.
    #[serde(default = "unnamed")]
    pub name: String,
    pub lat: f64,
    pub lng: f64,
    #[serde(default)]
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
    #[serde(default)]
    pub id: u64,
    #[serde(default = "unnamed")]
    pub name: String,
    /// Pin colour for every place in this folder, as sRGB 0–1.
    #[serde(default = "default_folder_color")]
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

/// Where `user.json` lives this run.
///
/// `utils::cache` is compiled out on wasm (`utils/mod.rs` — there is no filesystem
/// to resolve a directory on), and the web build's store has always been a no-op:
/// every `std::fs` call in here fails and the app runs on defaults. The literal path
/// on that arm is therefore a placeholder that is never opened, not a location.
fn user_data_path() -> PathBuf {
    #[cfg(not(target_arch = "wasm32"))]
    {
        crate::utils::cache::app_data_root().join("user.json")
    }
    #[cfg(target_arch = "wasm32")]
    {
        PathBuf::from("data").join("user.json")
    }
}

/// Durable write of the store, on the one arm that has a filesystem. See
/// [`crate::utils::cache::write_atomic`] for why it is a rename and why the flush
/// before it matters.
fn write_user_json(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        crate::utils::cache::write_atomic(path, bytes)
    }
    #[cfg(target_arch = "wasm32")]
    {
        std::fs::write(path, bytes)
    }
}

/// Rename an unparseable `user.json` to `user.json.corrupt-<unix seconds>` and
/// return where it went, so the log can name a file someone can actually recover
/// bookmarks out of by hand. `None` if even the rename failed.
///
/// Timestamped rather than a single `.corrupt` sibling: a machine that produces one
/// bad file usually produces another, and the second must not overwrite the first
/// copy — which may be the only intact one.
fn quarantine(path: &Path) -> Option<PathBuf> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut kept = path.to_path_buf().into_os_string();
    kept.push(format!(".corrupt-{stamp}"));
    let kept = PathBuf::from(kept);
    std::fs::rename(path, &kept).ok().map(|()| kept)
}

/// A store for someone who has none yet — stamped with the current schema so the
/// first file it writes carries a version.
fn fresh_data() -> UserData {
    UserData {
        version: USER_DATA_VERSION,
        ..UserData::default()
    }
}

/// Bring a document written by an older build up to [`USER_DATA_VERSION`].
///
/// **There is nothing to migrate today, and that is the point**: every schema change
/// so far has been expressible as a serde default, which needs no version at all.
/// This is the hook for the first one that is not — a field whose *meaning* changed,
/// or a value that has to be recomputed from another. Such a change branches on
/// `data.version` here, before the stamp below, and only then bumps the constant.
///
/// Note what the shape guarantees: a migration written here only ever sees a
/// document that came off disk (a fresh store is built by [`fresh_data`] already at
/// the current version), so it never has to be safe to run against defaults.
fn migrate(data: &mut UserData) {
    data.version = USER_DATA_VERSION;
}

impl UserStore {
    pub fn load() -> Self {
        let path = user_data_path();
        let (data, last_written) = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<UserData>(&text) {
                Ok(mut data) => {
                    migrate(&mut data);
                    (data, text)
                }
                Err(e) => {
                    // Move the unparseable file aside *before* handing back an empty
                    // store. The store is the only copy of the user's bookmarks and
                    // the autosave's first deadline is time zero, so without this the
                    // original is gone within a frame of the app starting — and the
                    // one situation in which that happens is the one where something
                    // is already wrong and the bytes are worth keeping.
                    match quarantine(&path) {
                        Some(kept) => tracing::warn!(
                            error = %e,
                            kept = %kept.display(),
                            "user.json did not parse; kept a copy and started fresh"
                        ),
                        None => tracing::warn!(
                            error = %e,
                            path = %path.display(),
                            "user.json did not parse and could not be set aside; starting fresh"
                        ),
                    }
                    (fresh_data(), String::new())
                }
            },
            // No file: a first run, or a directory we cannot read. Either way there
            // is nothing to preserve, so this is the only path that starts blank
            // without saying anything.
            Err(_) => (fresh_data(), String::new()),
        };
        Self {
            data,
            dirty: false,
            path,
            last_written,
        }
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
        // Temp file + rename, never a truncating write: this runs every five
        // seconds and on exit, and the exit case is exactly when the process is
        // most likely to be killed part-way through (Android kills backgrounded
        // apps as a matter of routine). A half-written `user.json` is an
        // unparseable one, which used to be indistinguishable from having no
        // bookmarks at all.
        match write_user_json(&self.path, json.as_bytes()) {
            Ok(()) => {
                self.last_written = json;
                self.dirty = false;
            }
            Err(e) => {
                tracing::warn!(error = %e, path = %self.path.display(), "user.json write failed")
            }
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

    /// Remember how the world was entered, for the next launch's sign-in screen.
    pub fn set_session_prefs(&mut self, prefs: crate::systems::session::SessionPrefs) {
        if self.data.session != prefs {
            self.data.session = prefs;
            self.dirty = true;
        }
    }

    /// Forget a server's session token — what "sign out" means for a machine that
    /// someone else also uses. The next sign-in there asks for the password again.
    pub fn clear_session_token(&mut self, server: &str) {
        if self.data.session_tokens.remove(server).is_some() {
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
        let id = self.data.bookmarks.iter().map(|b| b.id).max().unwrap_or(0) + 1;
        self.data.bookmarks.push(Bookmark {
            id,
            name,
            lat,
            lng,
            altitude,
            pose,
            folder: None,
        });
        self.dirty = true;
        id
    }

    /// Create a folder under `parent` and return its id. Its colour is the next
    /// one in [`FOLDER_PALETTE`], so folders differ on the map without anyone
    /// picking a colour first.
    pub fn add_folder(&mut self, name: String, parent: Option<u64>) -> u64 {
        let id = self.data.folders.iter().map(|f| f.id).max().unwrap_or(0) + 1;
        let color = FOLDER_PALETTE[self.data.folders.len() % FOLDER_PALETTE.len()];
        self.data.folders.push(PlaceFolder {
            id,
            name,
            color,
            parent,
            collapsed: false,
        });
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
        for f in self
            .data
            .folders
            .iter_mut()
            .filter(|f| f.parent == Some(id))
        {
            f.parent = parent;
        }
        for b in self
            .data
            .bookmarks
            .iter_mut()
            .filter(|b| b.folder == Some(id))
        {
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
        self.data.home = Some(SavedLocation {
            lat,
            lng,
            altitude,
            pose,
        });
        self.dirty = true;
    }
}

/// Altitude a restored location may name, in metres. The ceiling is the farthest the
/// space camera can pull back from the Earth's centre
/// (`free_camera::ORBIT_MAX_DISTANCE_SPACE`, 45,000 km), so nothing the camera can
/// legitimately reach is refused; the floor allows a little below sea level and
/// nothing that would put the camera inside the planet.
const RESTORE_ALT_RANGE: std::ops::RangeInclusive<f64> = -1_000.0..=45_000_000.0;

/// Is a saved location one the camera could have been at?
///
/// The same check `geo_nav::parse_location` runs on pasted text, applied to the one
/// input that never had it — the file. Text is obviously untrusted and a file
/// silently is too: it is written by a process that can be killed mid-write, edited
/// by hand, and (since the serde defaults above) able to supply a field the writer
/// never wrote.
///
/// NaN fails every one of these comparisons, which is the intended answer and worth
/// stating: a `RangeInclusive::contains` on NaN is `false`, so the pattern needs no
/// separate `is_nan` arm.
fn plausible_start(loc: &SavedLocation) -> bool {
    (-90.0..=90.0).contains(&loc.lat)
        && (-180.0..=180.0).contains(&loc.lng)
        && RESTORE_ALT_RANGE.contains(&loc.altitude)
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
    mut nav: ResMut<GeoNavState>,
    map_anchor: Res<MapAnchor>,
) {
    if *done {
        return;
    }
    if map_anchor.geo.is_none() {
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
    if !plausible_start(loc) {
        // Dropping the restore leaves the default start, which is the only escape
        // hatch there is: this runs before any UI exists, so a garbled location
        // teleports the camera into nowhere on *every* launch and the user's only
        // way out is to find and delete a file they have never heard of. A NaN
        // latitude, an altitude of 1e30 or a longitude of 400 all reach here the
        // same way — a truncated write, a hand-edit, or a `serde` default standing
        // in for a field that went missing.
        tracing::warn!(
            lat = loc.lat,
            lng = loc.lng,
            altitude = loc.altitude,
            "ignoring an out-of-range saved location; starting at the default"
        );
        return;
    }

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

/// Which sky knobs this run's command line pinned. A flag is an explicit
/// instruction for *this* launch and must survive the restore — `--time 6`
/// asks for dawn, not for whatever the last session left behind. (Scripted
/// captures never reach the restore at all; this is for an interactive run
/// launched with a flag.)
#[derive(Resource, Default)]
pub struct SkyCliOverrides {
    pub time: bool,
    pub fog: bool,
    pub clouds: bool,
    pub rain: bool,
    pub weather_live: bool,
}

/// Startup restore of the sun mode and the weather mode, once — the same shape
/// as [`restore_layer_prefs`] and for the same reason: the sky is a setting the
/// user chose, not a thing to re-pick every launch.
pub fn restore_sky_prefs(
    mut done: Local<bool>,
    store: Res<UserStore>,
    cli: Res<SkyCliOverrides>,
    mut dev: ResMut<crate::resources::DevPanelState>,
    mut weather: ResMut<crate::systems::weather::WeatherState>,
) {
    if *done {
        return;
    }
    *done = true;
    let Some(p) = &store.data.sky else {
        return;
    };

    if !cli.time {
        dev.sun_mode = p.sun_mode;
        // Only Manual owns the clock; in every other mode `apply_sun_mode`
        // computes it from the real time on the next frame.
        if let Some(t) = p.time_of_day {
            dev.time_of_day = t.rem_euclid(24.0);
        }
    }
    if !cli.weather_live {
        weather.live = p.weather_live;
    }
    if !cli.fog {
        dev.fog_density = p.fog_density.clamp(0.0, 1.0);
    }
    if !cli.clouds {
        dev.cloud_cover = p.cloud_cover.clamp(0.0, 1.0);
    }
    if !cli.rain {
        dev.precipitation = p.precipitation.clamp(0.0, 1.0);
    }
}

pub fn autosave_user_store(
    time: Res<Time>,
    mut next_at: Local<f32>,
    mut exits: MessageReader<AppExit>,
    mut store: ResMut<UserStore>,
    globe: Res<crate::systems::globe::GlobeState>,
    camera_state: Res<CameraState>,
    cameras: Query<(&GridCell, &Transform), With<FreeCamera>>,
    dl: Res<crate::systems::data_layers::DataLayers>,
    dev: Res<crate::resources::DevPanelState>,
    weather: Res<crate::systems::weather::WeatherState>,
    map_anchor: Res<MapAnchor>,
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
    // Both go through `DataLayers`, which sees past driver mode's temporary
    // clear sky to what the user actually set.
    store.data.sky = Some(dl.sky_prefs(&dev, &weather));

    if let (Some(anchor), Ok((cell, tf))) = (map_anchor.geo, cameras.single()) {
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
        // Missing sky prefs: the sun/weather switchers keep their defaults.
        assert!(data.sky.is_none());
    }

    /// The sun mode round-trips by *name*, so adding a variant can never shift a
    /// saved file onto a different mode.
    #[test]
    fn sky_prefs_round_trip_by_name() {
        use crate::resources::SunMode;
        let prefs = SkyPrefs {
            sun_mode: SunMode::Night,
            time_of_day: None,
            weather_live: false,
            fog_density: 0.3,
            cloud_cover: 0.5,
            precipitation: 0.0,
        };
        let json = serde_json::to_string(&prefs).unwrap();
        assert!(
            json.contains("\"night\""),
            "modes are named on disk: {json}"
        );
        assert_eq!(serde_json::from_str::<SkyPrefs>(&json).unwrap(), prefs);
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
        assert_eq!(
            store.folder(deep).unwrap().parent,
            Some(outer),
            "child folder lifted"
        );
        assert_eq!(
            store.data.bookmarks.len(),
            1,
            "the place survives the delete"
        );
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
        assert!(
            !store.move_folder(outer, Some(outer)),
            "self-parent refused"
        );
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

    /// A file written by a build with a sun mode this one has never heard of must
    /// cost the sky setting and nothing else. Before the hand-written
    /// `Deserialize`, serde failed the whole document on that one string — and the
    /// autosave then overwrote it, so a rollback ate every bookmark.
    #[test]
    fn an_unknown_sun_mode_costs_only_the_sun_mode() {
        let json = r#"{
            "bookmarks": [
                {"id": 1, "name": "Home", "lat": 53.21, "lng": 6.56, "altitude": 80.0}
            ],
            "sky": {"sun_mode": "eclipse", "weather_live": true,
                    "fog_density": 0.2, "cloud_cover": 0.1, "precipitation": 0.0}
        }"#;
        let data: UserData = serde_json::from_str(json).expect("must not fail the document");
        assert_eq!(data.bookmarks.len(), 1, "the bookmarks survive");
        let sky = data.sky.expect("the sky block still loads");
        assert_eq!(sky.sun_mode, crate::resources::SunMode::Auto);
        assert!(sky.weather_live, "the rest of the block is untouched");
    }

    /// The same rule one level down: a layer block missing fields (an older build's,
    /// or a newer one's read back) loads with the fresh-install answer for each,
    /// rather than failing and taking the file with it.
    #[test]
    fn a_partial_layer_block_keeps_fresh_install_defaults() {
        let prefs: LayerPrefs =
            serde_json::from_str(r#"{"vehicles": true}"#).expect("a partial layer block must load");
        assert!(prefs.buildings, "buildings are on for a fresh install");
        assert!(prefs.map);
        assert!(prefs.vehicles, "what the file did say is kept");
        assert!(!prefs.height, "the height layer is opt-in");
        assert_eq!(
            prefs.street_labels,
            crate::quality::budget().detail_layers,
            "detail layers follow the device's budget, not a constant"
        );
    }

    /// A location the camera could never have been at must not be restored: this
    /// runs before there is any UI to escape with, so a garbled file would teleport
    /// into nowhere on every launch, forever.
    #[test]
    fn an_impossible_saved_location_is_refused() {
        let at = |lat, lng, altitude| SavedLocation {
            lat,
            lng,
            altitude,
            pose: CameraPose::default(),
        };
        assert!(plausible_start(&at(53.21, 6.56, 120.0)));
        assert!(
            plausible_start(&at(0.0, 0.0, 0.0)),
            "null island is a place"
        );
        assert!(!plausible_start(&at(f64::NAN, 6.56, 120.0)));
        assert!(!plausible_start(&at(53.21, f64::NAN, 120.0)));
        assert!(!plausible_start(&at(53.21, 6.56, f64::NAN)));
        assert!(!plausible_start(&at(91.0, 6.56, 120.0)));
        assert!(!plausible_start(&at(53.21, 400.0, 120.0)));
        assert!(!plausible_start(&at(53.21, 6.56, 1e30)));
        assert!(
            plausible_start(&at(53.21, 6.56, 30_000_000.0)),
            "a genuine deep-space viewpoint is not garbage"
        );
    }

    /// Every file in the wild predates the version field, and must read as 0 — the
    /// signal a future migration branches on. A fresh store carries the current one
    /// instead, so it is never mistaken for a legacy document.
    #[test]
    fn version_is_zero_on_legacy_files_and_current_on_a_fresh_store() {
        let legacy: UserData = serde_json::from_str("{}").unwrap();
        assert_eq!(legacy.version, 0);
        let mut migrated = legacy;
        migrate(&mut migrated);
        assert_eq!(migrated.version, USER_DATA_VERSION);
        assert_eq!(fresh_data().version, USER_DATA_VERSION);
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
