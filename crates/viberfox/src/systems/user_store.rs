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
    pub bookmarks: Vec<Bookmark>,
    /// ADR-016: session tokens keyed by server address, so an external sim only
    /// asks for a password once. Not stored for solo mode (ephemeral port).
    #[serde(default)]
    pub session_tokens: std::collections::HashMap<String, String>,
    /// The local player's chosen avatar body + clothing. `#[serde(default)]`
    /// keeps pre-appearance files loadable (they get Man/Casual).
    #[serde(default)]
    pub appearance: AvatarAppearance,
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
}

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
            .push(Bookmark { id, name, lat, lng, altitude, pose });
        self.dirty = true;
        id
    }
}

/// Once the world anchor exists, route the last location through the normal
/// teleport path, which restores its camera pose (mode + orientation) too.
pub fn restore_last_location(
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
pub fn autosave_user_store(
    time: Res<Time>,
    mut next_at: Local<f32>,
    mut exits: MessageReader<AppExit>,
    mut store: ResMut<UserStore>,
    stream: Res<MapStream>,
    globe: Res<crate::systems::globe::GlobeState>,
    camera_state: Res<CameraState>,
    cameras: Query<(&GridCell, &Transform), With<FreeCamera>>,
) {
    let exiting = exits.read().next().is_some();
    let due = time.elapsed_secs() >= *next_at;
    if !exiting && !due && !store.dirty {
        return;
    }
    if due {
        *next_at = time.elapsed_secs() + AUTOSAVE_INTERVAL;
    }

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
    }

    #[test]
    fn session_token_get_set_is_idempotent() {
        let mut store = UserStore {
            data: UserData::default(),
            dirty: false,
            path: PathBuf::from("/tmp/viberfox-test-user.json"),
            last_written: String::new(),
        };
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
