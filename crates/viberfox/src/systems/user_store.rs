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
use crate::systems::free_camera::{FreeCamera, WORLD_CELL_EDGE};
use crate::systems::geo_nav::{world_to_lat_lng, CameraPose, GeoNavState, Teleport};
use crate::systems::map_stream::MapStream;

/// Seconds between autosave snapshots of the current location.
const AUTOSAVE_INTERVAL: f32 = 5.0;

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct UserData {
    pub last_location: Option<SavedLocation>,
    pub bookmarks: Vec<Bookmark>,
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
    mut exits: EventReader<AppExit>,
    mut store: ResMut<UserStore>,
    stream: Res<MapStream>,
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
        let wx = cell.x as f64 * WORLD_CELL_EDGE as f64 + tf.translation.x as f64;
        let wz = cell.z as f64 * WORLD_CELL_EDGE as f64 + tf.translation.z as f64;
        let (lat, lng) = world_to_lat_lng(anchor, wx, wz);
        let altitude = cell.y as f64 * WORLD_CELL_EDGE as f64 + tf.translation.y as f64;
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
    }
}
