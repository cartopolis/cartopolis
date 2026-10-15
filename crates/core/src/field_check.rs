//! A **field check**: a photograph of a real place and Cartopolis drawn from
//! the same pose, sent to the agent so it can say where the world is wrong
//! (`docs/mockups/field-check/`).
//!
//! This is the pose half: where the phone was, which way it looked, through
//! which lens, when, and how good each of those was. It travels in the
//! picture's caption, one marked line of JSON under the sender's own note, so
//! a field check needs no protocol change — an encrypted chat picture already
//! carries a caption — and a client that has never heard of it shows the note
//! and a line of JSON rather than nothing.
//!
//! **Its purpose is that the view can be put back exactly**, by the agent,
//! later, as often as it likes: [`FieldPose::preset_toml`] writes it as a
//! `[[shot]]` block for the screenshot harness (`docs/shots.toml` format), with
//! the receiver's numbers in a `[shot.measured]` table the harness keeps as data
//! and never renders.
//!
//! Here rather than in the client because three crates read it: the client
//! writes it, the client's MCP surface hands it to the agent, and the chat
//! engine strips it from what a person sees.

use serde::{Deserialize, Serialize};

/// What opens the pose in a caption: the rest of its line is the JSON. Found
/// anywhere in a line, because a caption with no note is the pose alone and the
/// chat line made of it starts with "Picture: ".
pub const CAPTION_MARKER: &str = "cartopolis:field-check ";

/// The shot group every field check is saved under (`--only field-check`).
pub const GROUP: &str = "field-check";

/// Where a phone held up to take a picture is, above the ground, when nothing
/// measured it. The receiver's own height says where its *antenna* is, and the
/// antenna is wherever the person carries it, so it is recorded beside the
/// pose (`Receiver::altitude_m`) rather than trusted as the camera's height.
pub const EYE_HEIGHT_M: f64 = 1.5;

/// Each half of the picture is this wide for its height: 3:4, portrait, the
/// shape of a phone camera's sensor held upright. The photograph is cropped to
/// it and the render is drawn in it, and [`FieldPose::fov_y_deg`] is the
/// vertical angle of *that* shape — so a re-render must be made at the same
/// shape ([`MANIFEST_HEADER`]'s `size`) to frame the same picture.
pub const PICTURE_ASPECT: f32 = 3.0 / 4.0;

/// The top of `docs/field-checks.toml`, for whoever creates it: the run-wide
/// settings every field check is re-rendered with. The size has the picture's
/// shape ([`PICTURE_ASPECT`]); the tile source is pinned as in the other
/// manifests; the aerial photograph is written beside every render.
pub const MANIFEST_HEADER: &str = "\
# Field checks: places photographed in the field, re-rendered from the pose the
# photograph was taken at. Each [[shot]] is a field check's FieldPose::preset_toml,
# appended by the agent that received it. Re-render one, or all of them:
#
#   cargo shots --shots docs/field-checks.toml --only field-2026-10-15-120311
#   cargo shots --shots docs/field-checks.toml --only field-check
tile_url = \"https://tiles.cartopolis.org/tiles/osm/{z}/{x}/{y}\"
size = \"750x1000\"
out_dir = \"gpu\"
hide_ui = true
mode = \"free\"
settle = 3.0
wait = 180.0
aerial_reference = true
";

/// Which camera took the photograph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Facing {
    /// The back of the phone: the view is where the phone points.
    Rear,
    /// The screen side: the view looks back past the person holding it.
    Front,
}

impl Facing {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Facing::Rear => "rear",
            Facing::Front => "front",
        }
    }
}

/// Where the pose came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PoseSource {
    /// The phone's positioning and orientation sensors at the shutter.
    Sensors,
    /// The app's own camera, on a machine with no sensors (a desktop): the
    /// photograph is then not *of* this pose and the check is a test of the
    /// pipeline, not of the world.
    View,
}

/// What an external GNSS receiver said at the shutter, kept as data. Nothing
/// here is rendered; it is what the agent needs to judge the check and, for
/// the height, to compare the ground with the height map.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Receiver {
    /// The receiver's altitude above its own geoid model (NMEA `GGA`), metres.
    /// That geoid is a coarse global one, so this is not a NAP height until
    /// converted with the Dutch geoid; and it is the antenna's height, not the
    /// ground's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub altitude_m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geoid_separation_m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub satellites: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hdop: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correction_age_s: Option<f64>,
    /// The correction stream's mountpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub station: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub station_km: Option<f64>,
    /// One-sigma error east-west and north-south, metres (the receiver's GST).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sigma_m: Option<[f64; 2]>,
    /// One-sigma error up, metres, when the receiver gives one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sigma_up_m: Option<f64>,
}

/// A photograph's pose.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldPose {
    pub lat: f64,
    pub lng: f64,
    /// The camera's height above the ground at (`lat`, `lng`), metres.
    pub height_m: f64,
    /// Which way the view looks: degrees clockwise from north, a compass
    /// heading. The harness's yaw turns the other way ([`Self::harness_yaw_deg`]).
    pub heading_deg: f32,
    /// Up from the horizon, degrees.
    pub pitch_deg: f32,
    /// The turn about the line of sight, degrees: the third angle of the
    /// camera's `YXZ` Euler rotation, read off the phone's attitude in the same
    /// convention the renderer applies it, so it needs no interpretation.
    pub roll_deg: f32,
    /// The vertical field of view of the picture's half, degrees — of the
    /// photograph as cropped to [`PICTURE_ASPECT`], which is also the render's.
    pub fov_y_deg: f32,
    pub camera: Facing,
    /// The shutter, Unix milliseconds UTC.
    pub taken_ms: u64,
    pub source: PoseSource,
    /// One-sigma horizontal position error, metres.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accuracy_m: Option<f32>,
    /// "RTK fixed", "RTK float", "GPS", … as the receiver reported it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heading_accuracy_deg: Option<f32>,
    /// The ground under the camera in the app's height map (AHN, metres NAP),
    /// when it had one there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ground_ahn_m: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver: Option<Receiver>,
    /// The build that drew the render: a render is evidence only about the
    /// code that made it.
    pub client: String,
}

impl FieldPose {
    /// Every number a renderer will be handed is a real one.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        (-90.0..=90.0).contains(&self.lat)
            && (-180.0..=180.0).contains(&self.lng)
            && self.height_m.is_finite()
            && self.heading_deg.is_finite()
            && self.pitch_deg.is_finite()
            && self.roll_deg.is_finite()
            && self.fov_y_deg.is_finite()
            && self.fov_y_deg > 1.0
            && self.fov_y_deg < 170.0
    }

    /// The screenshot harness's yaw for this heading: it turns
    /// counter-clockwise (+Y up, north is −Z, east is +X, so a positive turn
    /// about +Y swings −Z towards −X, west), the opposite of a compass. Kept in
    /// (−180, 180].
    #[must_use]
    pub fn harness_yaw_deg(&self) -> f32 {
        wrap_deg(-self.heading_deg)
    }

    /// The shutter's date and time, UTC.
    #[must_use]
    pub fn taken_utc(&self) -> (i64, i64, i64, u32, u32, u32) {
        let secs = self.taken_ms / 1000;
        let (days, rem) = (secs / 86_400, secs % 86_400);
        let (y, m, d) = crate::point::civil_from_days(days as i64);
        (
            y,
            m,
            d,
            (rem / 3600) as u32,
            ((rem % 3600) / 60) as u32,
            (rem % 60) as u32,
        )
    }

    /// `2026-10-15`, the shutter's date in UTC.
    #[must_use]
    pub fn date(&self) -> String {
        let (y, m, d, ..) = self.taken_utc();
        format!("{y:04}-{m:02}-{d:02}")
    }

    /// The hour of the shutter in UTC, as the harness's `time` takes it.
    #[must_use]
    pub fn hours_utc(&self) -> f32 {
        let (.., h, mi, s) = self.taken_utc();
        h as f32 + mi as f32 / 60.0 + s as f32 / 3600.0
    }

    /// A name that sorts in the order the checks were taken and never repeats
    /// for one person walking: `field-2026-10-15-120311`.
    #[must_use]
    pub fn name(&self) -> String {
        let (y, m, d, h, mi, s) = self.taken_utc();
        format!("field-{y:04}-{m:02}-{d:02}-{h:02}{mi:02}{s:02}")
    }

    /// The caption for the picture: the sender's note, then the pose line.
    #[must_use]
    pub fn caption(&self, note: &str) -> String {
        let json = serde_json::to_string(self).unwrap_or_default();
        let note = note.trim();
        if note.is_empty() {
            format!("{CAPTION_MARKER}{json}")
        } else {
            format!("{note}\n{CAPTION_MARKER}{json}")
        }
    }

    /// The sender's note and the pose, if the caption carries one.
    #[must_use]
    pub fn from_caption(caption: &str) -> Option<(String, FieldPose)> {
        let pose = caption
            .lines()
            .find_map(pose_json)
            .and_then(|json| serde_json::from_str::<FieldPose>(json).ok())
            .filter(FieldPose::is_valid)?;
        Some((display_caption(caption), pose))
    }

    /// The two lines written under the picture, so it means something wherever
    /// it goes without its caption: where, how good, which way; then the lens,
    /// the time and the build.
    #[must_use]
    pub fn strip_lines(&self) -> [String; 2] {
        let ns = if self.lat >= 0.0 { 'N' } else { 'S' };
        let ew = if self.lng >= 0.0 { 'E' } else { 'W' };
        let mut first = format!("{:.5} {ns}  {:.5} {ew}", self.lat.abs(), self.lng.abs());
        match (&self.fix, self.accuracy_m) {
            (Some(f), Some(a)) => first += &format!("  ·  {f}, {}", metres(a)),
            (Some(f), None) => first += &format!("  ·  {f}"),
            (None, Some(a)) => first += &format!("  ·  ± {}", metres(a)),
            (None, None) => {}
        }
        first += &format!("  ·  heading {:.0}°", self.heading_deg.rem_euclid(360.0));
        if let Some(h) = self.heading_accuracy_deg {
            first += &format!(" ± {h:.0}°");
        }
        first += &format!("  ·  pitch {:.0}°", self.pitch_deg);
        if self.roll_deg.abs() >= 0.5 {
            first += &format!("  ·  roll {:.0}°", self.roll_deg);
        }
        let (y, m, d, h, mi, s) = self.taken_utc();
        let mut second = format!(
            "{} camera, {:.0}° high  ·  {y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}Z  ·  client {}",
            self.camera.label(),
            self.fov_y_deg,
            self.client
        );
        if self.source == PoseSource::View {
            second += "  ·  pose from the app's camera, not sensors";
        }
        [first, second]
    }

    /// The pose as a `[[shot]]` block for the screenshot harness, with what was
    /// measured in a `[shot.measured]` table. Appending it to
    /// `docs/field-checks.toml` makes the view one the agent can put the
    /// camera back on at any time: `cargo shots --shots docs/field-checks.toml
    /// --only <name>`.
    #[must_use]
    pub fn preset_toml(&self) -> String {
        let mut t = String::new();
        t += "[[shot]]\n";
        t += &format!("name = {}\n", toml_str(&self.name()));
        t += &format!("group = {}\n", toml_str(GROUP));
        t += &format!(
            "at = [{:.7}, {:.7}, {:.2}]\n",
            self.lat, self.lng, self.height_m
        );
        t += "on_ground = true\n";
        t += &format!(
            "look = [{:.2}, {:.2}]\n",
            self.pitch_deg,
            self.harness_yaw_deg()
        );
        t += &format!("roll = {:.2}\n", self.roll_deg);
        t += &format!("fov = {:.2}\n", self.fov_y_deg);
        t += &format!("date = {}\n", toml_str(&self.date()));
        t += &format!("time = {:.4}\n", self.hours_utc());
        t += "\n[shot.measured]\n";
        t += &format!("heading = {:.2}\n", self.heading_deg);
        t += &format!("camera = {}\n", toml_str(self.camera.label()));
        t += &format!(
            "source = {}\n",
            toml_str(match self.source {
                PoseSource::Sensors => "sensors",
                PoseSource::View => "view",
            })
        );
        t += &format!("client = {}\n", toml_str(&self.client));
        if let Some(a) = self.accuracy_m {
            t += &format!("accuracy_m = {a:.3}\n");
        }
        if let Some(f) = &self.fix {
            t += &format!("fix = {}\n", toml_str(f));
        }
        if let Some(h) = self.heading_accuracy_deg {
            t += &format!("heading_accuracy = {h:.1}\n");
        }
        if let Some(g) = self.ground_ahn_m {
            t += &format!("ground_ahn_m = {g:.2}\n");
        }
        if let Some(r) = &self.receiver {
            if let Some(v) = r.altitude_m {
                t += &format!("receiver_altitude_m = {v:.3}\n");
            }
            if let Some(v) = r.geoid_separation_m {
                t += &format!("geoid_separation_m = {v:.3}\n");
            }
            if let Some(v) = r.satellites {
                t += &format!("satellites = {v}\n");
            }
            if let Some(v) = r.hdop {
                t += &format!("hdop = {v:.2}\n");
            }
            if let Some(v) = r.correction_age_s {
                t += &format!("correction_age_s = {v:.1}\n");
            }
            if let Some(v) = &r.station {
                t += &format!("station = {}\n", toml_str(v));
            }
            if let Some(v) = r.station_km {
                t += &format!("station_km = {v:.1}\n");
            }
            if let Some([e, n]) = r.sigma_m {
                t += &format!("sigma_m = [{e:.3}, {n:.3}]\n");
            }
            if let Some(u) = r.sigma_up_m {
                t += &format!("sigma_up_m = {u:.3}\n");
            }
        }
        t
    }
}

/// The centred crop of a `width × height` picture to [`PICTURE_ASPECT`]:
/// `(x, y, w, h)` in its pixels.
#[must_use]
pub fn picture_crop(width: u32, height: u32) -> (u32, u32, u32, u32) {
    let (w, h) = (width as f32, height as f32);
    if w / h > PICTURE_ASPECT {
        let cw = ((h * PICTURE_ASPECT).round() as u32).clamp(1, width);
        ((width - cw) / 2, 0, cw, height)
    } else {
        let ch = ((w / PICTURE_ASPECT).round() as u32).clamp(1, height);
        (0, (height - ch) / 2, width, ch)
    }
}

/// The vertical field of view of [`picture_crop`] of a picture whose own
/// vertical field of view is `fov_y_deg`: unchanged when the crop takes the
/// sides, narrower when it takes the top and bottom.
#[must_use]
pub fn cropped_fov_y_deg(width: u32, height: u32, fov_y_deg: f32) -> f32 {
    let (_, _, _, ch) = picture_crop(width, height);
    let half = (fov_y_deg.to_radians() / 2.0).tan() * ch as f32 / height.max(1) as f32;
    2.0 * half.atan().to_degrees()
}

/// The vertical field of view to give a camera drawing into a `width ×
/// height` window so that the window's [`picture_crop`] sees `fov_y_deg`
/// vertically — the inverse of [`cropped_fov_y_deg`]. A phone held upright is
/// taller than 3:4, so its window must see more than the picture does.
#[must_use]
pub fn window_fov_y_deg(width: u32, height: u32, fov_y_deg: f32) -> f32 {
    let (_, _, _, ch) = picture_crop(width, height);
    let half = (fov_y_deg.to_radians() / 2.0).tan() * height as f32 / ch.max(1) as f32;
    2.0 * half.atan().to_degrees()
}

/// The pose's JSON in a line, wherever the marker stands in it. Not only at
/// the start: a caption with no note *is* the pose line, and the chat line it
/// becomes is `Picture: cartopolis:field-check {…}` — one line, marker inside.
fn pose_json(line: &str) -> Option<&str> {
    line.find(CAPTION_MARKER)
        .map(|i| &line[i + CAPTION_MARKER.len()..])
}

/// A caption as a person should read it: everything but the pose.
#[must_use]
pub fn display_caption(caption: &str) -> String {
    caption
        .lines()
        .map(|l| match l.find(CAPTION_MARKER) {
            Some(i) => l[..i].trim_end(),
            None => l,
        })
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Whether a caption, or a chat line made of one, is a field check's.
#[must_use]
pub fn is_field_check(caption: &str) -> bool {
    caption.lines().any(|l| pose_json(l).is_some())
}

fn wrap_deg(d: f32) -> f32 {
    let w = d.rem_euclid(360.0);
    if w > 180.0 {
        w - 360.0
    } else {
        w
    }
}

fn metres(m: f32) -> String {
    if m < 1.0 {
        format!("{:.0} cm", (m * 100.0).max(1.0))
    } else {
        format!("{m:.1} m")
    }
}

/// A TOML basic string. Control characters are dropped rather than escaped:
/// nothing here should carry one, and a stray one must not break the file.
fn toml_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grote_markt() -> FieldPose {
        FieldPose {
            lat: 53.2183,
            lng: 6.5679,
            height_m: EYE_HEIGHT_M,
            heading_deg: 9.0,
            pitch_deg: 14.0,
            roll_deg: 0.4,
            fov_y_deg: 68.0,
            camera: Facing::Rear,
            // 2026-10-15T12:03:11Z
            taken_ms: 1_792_065_791_000,
            source: PoseSource::Sensors,
            accuracy_m: Some(0.012),
            fix: Some("RTK fixed".into()),
            heading_accuracy_deg: Some(3.0),
            ground_ahn_m: Some(6.97),
            receiver: Some(Receiver {
                altitude_m: Some(48.6),
                geoid_separation_m: Some(41.0),
                satellites: Some(31),
                hdop: Some(0.6),
                correction_age_s: Some(1.0),
                station: Some("DELZ00NLD0".into()),
                station_km: Some(26.0),
                sigma_m: Some([0.009, 0.011]),
                sigma_up_m: Some(0.02),
            }),
            client: "410d46f4".into(),
        }
    }

    #[test]
    fn a_picture_is_cropped_to_three_by_four_and_its_lens_follows() {
        // Already 3:4: nothing cut, nothing changes.
        assert_eq!(picture_crop(1440, 1920), (0, 0, 1440, 1920));
        assert!((cropped_fov_y_deg(1440, 1920, 70.0) - 70.0).abs() < 1e-3);
        // A tall phone window: the top and bottom go, the angle narrows.
        assert_eq!(picture_crop(1080, 2400), (0, 480, 1080, 1440));
        let narrowed = cropped_fov_y_deg(1080, 2400, 100.0);
        assert!(narrowed < 100.0);
        // …and the window's lens is the inverse: give the window this, and its
        // crop sees exactly what the photograph did.
        let window = window_fov_y_deg(1080, 2400, narrowed);
        assert!((window - 100.0).abs() < 1e-3, "{window}");
        // A landscape desktop window: the sides go, the angle is kept.
        assert_eq!(picture_crop(1600, 900), (462, 0, 675, 900));
        assert!((window_fov_y_deg(1600, 900, 60.0) - 60.0).abs() < 1e-3);
    }

    #[test]
    fn the_shutter_time_reads_back_as_a_date_and_an_hour() {
        let p = grote_markt();
        assert_eq!(p.taken_utc(), (2026, 10, 15, 12, 3, 11));
        assert_eq!(p.date(), "2026-10-15");
        assert!((p.hours_utc() - (12.0 + 3.0 / 60.0 + 11.0 / 3600.0)).abs() < 1e-4);
        assert_eq!(p.name(), "field-2026-10-15-120311");
    }

    #[test]
    fn a_compass_heading_is_the_harness_yaw_turned_the_other_way() {
        let mut p = grote_markt();
        for (heading, yaw) in [
            (0.0, 0.0),
            (9.0, -9.0),
            (90.0, -90.0),
            (270.0, 90.0),
            (180.0, 180.0),
        ] {
            p.heading_deg = heading;
            assert!(
                (p.harness_yaw_deg() - yaw).abs() < 1e-4,
                "heading {heading} gave yaw {}",
                p.harness_yaw_deg()
            );
        }
    }

    #[test]
    fn the_pose_rides_in_the_caption_under_the_note() {
        let p = grote_markt();
        let caption = p.caption("  the tower is all wrong ");
        assert!(caption.starts_with("the tower is all wrong\n"));
        assert!(is_field_check(&caption));
        let (note, back) = FieldPose::from_caption(&caption).expect("reads back");
        assert_eq!(note, "the tower is all wrong");
        assert_eq!(back, p);
        assert_eq!(display_caption(&caption), "the tower is all wrong");
    }

    #[test]
    fn a_caption_with_no_note_is_the_pose_alone() {
        let caption = grote_markt().caption("");
        assert!(caption.starts_with(CAPTION_MARKER));
        assert_eq!(display_caption(&caption), "");
        assert_eq!(FieldPose::from_caption(&caption).unwrap().0, "");
    }

    #[test]
    fn the_pose_is_found_in_the_chat_line_a_caption_with_no_note_becomes() {
        // `cartopolis_chat::picture_caption` puts the caption after a prefix.
        let line = format!("Picture: {}", grote_markt().caption(""));
        assert!(is_field_check(&line));
        let (note, pose) = FieldPose::from_caption(&line).expect("found mid-line");
        assert_eq!(pose, grote_markt());
        assert_eq!(note, "Picture:");
    }

    #[test]
    fn an_ordinary_caption_is_not_a_field_check() {
        assert!(!is_field_check("look at this"));
        assert!(FieldPose::from_caption("look at this").is_none());
        assert_eq!(display_caption("look at this"), "look at this");
    }

    #[test]
    fn a_pose_that_is_not_a_place_is_refused() {
        let mut p = grote_markt();
        p.lat = 91.0;
        assert!(FieldPose::from_caption(&p.caption("x")).is_none());
        let mut p = grote_markt();
        p.fov_y_deg = f32::NAN;
        assert!(FieldPose::from_caption(&p.caption("x")).is_none());
    }

    #[test]
    fn the_preset_carries_the_view_and_the_measurements_apart() {
        let t = grote_markt().preset_toml();
        assert!(t.starts_with(
            "[[shot]]\nname = \"field-2026-10-15-120311\"\ngroup = \"field-check\"\n"
        ));
        assert!(
            t.contains("at = [53.2183000, 6.5679000, 1.50]\non_ground = true\n"),
            "{t}"
        );
        assert!(t.contains("look = [14.00, -9.00]\n"), "{t}");
        assert!(
            t.contains("roll = 0.40\nfov = 68.00\ndate = \"2026-10-15\"\n"),
            "{t}"
        );
        let (view, measured) = t
            .split_once("\n[shot.measured]\n")
            .expect("a measured table");
        for key in [
            "heading",
            "accuracy_m",
            "station",
            "ground_ahn_m",
            "sigma_m",
        ] {
            assert!(
                !view.contains(&format!("\n{key} =")),
                "{key} leaked into the view"
            );
            assert!(measured.contains(&format!("{key} =")), "{key} missing");
        }
    }

    #[test]
    fn a_quote_in_a_station_name_cannot_break_the_file() {
        let mut p = grote_markt();
        p.receiver.as_mut().unwrap().station = Some("A\"B\\C\n".into());
        assert!(p.preset_toml().contains("station = \"A\\\"B\\\\C\"\n"));
    }

    #[test]
    fn the_strip_says_where_how_well_and_when() {
        let [a, b] = grote_markt().strip_lines();
        assert_eq!(
            a,
            "53.21830 N  6.56790 E  ·  RTK fixed, 1 cm  ·  heading 9° ± 3°  ·  pitch 14°"
        );
        assert_eq!(
            b,
            "rear camera, 68° high  ·  2026-10-15 12:03:11Z  ·  client 410d46f4"
        );
    }
}
