//! The building card's two facade buttons: **Facade wrong**, which has atlas
//! measure the building's walls again from Mapillary's street photographs, and
//! **Facade photo**, which sends a photograph the person takes of it to be
//! measured instead (atlas's `facade_edits`, `pipeline::street_photo`).
//!
//! Either way what comes back is a colour and a material, never a picture:
//! atlas reads the photograph, finds the building in it and drops it. The
//! correction reaches the map the way a building design does — atlas restamps
//! the cell, the manifest poll sees it (asked for at once here), and `lod22`
//! rebuilds the cells it touches.
//!
//! The photograph goes up as the camera wrote it, EXIF and all: its GPS fix and
//! focal length are what atlas starts its search for the camera's pose from.
//! The device's own fix rides along for a camera that wrote none, which is why
//! a location watch is held from the tap to the upload.

use std::sync::Arc;

use bevy::prelude::*;
use bevy::tasks::{IoTaskPool, Task};

use crate::platform::location::LocationWatch;
use crate::platform::photo::{self, Capture, Outcome};

#[derive(Resource, Default)]
pub struct FacadeCheck {
    /// The building being asked about.
    pand: Option<Arc<str>>,
    /// A point in it, for atlas to find its cell by.
    near: (f64, f64),
    capture: Option<Capture>,
    location: Option<LocationWatch>,
    task: Option<Task<Result<String, String>>>,
    /// When to ask atlas again how a measurement is going (seconds of app
    /// time), while it runs: it takes minutes, longer than a phone keeps a
    /// request open.
    ask_at: Option<f64>,
    /// How many times it has been asked.
    asked: u32,
    /// What became of the last request, in words, for the card to show.
    note: Option<(Arc<str>, String)>,
}

impl FacadeCheck {
    /// Whether a request about `pand` is under way.
    #[must_use]
    pub fn busy(&self) -> bool {
        self.capture.is_some() || self.task.is_some() || self.ask_at.is_some()
    }

    /// Whether the camera (or the file dialog) is open for a photograph.
    #[must_use]
    pub fn capturing(&self) -> bool {
        self.capture.is_some()
    }

    /// The line the card shows under its buttons for `pand`.
    #[must_use]
    pub fn note(&self, pand: &str) -> Option<&str> {
        match (&self.note, &self.pand) {
            (Some((p, n)), _) if &**p == pand => Some(n),
            _ => None,
        }
    }

    fn say(&mut self, pand: &Arc<str>, note: impl Into<String>) {
        self.note = Some((pand.clone(), note.into()));
    }

    /// Ask atlas to measure `pand` again from Mapillary.
    pub fn recheck(&mut self, pand: Arc<str>, near: (f64, f64)) {
        if self.busy() {
            return;
        }
        self.say(&pand, "Looking for street photos of this building…");
        let path = format!(
            "/api/pipeline/buildings/{pand}/facade/recheck?lat={}&lng={}",
            near.0, near.1
        );
        self.task = Some(IoTaskPool::get().spawn(async move {
            crate::systems::model_assets::api_post_bytes(
                &path,
                "application/octet-stream",
                Vec::new(),
            )
            .await
        }));
        self.pand = Some(pand);
        self.near = near;
    }

    /// Take or pick a photograph of `pand`'s facade and send it.
    pub fn photo(&mut self, pand: Arc<str>, near: (f64, f64)) {
        if self.busy() {
            return;
        }
        match photo::start() {
            Ok(c) => {
                self.capture = Some(c);
                // Warm from the tap: a cold GNSS fix takes longer than a photo.
                if LocationWatch::available() {
                    self.location = Some(LocationWatch::start());
                }
                self.say(
                    &pand,
                    "Take a photo of the whole front, from across the street",
                );
            }
            Err(e) => self.say(&pand, format!("No camera: {e}")),
        }
        self.pand = Some(pand);
        self.near = near;
    }

    /// Give up on a photograph not yet taken.
    pub fn cancel(&mut self) {
        if let Some(mut c) = self.capture.take() {
            c.cancel();
        }
        self.location = None;
        self.note = None;
    }

    fn send(&mut self, pand: Arc<str>, bytes: Vec<u8>) {
        let fix = self.location.take().and_then(|w| w.latest());
        let mut path = format!(
            "/api/pipeline/buildings/{pand}/facade/photo?lat={}&lng={}",
            self.near.0, self.near.1
        );
        if let Some(f) = fix {
            path.push_str(&format!(
                "&at_lat={}&at_lng={}&accuracy={}",
                f.lat, f.lng, f.accuracy_m
            ));
        }
        self.say(&pand, "Measuring the facade in your photo…");
        self.task = Some(IoTaskPool::get().spawn(async move {
            crate::systems::model_assets::api_post_bytes(&path, "image/jpeg", bytes).await
        }));
    }
}

/// Carry a request on: a photograph taken, an answer come back.
pub fn drive_facade_check(mut check: ResMut<FacadeCheck>, time: Res<Time>) {
    let now = time.elapsed_secs_f64();
    let Some(pand) = check.pand.clone() else {
        return;
    };
    if let Some(capture) = check.capture.as_mut() {
        match capture.poll() {
            Outcome::Pending => {}
            Outcome::Ready(bytes) => {
                check.capture = None;
                check.send(pand.clone(), bytes);
            }
            Outcome::Cancelled => check.cancel(),
            Outcome::Failed(e) => {
                check.capture = None;
                check.location = None;
                check.say(&pand, format!("The photo could not be taken: {e}"));
            }
        }
    }
    if check.task.is_none() && check.ask_at.is_some_and(|t| now >= t) {
        check.ask_at = None;
        check.asked += 1;
        let path = format!("/api/pipeline/buildings/{pand}/facade/status");
        check.task = Some(
            IoTaskPool::get()
                .spawn(async move { crate::systems::model_assets::api_get(&path).await }),
        );
    }
    let Some(task) = check.task.as_mut() else {
        return;
    };
    let Some(result) = crate::platform::http::poll_task(task) else {
        return;
    };
    check.task = None;
    let note = match result {
        Ok(body) if still_measuring(&body) => {
            if check.asked >= MAX_ASKS {
                check.asked = 0;
                "No answer after a quarter of an hour; try again later".to_string()
            } else {
                check.ask_at = Some(now + ASK_EVERY_S);
                "Measuring the facade from street photos; this takes a few minutes…".to_string()
            }
        }
        Ok(body) => {
            check.asked = 0;
            answer(&body)
        }
        Err(e) => {
            tracing::warn!(%pand, "facade check: {e}");
            "The server could not be asked; try again later".to_string()
        }
    };
    check.say(&pand, note);
}

/// How often atlas is asked how a measurement is going, seconds.
const ASK_EVERY_S: f64 = 5.0;
/// Asked this often, it has run a quarter of an hour: something is wrong.
const MAX_ASKS: u32 = 180;

/// Whether atlas says the measurement is still running.
fn still_measuring(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body)
        .is_ok_and(|j| j["status"].as_str() == Some("measuring"))
}

/// The card's line for atlas's answer: `{"facade": …}` when it measured one,
/// `{"facade": null, "why": …}` when it could not.
fn answer(body: &str) -> String {
    let json: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    if json["facade"].is_object() {
        crate::systems::facade_colours::poll_edits_soon();
        let what = match json["facade"]["m"].as_str() {
            Some("brick") => "brick",
            Some("render") => "plaster",
            Some("panel") => "panels",
            _ => "wall",
        };
        format!("Measured again: {what}. The map shows it in a moment")
    } else {
        json["why"]
            .as_str()
            .unwrap_or("Nothing could be measured")
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atlas_answers_read_as_sentences() {
        assert!(
            answer(r#"{"facade":{"c":[150,60,50],"m":"brick","n":4000,"s":"photo"}}"#)
                .starts_with("Measured again: brick")
        );
        assert_eq!(
            answer(r#"{"facade":null,"why":"No street photograph shows enough of this building"}"#),
            "No street photograph shows enough of this building"
        );
        assert_eq!(answer("not json"), "Nothing could be measured");
        assert!(still_measuring(r#"{"status":"measuring"}"#));
        assert!(!still_measuring(r#"{"facade":null,"why":"x"}"#));
    }
}
