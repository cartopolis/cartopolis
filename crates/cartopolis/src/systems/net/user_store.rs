//! Persistent per-user data: last location/camera and bookmarks, stored as JSON
//! under the key `user.json` in the Data namespace of
//! [`crate::platform::storage`]. The start of the user-information store that will
//! later grow OpenSim-style inventory — deliberately simple for now.
//!
//! **It is a storage key, not a path, and that is what makes the web build
//! persistent.** Natively the key resolves to a file under
//! [`crate::utils::cache::app_data_root`], exactly where it has always been; on the
//! web it is an IndexedDB record, where until 2026-08 there was nothing at all —
//! every `std::fs` call failed and the app silently ran on defaults every reload.
//! The synchronous read below works on both because the web build pulls this key
//! into memory before the `App` is built (`platform::storage::preload`).
//!
//! **The location is resolved, not literal, and that is load-bearing.** It used to
//! be a cwd-relative `data/user.json`, which on Android meant the file was never
//! written at all (a NativeActivity process starts in `/`; every autosave failed
//! into a `warn!`) and on desktop meant the profile you got depended on the
//! directory you launched from. `app_data_root` keeps an existing checkout `data/`
//! so nothing moves under a dev box, and answers with a real per-user directory
//! everywhere else.
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

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::grid::AnchoredCamera;
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
    /// Where the local avatar was standing when a session last had one. Separate
    /// from `last_location` because that is the *camera*, which in Follow sits a
    /// boom's length behind the body and in Earth or Fly can be anywhere — and a
    /// fresh sign-in otherwise lands the avatar on the anchor's tile centre
    /// (`session::keep_avatar_across_reconnect`, which puts it back).
    #[serde(default)]
    pub last_avatar: Option<SavedAvatar>,
    /// The user's Home location — set from the status panel's Set Home button
    /// and jumped to by the nav bar's Home button. Camera pose included (like a
    /// bookmark) so Home returns to the exact view, not just the spot.
    #[serde(default)]
    pub home: Option<SavedLocation>,
    /// The Settings panel's own preferences — units, turn chimes,
    /// keep-screen-on, the quality preset. Kept here rather than in `LayerPrefs`
    /// because that describes what is *drawn*; this describes how the app should
    /// behave, and the two get changed for different reasons.
    #[serde(default)]
    pub settings: crate::resources::AppPrefs,
    #[serde(default)]
    pub bookmarks: Vec<Bookmark>,
    /// The directory tree the bookmarks hang in (see [`PlaceFolder`]).
    /// `#[serde(default)]` keeps pre-folder files loadable — every bookmark in
    /// one is simply at the top level.
    #[serde(default)]
    pub folders: Vec<PlaceFolder>,
    /// Serial number for photo file names, only ever increasing.
    ///
    /// A photo's files could be named after the place alone, but then replacing
    /// one would write new bytes to a name egui has already uploaded as a texture
    /// — and a texture is cached by name, so the row would keep showing the old
    /// photo until the app restarted. A fresh stem per capture makes the cache
    /// key change with the picture, which is the property that matters.
    #[serde(default)]
    pub photo_seq: u64,
    /// ADR-016: session tokens keyed by server address, so an external sim only
    /// asks for a password once.
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
    /// The bearer token an editor presents to this client's MCP server
    /// (`--mcp-listen`, `systems::mcp`).
    ///
    /// **Here rather than in a file of its own, and that is the point.** A token
    /// written somewhere beside the app data would be a secret nobody can find:
    /// the person who needs it is configuring an editor on the same machine, and
    /// what they need is to *see* it and copy it. It is in `user.json` because it
    /// belongs to this installation, and it is shown in Settings ▸ General
    /// because that is where somebody would look for it.
    ///
    /// `None` until the first run that serves MCP, which mints one. Regenerating
    /// it from Settings invalidates whatever an editor was holding, which is the
    /// whole of "revoke". `CARTO_MCP_TOKEN` overrides it for a run that has no
    /// screen to read it off.
    #[serde(default)]
    pub mcp_token: Option<String>,
    /// Whose words an agent driving this client may read (`systems::mcp::agent`):
    /// usernames separated by commas, or `*` for everyone. `None` and the empty
    /// string both mean nobody — the agent then sees how many lines arrived and
    /// never what they said. Edited in Settings ▸ General ▸ Editor access;
    /// `--mcp-allow` overrides it for a run.
    #[serde(default)]
    pub mcp_allow: Option<String>,
    /// Which of Chats and World was showing, and which chat was open, so a phone
    /// that Android stopped in the background comes back to the same screen
    /// (`chats::remember_screen`, `chats::restore_screen`). `None` on files from
    /// before the field existed, and on every run that never signed in.
    #[serde(default)]
    pub screen: Option<SavedScreen>,
}

/// See [`UserData::screen`].
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct SavedScreen {
    /// The Chats tab was up (the chat list, or a chat in its Chat view).
    #[serde(default)]
    pub chats: bool,
    /// The chat that was open, by channel key.
    #[serde(default)]
    pub open: Option<String>,
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
    /// The moon's age in days. `Some` only in [`SunMode::Manual`], for the same
    /// reason as `time_of_day`: every other mode derives it from the real date.
    /// Without it a hand-set moon phase came back as the full-moon default.
    #[serde(default)]
    pub moon_age_days: Option<f32>,
    /// "Run the day" — Manual's quick cycle. Off by default; only acts in Manual.
    #[serde(default)]
    pub auto_cycle: bool,
    /// Real seconds per loop of the quick cycle. Defaulted to
    /// `DevPanelState::default`'s 10, not to serde's 0 (which the cycle clamps to
    /// a 0.1 s day).
    #[serde(default = "default_cycle_seconds")]
    pub cycle_seconds: f32,
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
    /// Whether the cloud shells are drawn at all (Settings ▸ World ▸ Weather).
    ///
    /// `default_true`, not a bare `#[serde(default)]`, and this is the field that
    /// most needed the rule in this struct's doc comment: a `bool`'s serde zero is
    /// `false`, so shipping this with the bare attribute would read every
    /// `user.json` written before it existed as "the user turned clouds off" and
    /// silently take the sky away from everybody who upgraded.
    #[serde(default = "default_true")]
    pub clouds: bool,
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
    /// Near-field grass (`systems::grass`).
    #[serde(default = "detail_default")]
    pub grass: bool,
    #[serde(default = "detail_default")]
    pub roads_textured: bool,
    #[serde(default = "detail_default")]
    pub water_shading: bool,
    /// Street names over the map (`systems::street_labels`).
    #[serde(default = "detail_default")]
    pub street_labels: bool,
    /// The flood defences, named where they stand (`systems::dikes`).
    ///
    /// On by default rather than on the detail tier: it is a handful of names
    /// written by a layer that is already running, it costs one small file per
    /// cell, and outside the country that publishes the register there is nothing
    /// to draw and therefore nothing to pay for.
    #[serde(default = "yes")]
    pub flood_defences: bool,
    /// The sewers, and the opened ground (`systems::sewers`). Off unless asked
    /// for: it changes how the ground looks.
    #[serde(default)]
    pub sewers: bool,
    /// The other underground layers by `underground::LAYERS` key. A map rather
    /// than a field each, so a layer added later needs no field here; absent is off.
    #[serde(default)]
    pub underground: std::collections::BTreeMap<String, bool>,
    /// Street furniture (`systems::street_furniture`).
    #[serde(default = "detail_default")]
    pub furniture: bool,
    /// The **Life** group master: everything that moves (`data_layers::
    /// DataLayers::life_enabled`). Defaults to on, like the other group masters —
    /// what a fresh install starts *with* is decided by the three rows under it.
    #[serde(default = "yes")]
    pub life: bool,
    /// Ambient cars (`systems::traffic`).
    ///
    /// `serde(default)` on the same helper as the rest, so a `user.json` written
    /// before this layer existed reads as the tier's default rather than as off —
    /// an absent key is "not stated", not "the user turned it off".
    #[serde(default = "detail_default")]
    pub traffic: bool,
    /// Ambient people (`systems::traffic`'s walker pool).
    ///
    /// Split out of `traffic` when the Life group was formed. A file written
    /// before the split carries no key, so this reads as the tier's default —
    /// which is what the one toggle they *did* have meant for the walkers too.
    #[serde(default = "detail_default")]
    pub people: bool,
    /// Your contacts on the map, where their avatars are (`systems::chat_people`).
    /// On by default: seeing where your friends are is the point of having them.
    #[serde(default = "yes")]
    pub friends: bool,
    /// The Overpass street-detail layer (`systems::street_detail`).
    #[serde(default = "detail_default")]
    pub street_detail: bool,
    /// Off on a fresh install: the vehicle layer is opt-in.
    #[serde(default)]
    pub vehicles: bool,
    /// The aerial basemap (`systems::aerial`). Off for the same reason, and one
    /// more: it replaces the drawn ground rather than adding to it.
    #[serde(default)]
    pub aerial: bool,
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

/// `#[serde(default)]` for [`SkyPrefs::cycle_seconds`], in step by hand with
/// `DevPanelState::default`.
fn default_cycle_seconds() -> f32 {
    10.0
}

/// `#[serde(default)]` for [`SkyPrefs::clouds`] — see that field for why a bare
/// `#[serde(default)]` would have been a bug rather than a shorter spelling.
fn default_true() -> bool {
    true
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

/// Where the user was when Cartopolis closed, camera included.
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

/// Where the local avatar stood, as a place on Earth rather than a position in
/// a frame: the map re-anchors between launches, so only lat/lng survives.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
pub struct SavedAvatar {
    pub lat: f64,
    pub lng: f64,
    /// Height in the flat frame, so an avatar left flying comes back flying.
    #[serde(default)]
    pub y: f64,
}

/// The camera mode the startup restore put back, held until the launch's first
/// sign-in reads it (`free_camera::follow_on_sign_in`).
///
/// Offline has no Follow, so a remembered Follow is dropped to Fly on the first
/// frame and the autosave then records "free" — the live mode by the time the
/// session comes up is no longer what the user left. This keeps what the *file*
/// said. `None` when nothing was restored (a fresh install, or a launch link,
/// which names a view of its own) and after the first sign-in has taken it.
#[derive(Resource, Default)]
pub struct RestoredCameraMode(pub Option<String>);

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
    /// The photo attached to this place, as the *stem* of its two files in
    /// [`crate::utils::photo_store`] — never the bytes, and never a full path.
    ///
    /// A stem rather than a file name because the store owns both the extension
    /// and the thumbnail's spelling; not the bytes because this struct is
    /// serialised in full on every autosave tick. `None` is the ordinary case: a
    /// place has no photo until someone takes one.
    #[serde(default)]
    pub photo: Option<String>,
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

/// Pin colour for a place that is in no folder: the brand accent (`#FD6012`,
/// `theme::ACCENT`).
///
/// It was the classic map red (`#E73E36`) — borrowed from every other map, and
/// the only saturated colour in this app that belonged to nothing in it. Every
/// unfoldered place is drawn in it twice, as the pin on the map *and* as the
/// marker or the photo frame in the Places row, so on a fresh install the panel
/// was outlined in a red the rest of the UI never uses, which reads as a warning
/// rather than as a colour. This is the brand orange (`theme::BRAND`), and it
/// stayed orange when the chrome went monochrome: a pin is *map content*, not
/// chrome, it has to be found against parks, water and paper at a glance, and
/// grey is the one thing a map already has plenty of. Colours a user picks for a
/// folder are untouched — [`FOLDER_PALETTE`] below still starts at blue, so a
/// folder never comes up in the same orange the top level is in.
pub const DEFAULT_PIN_COLOR: [f32; 3] = [0.992, 0.376, 0.071];

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
///
/// `Default` is a store that has never been read or written — `last_written`
/// empty, so the first autosave writes. It exists for tests in *other* modules
/// (`session::sign_out`'s, which spans this resource and `SessionState`); nothing
/// in the app constructs one this way, since `load` is what a real store comes
/// from.
#[derive(Resource, Default)]
pub struct UserStore {
    pub data: UserData,
    pub dirty: bool,
    /// Last JSON written, to skip no-op writes.
    last_written: String,
}

/// Storage key of the document, in the Data namespace of
/// [`crate::platform::storage`] — a file under the app data root natively, an
/// IndexedDB record on the web.
pub const USER_KEY: &str = "user.json";

/// Where the store lives, for a human reading a log line: the resolved path
/// natively, the storage key on the web (where IndexedDB has no path to print).
fn user_data_location() -> String {
    #[cfg(not(target_arch = "wasm32"))]
    {
        crate::utils::cache::app_data_root()
            .join(USER_KEY)
            .display()
            .to_string()
    }
    #[cfg(target_arch = "wasm32")]
    {
        format!("IndexedDB cartopolis/data/{USER_KEY}")
    }
}

/// Set an unparseable document aside as `user.json.corrupt-<unix seconds>` and
/// return the key it was kept under, so the log can name something a person can
/// actually recover bookmarks out of by hand.
///
/// Timestamped rather than a single `.corrupt` sibling: a machine that produces
/// one bad file usually produces another, and the second must not overwrite the
/// first copy — which may be the only intact one.
///
/// Written from the bytes already in hand rather than renaming the original,
/// because the store is a key-value seam now and the web half has no rename. The
/// copy therefore lands *before* the fresh store can overwrite the original,
/// which is the property that matters (see the module note).
fn quarantine(bytes: &[u8]) -> Option<String> {
    // `web_time`, not `std::time`: `SystemTime::now()` panics on
    // wasm32-unknown-unknown, and this path runs on the web too now.
    let stamp = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let kept = format!("{USER_KEY}.corrupt-{stamp}");
    crate::platform::storage::write_now(crate::platform::storage::Namespace::Data, &kept, bytes)
        .ok()
        .map(|()| kept)
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
    /// The remembered quality preset, read before anything else exists.
    ///
    /// `quality::init` has to run at the very top of `run()` — it decides how wide
    /// the worker pool may be, and every streamer expects it settled before the
    /// first frame — while the store proper is loaded much later, *after* the
    /// tracing subscriber, so that "user.json unreadable, kept a copy at …" is a
    /// message someone actually sees. Reading the one field early is the cheap way
    /// to satisfy both: a second read of a small JSON file at startup costs
    /// nothing measurable, and it keeps the quarantine warning where it is useful.
    ///
    /// Silent by construction. Any failure here means "no preference", which is
    /// the correct answer for a missing, unreadable or older file, and the real
    /// load a moment later is what reports it.
    ///
    /// **It must not deserialise [`UserData`], and that is not a performance
    /// point.** This function is called as the *argument* to `quality::init`, so
    /// it runs before the tier is set — and `LayerPrefs`' five detail flags carry
    /// `#[serde(default = "detail_default")]`, which calls `quality::budget()`,
    /// which initialises the process-wide `OnceLock` from `detect(None)`. A
    /// `user.json` whose `layers` object is missing any one of those keys
    /// therefore *decided the tier* before `init` was reached; `init`'s
    /// `BUDGET.set()` then failed, and because its log line sits inside that
    /// guard, the whole thing was silent — no `render budget resolved`, and the
    /// user's Settings choice discarded on the one platform that has no
    /// environment variable to override it with. Reading through
    /// [`serde_json::Value`] touches no `#[serde(default)]` on any other field,
    /// which is the property that matters here.
    #[must_use]
    pub fn preferred_quality() -> Option<&'static str> {
        let bytes =
            crate::platform::storage::read_now(crate::platform::storage::Namespace::Data, USER_KEY)
                .ready()?;
        let json: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        let preset: crate::resources::QualityPreset =
            serde_json::from_value(json.get("settings")?.get("quality")?.clone()).ok()?;
        preset.as_env_word()
    }

    /// The persisted bisecting switches, read the same early, narrow way
    /// [`Self::preferred_quality`] reads the tier — and for the same two reasons.
    ///
    /// It runs before the tracing subscriber and before the `App`, because
    /// `CARTO_NO_DUAL_SOURCE` is consulted while `DefaultPlugins` is assembled; and
    /// it goes through [`serde_json::Value`] rather than deserialising [`UserData`]
    /// so that no other field's `#[serde(default)]` runs this early. That is not
    /// caution: `LayerPrefs`' defaults call `quality::budget()`, which would settle
    /// the tier's `OnceLock` from a detection that has not been told the user's
    /// preference yet. See `preferred_quality` for the full account.
    ///
    /// Silent by construction — a missing, unreadable or older file means "no
    /// switches", which is the right answer, and the real load a moment later is
    /// what reports the failure.
    #[must_use]
    pub fn preferred_debug_switches() -> crate::resources::DebugSwitches {
        let read = || {
            let bytes = crate::platform::storage::read_now(
                crate::platform::storage::Namespace::Data,
                USER_KEY,
            )
            .ready()?;
            let json: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
            serde_json::from_value(json.get("settings")?.get("debug_switches")?.clone()).ok()
        };
        read().unwrap_or_default()
    }

    pub fn load() -> Self {
        use crate::platform::storage::{self, Namespace};

        let stored = storage::read_now(Namespace::Data, USER_KEY).ready();
        let (data, last_written) = match stored {
            Some(bytes) => match serde_json::from_slice::<UserData>(&bytes) {
                Ok(mut data) => {
                    migrate(&mut data);
                    (data, String::from_utf8(bytes).unwrap_or_default())
                }
                Err(e) => {
                    // Set the unparseable document aside *before* handing back an
                    // empty store. The store is the only copy of the user's
                    // bookmarks and the autosave's first deadline is time zero, so
                    // without this the original is gone within a frame of the app
                    // starting — and the one situation in which that happens is the
                    // one where something is already wrong and the bytes are worth
                    // keeping.
                    match quarantine(&bytes) {
                        Some(kept) => tracing::warn!(
                            error = %e,
                            %kept,
                            "user.json did not parse; kept a copy and started fresh"
                        ),
                        None => tracing::warn!(
                            error = %e,
                            location = %user_data_location(),
                            "user.json did not parse and could not be set aside; starting fresh"
                        ),
                    }
                    (fresh_data(), String::new())
                }
            },
            // Nothing stored: a first run, or a location we cannot read. Either way
            // there is nothing to preserve, so this is the only path that starts
            // blank without saying anything.
            None => (fresh_data(), String::new()),
        };
        Self {
            data,
            dirty: false,
            last_written,
        }
    }

    /// Write the store out if the serialized form changed.
    pub fn save(&mut self) {
        let Ok(json) = serde_json::to_string_pretty(&self.data) else {
            return;
        };
        if json == self.last_written {
            self.dirty = false;
            return;
        }
        // Durable by the time this returns natively — a temp file and a rename,
        // never a truncating write: this runs every five seconds and on exit, and
        // the exit case is exactly when the process is most likely to be killed
        // part-way through (Android kills backgrounded apps as a matter of
        // routine). A half-written `user.json` is an unparseable one, which used to
        // be indistinguishable from having no bookmarks at all. On the web the
        // IndexedDB write is queued instead, because nothing can block on one; the
        // in-memory view is updated first either way, so a reload that beats the
        // write is the only way to lose it.
        match crate::platform::storage::write_now(
            crate::platform::storage::Namespace::Data,
            USER_KEY,
            json.as_bytes(),
        ) {
            Ok(()) => {
                self.last_written = json;
                self.dirty = false;
            }
            Err(e) => {
                tracing::warn!(error = %e, location = %user_data_location(), "user.json write failed")
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
            photo: None,
        });
        self.dirty = true;
        id
    }

    /// Delete a place, and the photo attached to it.
    ///
    /// The photo has to go with the place: nothing else refers to those two files
    /// ever again, and a photo directory that only grows is a phone filling up
    /// with pictures of places its owner deleted.
    pub fn remove_bookmark(&mut self, id: u64) {
        if let Some(stem) = self
            .data
            .bookmarks
            .iter()
            .find(|b| b.id == id)
            .and_then(|b| b.photo.clone())
        {
            crate::utils::photo_store::remove(&stem);
        }
        self.data.bookmarks.retain(|b| b.id != id);
        self.dirty = true;
    }

    /// Attach a photo to a place, replacing any it already had.
    ///
    /// The store writes the files *before* it takes the new stem, so a failed
    /// write leaves the place pointing at the photo it had rather than at a name
    /// with nothing behind it.
    pub fn set_bookmark_photo(
        &mut self,
        id: u64,
        prepared: &crate::utils::photo::Prepared,
    ) -> Result<(), String> {
        if !self.data.bookmarks.iter().any(|b| b.id == id) {
            return Err("that place is gone".to_string());
        }
        let seq = self.data.photo_seq.wrapping_add(1);
        let stem = format!("place-{id}-{seq}");
        crate::utils::photo_store::write(&stem, prepared)?;

        self.data.photo_seq = seq;
        let previous = self
            .data
            .bookmarks
            .iter_mut()
            .find(|b| b.id == id)
            .and_then(|b| b.photo.replace(stem));
        if let Some(previous) = previous {
            crate::utils::photo_store::remove(&previous);
        }
        self.dirty = true;
        Ok(())
    }

    /// Detach a place's photo and delete its files.
    pub fn clear_bookmark_photo(&mut self, id: u64) {
        let Some(stem) = self
            .data
            .bookmarks
            .iter_mut()
            .find(|b| b.id == id)
            .and_then(|b| b.photo.take())
        else {
            return;
        };
        crate::utils::photo_store::remove(&stem);
        self.dirty = true;
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

/// Everything that reads the loaded document to set the session up.
///
/// **A set rather than three `.after()` edges, because the hazard is the one a
/// fourth restore would silently walk into.** [`autosave_user_store`]'s first
/// deadline is time zero — `next_at` is a `Local<f32>` starting at 0.0, so it
/// saves on the very first frame it runs — and it writes the live resources over
/// `UserStore::data` wholesale. Nothing ordered it against the restores, and
/// measured on 2026-09-22 the executor ran it *first*: the autosave wrote the
/// launch's default `DevPanelState` into `data.sky`, and `restore_sky_prefs`
/// then read back what the autosave had just written rather than what was on
/// disk. A `user.json` seeded with `sun_mode: night, fog_density: 0.42` came out
/// of a single 60-second run as `manual, 0.15`. That is "the day/night setting
/// is not remembered", and `data.layers` was going the same way in the same
/// function — `last_location` escaped only by accident, because `MapAnchor::geo`
/// is still `None` that early and the autosave skips the block that writes it.
///
/// So the invariant is stated once, here: **nothing writes the store until
/// everything that must read it has.** Joining this set is what a new restore
/// system does, and the edge it needs then already exists — the same reason the
/// panel chain in `gui::plugin` is one `.chain()` tuple rather than a dozen scattered
/// links.
///
/// Deliberately *not* fixed by moving the autosave's first deadline: a grace
/// period is a guess about how long a restore takes, and a restore that waits
/// for something (as [`restore_start_location`] waits for the anchor) can
/// outlast any number picked here.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct Restore;

/// Once the world anchor exists, put the session where it should begin, through
/// the normal teleport path (which restores the camera pose — mode and
/// orientation — as well as the spot).
///
/// Two possible starts, and the order matters: a place named by the launch
/// (`?place=…` on the web, `CARTO_PLACE` on desktop) wins over the persisted
/// last location, because a link that names a place has to land there. Only one
/// of them is ever applied, so the two can't fight over the first frames.
pub fn restore_start_location(
    mut done: Local<bool>,
    store: Res<UserStore>,
    mut nav: ResMut<GeoNavState>,
    map_anchor: Res<MapAnchor>,
    mut restored_mode: ResMut<RestoredCameraMode>,
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
        nav.pending_teleport = Some(Teleport::fix(fix).silently());
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

    restored_mode.0 = Some(loc.pose.mode.clone()).filter(|m| !m.is_empty());
    // Silent: restoring where you were last time is the app putting the world
    // back, not the avatar going anywhere. Announcing it would whoosh at every
    // launch.
    nav.pending_teleport = Some(Teleport {
        lat: loc.lat,
        lng: loc.lng,
        altitude: Some(loc.altitude),
        pose: Some(loc.pose.clone()),
        silent: true,
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
        if let Some(m) = p.moon_age_days {
            dev.moon_age_days = m.rem_euclid(crate::utils::sun::LUNAR_CYCLE_DAYS);
        }
        dev.auto_cycle = p.auto_cycle;
        dev.cycle_seconds = p.cycle_seconds.clamp(0.1, 3600.0);
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
    // No CLI arm: `--clouds` seeds *coverage*, which is a different question, and
    // the scripted way to take the shells out of a frame is `CARTO_NO_CLOUDS`.
    dev.clouds_enabled = p.clouds;
}

pub fn autosave_user_store(
    time: Res<Time>,
    mut next_at: Local<f32>,
    mut exits: MessageReader<AppExit>,
    mut lifecycle: MessageReader<bevy::window::AppLifecycle>,
    mut store: ResMut<UserStore>,
    prefs: Res<crate::resources::AppPrefs>,
    globe: Res<crate::systems::globe::GlobeState>,
    camera_state: Res<CameraState>,
    cameras: AnchoredCamera,
    dl: Res<crate::systems::data_layers::DataLayers>,
    dev: Res<crate::resources::DevPanelState>,
    weather: Res<crate::systems::weather::WeatherState>,
    map_anchor: Res<MapAnchor>,
    driver: Res<crate::systems::driver::DriverState>,
) {
    // Two flush triggers, not one. `AppExit` is the desktop's way of leaving, and
    // Android never sends it: the system suspends the process and later kills it
    // outright, so on a phone the only warning you get is `WillSuspend` — and the
    // docs are explicit that you have exactly one frame to react. Without this,
    // every app switch loses up to `AUTOSAVE_INTERVAL` of work, which on the
    // platform where switching away is the normal way to stop using an app means
    // "it forgot my bookmark again" with nothing in the log to say why.
    let suspending = lifecycle
        .read()
        .any(|l| matches!(l, bevy::window::AppLifecycle::WillSuspend));
    let exiting = exits.read().next().is_some() || suspending;
    let due = time.elapsed_secs() >= *next_at;
    if !exiting && !due && !store.dirty {
        return;
    }
    if due {
        *next_at = time.elapsed_secs() + AUTOSAVE_INTERVAL;
    }

    store.data.settings = *prefs;
    store.data.layers = Some(dl.prefs());
    // Both go through `DataLayers`, which sees past driver mode's temporary
    // clear sky to what the user actually set.
    store.data.sky = Some(dl.sky_prefs(&dev, &weather));

    if let (Some(anchor), Some((cell, tf))) = (map_anchor.geo, cameras.get()) {
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
            // Past driver mode's temporary rig to the view the user actually
            // chose — see `DriverState::pose_before_drive`. The *place* is still
            // the live one: coming back to where the drive got to is right, it is
            // only the camera that was borrowed.
            pose: driver
                .pose_before_drive(&camera_state)
                .unwrap_or_else(|| CameraPose::capture(&camera_state)),
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
            moon_age_days: Some(3.5),
            auto_cycle: true,
            cycle_seconds: 42.0,
            weather_live: false,
            fog_density: 0.3,
            cloud_cover: 0.5,
            precipitation: 0.0,
            clouds: true,
        };
        let json = serde_json::to_string(&prefs).unwrap();
        assert!(
            json.contains("\"night\""),
            "modes are named on disk: {json}"
        );
        assert_eq!(serde_json::from_str::<SkyPrefs>(&json).unwrap(), prefs);
    }

    /// A `user.json` written before the clouds toggle existed restores as "clouds
    /// on", not as serde's `false`.
    ///
    /// This is the whole reason [`default_true`] exists, and the failure it guards
    /// is silent and global: every upgrading user would have lost the sky, with
    /// the app reporting a setting they had never touched as their own choice. The
    /// fog default beside it carries the same rule for the same reason, so the two
    /// are asserted together — a future knob added here with a bare
    /// `#[serde(default)]` should fail this test rather than ship.
    #[test]
    fn a_sky_block_from_before_the_clouds_toggle_restores_with_clouds_on() {
        let old = r#"{"sun_mode":"auto","weather_live":true,"cloud_cover":0.5}"#;
        let p: SkyPrefs = serde_json::from_str(old).unwrap();
        assert!(
            p.clouds,
            "a missing toggle means the app's default, not off"
        );
        assert!(
            (p.fog_density - default_fog()).abs() < f32::EPSILON,
            "and the same for fog: {}",
            p.fog_density
        );
        assert!(!p.auto_cycle && p.moon_age_days.is_none());
        assert!((p.cycle_seconds - default_cycle_seconds()).abs() < f32::EPSILON);
    }

    /// The autosave must never write the document before a restore has read it.
    ///
    /// The regression this pins cost the day/night setting: `autosave_user_store`
    /// saves on the first frame it runs, and with nothing ordering it against
    /// `restore_sky_prefs` the executor ran it first, so the restore read the
    /// launch defaults the autosave had just written instead of the file. See
    /// [`Restore`].
    ///
    /// Read out of `systems/net/plugin.rs` (where the restores and the autosave
    /// are registered) rather than run, for the reason
    /// `egui_manager::every_root_ui_panel_is_in_the_chain` is: what has to hold
    /// is a property of the *registration*, and a test that builds its own
    /// two-system schedule would only be re-testing that Bevy honours
    /// `.after()`. It would stay green with the edge deleted from the app, which
    /// is precisely the change that reintroduces the bug.
    #[test]
    fn the_autosave_is_ordered_after_every_restore() {
        let plugin =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/systems/net/plugin.rs");
        let text = std::fs::read_to_string(plugin).expect("systems/net/plugin.rs is readable");

        assert!(
            text.contains(".after(systems::user_store::Restore)"),
            "autosave_user_store must be ordered after the Restore set — without \
             it the autosave's time-zero deadline overwrites the document the \
             restores have not read yet"
        );
        assert!(
            text.contains(".in_set(systems::user_store::Restore)"),
            "the restore systems must join the Restore set, or the edge above \
             orders the autosave after nothing"
        );

        // And every restore is inside it: one registered outside the set gets no
        // edge at all, silently, which is how this began.
        for name in [
            "restore_start_location",
            "restore_layer_prefs",
            "restore_sky_prefs",
        ] {
            let at = text
                .find(&format!("systems::user_store::{name}"))
                .unwrap_or_else(|| panic!("{name} is registered in systems/net/plugin.rs"));
            let set_at = text
                .find(".in_set(systems::user_store::Restore)")
                .expect("checked above");
            assert!(
                at < set_at,
                "{name} is registered after the tuple that joins the Restore set, \
                 so it is not in it"
            );
        }
    }

    fn test_store() -> UserStore {
        UserStore {
            data: UserData::default(),
            dirty: false,
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
        assert!(!prefs.aerial, "the aerial basemap is opt-in");
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
