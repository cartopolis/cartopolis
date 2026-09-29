//! A **point**: a place in the world, seen from somewhere, sent into a chat.
//!
//! The one new kind of message the chat-and-world design rests on
//! (`docs/mockups/chat-world/`): a spot,
//! the camera view it was looked at from, a name and an optional line of text.
//! Tapping one puts the receiver's camera where the sender's was.
//!
//! **It is an ActivityStreams object from day one** (decided 2026-09-26): a
//! `Note` whose `location` is a `Place` with latitude and longitude, plus one
//! extension field, `cartopolis:view`, for the camera. Today a point only travels
//! *inside* an encrypted private chat, where the format is nobody's business but
//! the two clients'. The reason to use the standard shape anyway is the day a
//! point is posted publicly over ActivityPub: converting a home-made format then
//! would mean converting every stored point, and the encrypted ones can only be
//! converted on the phones that hold them. An app that knows nothing of the
//! extension still reads a sensible post: text, a place, and a link that opens
//! the web client at the same view.
//!
//! Here rather than in the client because it is the shared vocabulary: the
//! client writes and reads it now, and a federation service will read it later.

use serde::{Deserialize, Serialize};

/// The extension field's name. A JSON-LD term under our own prefix, so no
/// other vocabulary's reader can mistake it for one of theirs.
pub const VIEW_KEY: &str = "cartopolis:view";

/// Longest text a point carries, in characters. A caption, not an essay.
pub const MAX_POINT_TEXT: usize = 500;

/// Longest place name, in characters.
pub const MAX_POINT_NAME: usize = 120;

/// Where the camera was: height above the ground and the look angles in
/// degrees, the same (pitch, yaw) convention as place links and `docs/shots.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PointView {
    pub altitude: f64,
    pub pitch: f32,
    pub yaw: f32,
}

/// A place, seen from somewhere.
#[derive(Debug, Clone, PartialEq)]
pub struct Point {
    pub latitude: f64,
    pub longitude: f64,
    /// What the place is called, if the sender knew ("Stadsschouwburg").
    pub name: Option<String>,
    /// A line of text sent with it ("then the show after?").
    pub text: Option<String>,
    /// The camera it was looked at from. `None` for a bare spot.
    pub view: Option<PointView>,
}

#[derive(Serialize, Deserialize)]
struct PlaceJson {
    #[serde(rename = "type")]
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    name: Option<String>,
    latitude: f64,
    longitude: f64,
}

#[derive(Serialize, Deserialize)]
struct NoteJson {
    #[serde(rename = "type")]
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    content: Option<String>,
    location: PlaceJson,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    url: Option<String>,
    #[serde(
        rename = "cartopolis:view",
        skip_serializing_if = "Option::is_none",
        default
    )]
    view: Option<PointView>,
}

/// Why a point would not read.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PointError {
    #[error("not a point: {0}")]
    Format(String),
    #[error("not a place on Earth")]
    OutOfRange,
}

fn clip(s: Option<String>, max: usize) -> Option<String> {
    s.map(|s| s.trim().chars().take(max).collect::<String>())
        .filter(|s| !s.is_empty())
}

impl Point {
    /// Bounded and trimmed, so what is written is what any reader will accept.
    #[must_use]
    pub fn tidy(mut self) -> Self {
        self.name = clip(self.name, MAX_POINT_NAME);
        self.text = clip(self.text, MAX_POINT_TEXT);
        self
    }

    fn in_range(&self) -> bool {
        self.latitude.is_finite()
            && self.longitude.is_finite()
            && (-90.0..=90.0).contains(&self.latitude)
            && (-180.0..=180.0).contains(&self.longitude)
            && self
                .view
                .is_none_or(|v| v.altitude.is_finite() && v.pitch.is_finite() && v.yaw.is_finite())
    }

    /// The ActivityStreams JSON. `link` is the web address that opens this view
    /// (`place_link::web_link` on the client), carried as the Note's `url` for
    /// readers that do not know `cartopolis:view`.
    pub fn to_activity_json(&self, link: Option<String>) -> Result<String, PointError> {
        if !self.in_range() {
            return Err(PointError::OutOfRange);
        }
        let p = self.clone().tidy();
        let note = NoteJson {
            kind: "Note".into(),
            content: p.text,
            location: PlaceJson {
                kind: "Place".into(),
                name: p.name,
                latitude: p.latitude,
                longitude: p.longitude,
            },
            url: link,
            view: p.view,
        };
        serde_json::to_string(&note).map_err(|e| PointError::Format(e.to_string()))
    }

    /// Read a point back. Liberal about everything but the place itself: an
    /// object from elsewhere may omit the view, the name or the text, and still
    /// be a point.
    pub fn from_activity_json(json: &str) -> Result<Self, PointError> {
        let note: NoteJson =
            serde_json::from_str(json).map_err(|e| PointError::Format(e.to_string()))?;
        if note.location.kind != "Place" {
            return Err(PointError::Format("location is not a Place".into()));
        }
        let point = Point {
            latitude: note.location.latitude,
            longitude: note.location.longitude,
            name: note.location.name,
            text: note.content,
            view: note.view,
        }
        .tidy();
        if !point.in_range() {
            return Err(PointError::OutOfRange);
        }
        Ok(point)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schouwburg() -> Point {
        Point {
            latitude: 53.2166,
            longitude: 6.5708,
            name: Some("Stadsschouwburg".into()),
            text: Some("then the show after?".into()),
            view: Some(PointView {
                altitude: 120.0,
                pitch: -35.0,
                yaw: 40.0,
            }),
        }
    }

    #[test]
    fn a_point_round_trips_through_its_activitystreams_form() {
        let json = schouwburg().to_activity_json(None).unwrap();
        assert_eq!(Point::from_activity_json(&json).unwrap(), schouwburg());
    }

    #[test]
    fn the_json_is_a_standard_note_with_a_place() {
        let json = schouwburg()
            .to_activity_json(Some(
                "https://cartopolis.org/go?place=53.2166,6.5708,120m".into(),
            ))
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "Note");
        assert_eq!(v["location"]["type"], "Place");
        assert_eq!(v["location"]["latitude"], 53.2166);
        assert_eq!(v["content"], "then the show after?");
        assert!(v["url"].as_str().unwrap().contains("/go?place="));
        assert_eq!(v[VIEW_KEY]["altitude"], 120.0);
    }

    #[test]
    fn a_note_from_elsewhere_without_our_extension_still_reads() {
        let foreign = r#"{"type":"Note","content":"market's back",
            "location":{"type":"Place","latitude":53.2163,"longitude":6.5655},
            "attributedTo":"https://mastodon.nl/@anna"}"#;
        let p = Point::from_activity_json(foreign).unwrap();
        assert_eq!(p.view, None);
        assert_eq!(p.text.as_deref(), Some("market's back"));
    }

    #[test]
    fn a_place_off_the_planet_is_refused_both_ways() {
        let mut p = schouwburg();
        p.latitude = 91.0;
        assert_eq!(p.to_activity_json(None), Err(PointError::OutOfRange));
        let bad = r#"{"type":"Note","location":{"type":"Place","latitude":0,"longitude":999}}"#;
        assert_eq!(Point::from_activity_json(bad), Err(PointError::OutOfRange));
    }

    #[test]
    fn text_and_name_are_trimmed_and_bounded() {
        let mut p = schouwburg();
        p.text = Some(format!("  {}  ", "x".repeat(MAX_POINT_TEXT + 50)));
        p.name = Some("   ".into());
        let back = Point::from_activity_json(&p.to_activity_json(None).unwrap()).unwrap();
        assert_eq!(back.text.unwrap().chars().count(), MAX_POINT_TEXT);
        assert_eq!(back.name, None);
    }
}
