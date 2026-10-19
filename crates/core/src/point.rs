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
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Point {
    pub latitude: f64,
    pub longitude: f64,
    /// What the place is called, if the sender knew ("Stadsschouwburg").
    pub name: Option<String>,
    /// A line of text sent with it ("then the show after?").
    pub text: Option<String>,
    /// The camera it was looked at from. `None` for a bare spot.
    pub view: Option<PointView>,
    /// Set when the point is a **meet-up** — a place and a time — or somebody's
    /// answer to one (`docs/mockups/routing/v1`, frame I).
    pub meetup: Option<Meetup>,
}

/// A meet-up: when, and whether this message proposes it or answers "I'm going".
///
/// In ActivityStreams a proposal is an `Event` and the answer an `Accept`, both
/// with the meet-up's `location` and `startTime`. The answer carries the place and
/// the time itself rather than only pointing at the proposal, for two reasons:
/// there are no stable message ids inside an encrypted chat to point at, and an
/// older build — which reads any object with a `Place` as a plain point — then
/// shows "I'm going" at the right spot instead of a message it cannot display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Meetup {
    /// When, as Unix milliseconds, UTC.
    pub start_ms: u64,
    /// `true` for "I'm going", `false` for the proposal.
    pub going: bool,
}

impl Point {
    /// Whether `other` is about the same meet-up as this one: same time, and a
    /// place within a few metres (a round trip through JSON may move the last
    /// decimal).
    #[must_use]
    pub fn same_meetup(&self, other: &Point) -> bool {
        match (self.meetup, other.meetup) {
            (Some(a), Some(b)) => {
                a.start_ms == b.start_ms
                    && (self.latitude - other.latitude).abs() < 5e-5
                    && (self.longitude - other.longitude).abs() < 5e-5
            }
            _ => false,
        }
    }
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
    /// A meet-up's time, as ActivityStreams' `xsd:dateTime` (UTC, `Z`).
    #[serde(rename = "startTime", skip_serializing_if = "Option::is_none", default)]
    start_time: Option<String>,
}

/// Unix milliseconds as an RFC 3339 UTC timestamp (`2026-10-06T17:00:00Z`).
///
/// By hand rather than through a date crate: this crate is shared with the
/// simulator and every target, and one conversion each way is all it needs.
#[must_use]
pub fn rfc3339_utc(ms: u64) -> String {
    let secs = ms / 1000;
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (y, m, d) = civil_from_days(days as i64);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Read an RFC 3339 timestamp back to Unix milliseconds. Accepts a `Z` or a
/// numeric offset, and ignores fractional seconds.
#[must_use]
pub fn parse_rfc3339(s: &str) -> Option<u64> {
    let s = s.trim();
    let (date, rest) = s.split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>().ok());
    let (y, mo, da) = (d.next()??, d.next()??, d.next()??);
    let (time, offset_s): (&str, i64) = if let Some(t) = rest.strip_suffix('Z') {
        (t, 0)
    } else {
        let at = rest.rfind(['+', '-'])?;
        let (t, off) = rest.split_at(at);
        let sign = if off.starts_with('-') { -1 } else { 1 };
        let mut o = off[1..].split(':').map(|p| p.parse::<i64>().ok());
        let hours = o.next()??;
        let minutes = o.next().flatten().unwrap_or(0);
        (t, sign * (hours * 3600 + minutes * 60))
    };
    let time = time.split('.').next()?;
    let mut t = time.split(':').map(|p| p.parse::<i64>().ok());
    let (h, mi) = (t.next()??, t.next()??);
    let se = t.next().flatten().unwrap_or(0);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&da) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let secs = days_from_civil(y, mo, da) * 86_400 + h * 3600 + mi * 60 + se - offset_s;
    u64::try_from(secs).ok().map(|s| s * 1000)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// algorithm).
pub(crate) fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The inverse of [`days_from_civil`].
pub(crate) fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400;
    (if m <= 2 { y + 1 } else { y }, m, d)
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
            kind: match p.meetup {
                None => "Note",
                Some(Meetup { going: false, .. }) => "Event",
                Some(Meetup { going: true, .. }) => "Accept",
            }
            .into(),
            start_time: p.meetup.map(|m| rfc3339_utc(m.start_ms)),
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
        // A meet-up is an `Event` or `Accept` with a readable `startTime`; any
        // other object with a place — a Note from elsewhere, an Event with no
        // time — is a plain point.
        let meetup = note
            .start_time
            .as_deref()
            .and_then(parse_rfc3339)
            .and_then(|start_ms| match note.kind.as_str() {
                "Event" => Some(Meetup {
                    start_ms,
                    going: false,
                }),
                "Accept" => Some(Meetup {
                    start_ms,
                    going: true,
                }),
                _ => None,
            });
        let point = Point {
            latitude: note.location.latitude,
            longitude: note.location.longitude,
            name: note.location.name,
            text: note.content,
            view: note.view,
            meetup,
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
            meetup: None,
        }
    }

    #[test]
    fn timestamps_round_trip_and_read_offsets() {
        for ms in [0u64, 1_791_306_000_000, 951_782_400_000, 4_102_444_799_000] {
            assert_eq!(
                parse_rfc3339(&rfc3339_utc(ms)),
                Some(ms),
                "{}",
                rfc3339_utc(ms)
            );
        }
        assert_eq!(rfc3339_utc(1_791_306_000_000), "2026-10-06T17:00:00Z");
        assert_eq!(
            parse_rfc3339("2026-10-06T19:00:00+02:00"),
            parse_rfc3339("2026-10-06T17:00:00Z")
        );
        assert_eq!(
            parse_rfc3339("2026-10-06T17:00:00.250Z"),
            Some(1_791_306_000_000)
        );
        assert_eq!(parse_rfc3339("not a date"), None);
        assert_eq!(parse_rfc3339("2026-13-01T00:00:00Z"), None);
    }

    #[test]
    fn a_meetup_is_an_event_and_its_answer_an_accept() {
        let mut p = schouwburg();
        p.meetup = Some(Meetup {
            start_ms: 1_791_306_000_000,
            going: false,
        });
        let json = p.to_activity_json(None).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "Event");
        assert_eq!(v["startTime"], "2026-10-06T17:00:00Z");
        let back = Point::from_activity_json(&json).unwrap();
        assert_eq!(back, p);

        let mut going = p.clone();
        going.meetup = Some(Meetup {
            start_ms: 1_791_306_000_000,
            going: true,
        });
        going.text = Some("I'm going".into());
        let json = going.to_activity_json(None).unwrap();
        assert!(json.contains("\"Accept\""));
        let back = Point::from_activity_json(&json).unwrap();
        assert!(back.same_meetup(&p) && back.meetup.unwrap().going);
    }

    #[test]
    fn a_plain_note_with_a_start_time_is_still_a_plain_point() {
        let json = r#"{"type":"Note","startTime":"2026-10-06T17:00:00Z",
            "location":{"type":"Place","latitude":53.2,"longitude":6.5}}"#;
        assert_eq!(Point::from_activity_json(json).unwrap().meetup, None);
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
