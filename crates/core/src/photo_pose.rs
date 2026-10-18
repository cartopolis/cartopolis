//! The **pose of a picture** from the app's camera (`systems::camera_screen`,
//! `docs/mockups/social-camera/`): where the phone (or the view) was, which way
//! it looked, through which lens, when, and how good each of those was.
//!
//! Every picture the camera makes carries it in its own EXIF ([`PhotoPose::
//! exif`], [`PhotoPose::xmp`]), so the file alone is enough to put a camera
//! back on the same view. A picture sent into a chat also carries it in its
//! caption, one marked line of JSON under the sender's own note, which needs
//! no protocol change and is what the agent reads, since it is handed the
//! chat line and not the file. This replaced the field check (2026-10-18),
//! which was a developer tool for exactly that and nothing else.
//!
//! **Its purpose is that the view can be put back exactly**, by the agent,
//! later, as often as it likes: [`PhotoPose::preset_toml`] writes it as a
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
pub const CAPTION_MARKER: &str = "cartopolis:pose ";

/// The marker field checks were sent with before 2026-10-18, still read.
const LEGACY_MARKER: &str = "cartopolis:field-check ";

/// The shot group every picture's preset is saved under (`--only photo`).
pub const GROUP: &str = "photo";

/// Where a phone held up to take a picture is, above the ground, when nothing
/// measured it. The receiver's own height says where its *antenna* is, and the
/// antenna is wherever the person carries it, so it is recorded beside the
/// pose (`Receiver::altitude_m`) rather than trusted as the camera's height.
pub const EYE_HEIGHT_M: f64 = 1.5;

/// The shape of a phone camera's sensor held upright, width over height: 3:4.
/// A picture held sideways is the other way round. [`PhotoPose::aspect`] says
/// which shape a pose's [`PhotoPose::fov_y_deg`] is the vertical angle of, so a
/// re-render must be made at that shape to frame the same picture.
pub const PICTURE_ASPECT: f32 = 3.0 / 4.0;

/// The top of `docs/photos.toml`, for whoever creates it: the run-wide
/// settings every picture is re-rendered with. The size is a phone held
/// upright ([`PICTURE_ASPECT`]); a picture taken sideways says so in its
/// `[shot.measured]` `aspect` and needs a run of its own at that shape. The
/// tile source is pinned as in the other manifests; the aerial photograph is
/// written beside every render.
pub const MANIFEST_HEADER: &str = "\
# Pictures taken with the app's camera, re-rendered from the pose each was
# taken at. Each [[shot]] is a picture's PhotoPose::preset_toml, appended by the
# agent that received it. Re-render one, or all of them:
#
#   cargo shots --shots docs/photos.toml --only photo-2026-10-15-120311
#   cargo shots --shots docs/photos.toml --only photo
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
pub struct PhotoPose {
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
    /// The vertical field of view of the picture, degrees: of the frame
    /// [`Self::aspect`] describes, which in a Mixed picture is each half.
    pub fov_y_deg: f32,
    /// The picture's width over its height (each half's, in a Mixed one).
    /// Field checks before 2026-10-18 were all 3:4 and carry no value.
    #[serde(default = "upright")]
    pub aspect: f32,
    /// How it was taken: "world" (a polaroid of the view), "mixed" (the
    /// photograph beside the view) or "real" (the photograph alone).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
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
    /// The address at the shutter, as the geocoder put it ("Grote Markt 1,
    /// Groningen"), so a picture says where it is to a person and not only to a
    /// renderer. Looked up after the shutter; `None` when the lookup failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<String>,
    /// The build that drew the render: a render is evidence only about the
    /// code that made it.
    pub client: String,
}

fn upright() -> f32 {
    PICTURE_ASPECT
}

/// The longest place name written under a picture, in characters; a longer
/// one is cut with an ellipsis so the line still fits.
const PLACE_MAX_CHARS: usize = 48;

impl PhotoPose {
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
    /// for one person walking: `photo-2026-10-15-120311`.
    #[must_use]
    pub fn name(&self) -> String {
        let (y, m, d, h, mi, s) = self.taken_utc();
        format!("photo-{y:04}-{m:02}-{d:02}-{h:02}{mi:02}{s:02}")
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
    pub fn from_caption(caption: &str) -> Option<(String, PhotoPose)> {
        let pose = caption
            .lines()
            .find_map(pose_json)
            .and_then(|json| serde_json::from_str::<PhotoPose>(json).ok())
            .filter(PhotoPose::is_valid)?;
        Some((display_caption(caption), pose))
    }

    /// The two lines written under the picture, so it means something wherever
    /// it goes without its caption: where and how well; then which way, the
    /// lens, the time and the build.
    #[must_use]
    pub fn strip_lines(&self) -> [String; 2] {
        let ns = if self.lat >= 0.0 { 'N' } else { 'S' };
        let ew = if self.lng >= 0.0 { 'E' } else { 'W' };
        let mut first = String::new();
        if let Some(place) = self.place.as_deref().filter(|p| !p.trim().is_empty()) {
            let place = place.trim();
            if place.chars().count() > PLACE_MAX_CHARS {
                first += &place.chars().take(PLACE_MAX_CHARS - 1).collect::<String>();
                first += "…";
            } else {
                first += place;
            }
            first += "  ·  ";
        }
        first += &format!("{:.5} {ns}  {:.5} {ew}", self.lat.abs(), self.lng.abs());
        match (&self.fix, self.accuracy_m) {
            (Some(f), Some(a)) => first += &format!("  ·  {f}, {}", metres(a)),
            (Some(f), None) => first += &format!("  ·  {f}"),
            (None, Some(a)) => first += &format!("  ·  ± {}", metres(a)),
            (None, None) => {}
        }
        let mut second = format!("heading {:.0}°", self.heading_deg.rem_euclid(360.0));
        if let Some(h) = self.heading_accuracy_deg {
            second += &format!(" ± {h:.0}°");
        }
        second += &format!("  ·  pitch {:.0}°", self.pitch_deg);
        if self.roll_deg.abs() >= 0.5 {
            second += &format!("  ·  roll {:.0}°", self.roll_deg);
        }
        let (y, m, d, h, mi, s) = self.taken_utc();
        second += &format!(
            "  ·  {} camera, {:.0}° high  ·  {y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}Z  ·  client {}",
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
    /// `docs/photos.toml` makes the view one the agent can put the
    /// camera back on at any time: `cargo shots --shots docs/photos.toml
    /// --only <name>`.
    #[must_use]
    pub fn preset_toml(&self) -> String {
        let mut t = String::new();
        t += "[[shot]]\n";
        t += &format!("name = {}\n", toml_str(&self.name()));
        if let Some(place) = &self.place {
            t += &format!("description = {}\n", toml_str(place));
        }
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
        // The shape `fov` is the vertical angle of: a run at another shape
        // frames a different picture (`MANIFEST_HEADER`).
        t += &format!("aspect = {:.4}\n", self.aspect);
        if let Some(m) = &self.mode {
            t += &format!("mode = {}\n", toml_str(m));
        }
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

impl PhotoPose {
    /// The pose as an EXIF block (a little-endian TIFF structure, what a JPEG's
    /// APP1 segment carries after `Exif\0\0`), so a field check saved to a
    /// phone's gallery is a photo the gallery, a map and any EXIF reader place
    /// where it was taken, facing the way it faced.
    ///
    /// - GPS: latitude, longitude, the direction of the view, the horizontal
    ///   error, the UTC date and time, and an altitude when the height map had
    ///   the ground (AHN is metres NAP, which is close enough to sea level for
    ///   EXIF's purpose).
    /// - **The direction is magnetic** for a pose from the sensors: Android's
    ///   rotation vector is referenced to magnetic north and nothing here
    ///   applies the declination. A pose from the app's own view is true north.
    /// - The UserComment is the caption's pose line ([`Self::caption`]), so the
    ///   file alone is enough to put the camera back: `PhotoPose::from_caption`
    ///   reads it as it reads a chat picture's caption.
    #[must_use]
    pub fn exif(&self) -> Vec<u8> {
        let (y, m, d, h, mi, s) = self.taken_utc();
        let stamp = format!("{y:04}:{m:02}:{d:02} {h:02}:{mi:02}:{s:02}");
        let description = self
            .place
            .clone()
            .unwrap_or_else(|| "Cartopolis".to_string());
        let mut comment = b"ASCII\0\0\0".to_vec();
        comment.extend_from_slice(self.caption("").as_bytes());

        let mut gps = vec![
            Tag::bytes(0x0000, &[2, 3, 0, 0]),
            Tag::ascii(0x0001, if self.lat >= 0.0 { "N" } else { "S" }),
            Tag::rationals(0x0002, &dms(self.lat.abs())),
            Tag::ascii(0x0003, if self.lng >= 0.0 { "E" } else { "W" }),
            Tag::rationals(0x0004, &dms(self.lng.abs())),
        ];
        if let Some(ground) = self.ground_ahn_m {
            let altitude = f64::from(ground) + self.height_m;
            gps.push(Tag::bytes(0x0005, &[u8::from(altitude < 0.0)]));
            gps.push(Tag::rationals(0x0006, &[(altitude.abs(), 100)]));
        }
        gps.push(Tag::rationals(
            0x0007,
            &[(f64::from(h), 1), (f64::from(mi), 1), (f64::from(s), 1)],
        ));
        gps.push(Tag::ascii(
            0x0010,
            match self.source {
                PoseSource::Sensors => "M",
                PoseSource::View => "T",
            },
        ));
        gps.push(Tag::rationals(
            0x0011,
            &[(f64::from(self.heading_deg.rem_euclid(360.0)), 100)],
        ));
        gps.push(Tag::ascii(0x001D, &format!("{y:04}:{m:02}:{d:02}")));
        if let Some(a) = self.accuracy_m {
            gps.push(Tag::rationals(0x001F, &[(f64::from(a), 1000)]));
        }
        let mut exif = vec![
            Tag::ascii(0x9003, &stamp),
            Tag::ascii(0x9011, "+00:00"),
            Tag::undefined(0x9286, &comment),
        ];
        // The lens as a 35 mm camera's: the focal length that gives a 24 × 36
        // frame of this shape the same vertical angle. What a gallery shows as
        // "26 mm", and enough with the shape to rebuild the angle.
        if let Some(f) = self.focal_35mm() {
            exif.push(Tag::short(0xA405, f));
        }
        let ifd0 = |exif_at: u32, gps_at: u32| {
            vec![
                Tag::ascii(0x010E, &description),
                // The pixels are already the right way up, whatever way the
                // phone was held: a reader must not turn them again.
                Tag::short(0x0112, 1),
                Tag::ascii(0x0131, &format!("Cartopolis {}", self.client)),
                Tag::ascii(0x0132, &stamp),
                Tag::long(0x8769, exif_at),
                Tag::long(0x8825, gps_at),
            ]
        };
        // The pointers are fixed-size, so the first IFD's size does not depend
        // on their values: measure it with zeros, then lay the three out in
        // order.
        let exif_at = 8 + ifd_len(&ifd0(0, 0));
        let gps_at = exif_at + ifd_len(&exif);
        let mut out = b"II*\0".to_vec();
        out.extend_from_slice(&8u32.to_le_bytes());
        write_ifd(&mut out, &ifd0(exif_at, gps_at));
        write_ifd(&mut out, &exif);
        write_ifd(&mut out, &gps);
        out
    }

    /// The 35 mm-equivalent focal length of [`Self::fov_y_deg`] for a frame of
    /// [`Self::aspect`]: the frame's vertical side on 24 × 36 film is 36 mm
    /// held upright and 24 mm held sideways.
    #[must_use]
    pub fn focal_35mm(&self) -> Option<u16> {
        let side = if self.aspect < 1.0 { 36.0 } else { 24.0 };
        let half = (self.fov_y_deg.to_radians() / 2.0).tan();
        (half > 0.0 && half.is_finite())
            .then(|| (side / 2.0 / half).round())
            .filter(|f| (1.0..=2000.0).contains(f))
            .map(|f| f as u16)
    }

    /// The angles EXIF has no tag for, as an XMP packet: heading, pitch and
    /// roll in this module's own conventions, the lens and the height, under
    /// a namespace of the app's own so no reader mistakes them for another
    /// tool's (a drone's gimbal pitch, for one, counts down from the horizon
    /// to −90). What `jpeg::with_metadata` puts in the file beside the EXIF.
    #[must_use]
    pub fn xmp(&self) -> String {
        let mode = self.mode.as_deref().unwrap_or("");
        format!(
            "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\
             <x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\
             <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
             <rdf:Description rdf:about=\"\" xmlns:cartopolis=\"https://cartopolis.org/ns/pose/1.0/\" \
             cartopolis:HeadingDeg=\"{:.2}\" cartopolis:PitchDeg=\"{:.2}\" \
             cartopolis:RollDeg=\"{:.2}\" cartopolis:FovYDeg=\"{:.3}\" \
             cartopolis:Aspect=\"{:.4}\" cartopolis:HeightM=\"{:.2}\" \
             cartopolis:Latitude=\"{:.8}\" cartopolis:Longitude=\"{:.8}\" \
             cartopolis:Camera=\"{}\" cartopolis:Mode=\"{}\"/>\
             </rdf:RDF></x:xmpmeta><?xpacket end=\"w\"?>",
            self.heading_deg.rem_euclid(360.0),
            self.pitch_deg,
            self.roll_deg,
            self.fov_y_deg,
            self.aspect,
            self.height_m,
            self.lat,
            self.lng,
            self.camera.label(),
            xml_attr(mode),
        )
    }
}

/// Text safe inside an XML attribute.
fn xml_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// One EXIF field: its tag, TIFF type, count and value bytes.
struct Tag {
    tag: u16,
    kind: u16,
    count: u32,
    data: Vec<u8>,
}

impl Tag {
    fn bytes(tag: u16, v: &[u8]) -> Self {
        Self {
            tag,
            kind: 1,
            count: v.len() as u32,
            data: v.to_vec(),
        }
    }
    fn ascii(tag: u16, v: &str) -> Self {
        // EXIF's ASCII is 7-bit in the standard and UTF-8 in practice (every
        // reader in use decodes it so); a NUL inside would end the string early.
        let mut data: Vec<u8> = v.bytes().filter(|b| *b != 0).collect();
        data.push(0);
        Self {
            tag,
            kind: 2,
            count: data.len() as u32,
            data,
        }
    }
    fn short(tag: u16, v: u16) -> Self {
        Self {
            tag,
            kind: 3,
            count: 1,
            data: v.to_le_bytes().to_vec(),
        }
    }
    fn long(tag: u16, v: u32) -> Self {
        Self {
            tag,
            kind: 4,
            count: 1,
            data: v.to_le_bytes().to_vec(),
        }
    }
    /// Each value as a fraction over its own denominator.
    fn rationals(tag: u16, v: &[(f64, u32)]) -> Self {
        let mut data = Vec::with_capacity(v.len() * 8);
        for &(x, den) in v {
            let num = (x * f64::from(den)).round().clamp(0.0, f64::from(u32::MAX)) as u32;
            data.extend_from_slice(&num.to_le_bytes());
            data.extend_from_slice(&den.to_le_bytes());
        }
        Self {
            tag,
            kind: 5,
            count: v.len() as u32,
            data,
        }
    }
    fn undefined(tag: u16, v: &[u8]) -> Self {
        Self {
            tag,
            kind: 7,
            count: v.len() as u32,
            data: v.to_vec(),
        }
    }
    /// Bytes this field takes outside its entry: none when its value fits the
    /// entry's four, else the value padded to an even length.
    fn outside(&self) -> u32 {
        if self.data.len() <= 4 {
            0
        } else {
            (self.data.len() as u32 + 1) & !1
        }
    }
}

/// A coordinate as degrees, minutes and seconds to a ten-thousandth (3 mm).
fn dms(deg: f64) -> [(f64, u32); 3] {
    let d = deg.trunc();
    let m = ((deg - d) * 60.0).trunc();
    let s = (deg - d - m / 60.0) * 3600.0;
    [(d, 1), (m, 1), (s.max(0.0), 10_000)]
}

/// An IFD's whole length: its count, its entries, the next-IFD link and the
/// values that did not fit an entry.
fn ifd_len(tags: &[Tag]) -> u32 {
    2 + 12 * tags.len() as u32 + 4 + tags.iter().map(Tag::outside).sum::<u32>()
}

/// Append one IFD (tags in ascending order, as TIFF requires) with its values
/// right behind it, starting at `out.len()`, and no next IFD.
fn write_ifd(out: &mut Vec<u8>, tags: &[Tag]) {
    let start = out.len() as u32;
    let mut values_at = start + 2 + 12 * tags.len() as u32 + 4;
    let mut values = Vec::new();
    out.extend_from_slice(&(tags.len() as u16).to_le_bytes());
    let mut sorted: Vec<&Tag> = tags.iter().collect();
    sorted.sort_by_key(|t| t.tag);
    for t in sorted {
        out.extend_from_slice(&t.tag.to_le_bytes());
        out.extend_from_slice(&t.kind.to_le_bytes());
        out.extend_from_slice(&t.count.to_le_bytes());
        if t.data.len() <= 4 {
            let mut inline = [0u8; 4];
            inline[..t.data.len()].copy_from_slice(&t.data);
            out.extend_from_slice(&inline);
        } else {
            out.extend_from_slice(&values_at.to_le_bytes());
            values.extend_from_slice(&t.data);
            if t.data.len() % 2 == 1 {
                values.push(0);
            }
            values_at += t.outside();
        }
    }
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&values);
}

/// The centred crop of a `width × height` picture to `aspect` (width over
/// height): `(x, y, w, h)` in its pixels.
#[must_use]
pub fn crop_to(width: u32, height: u32, aspect: f32) -> (u32, u32, u32, u32) {
    let (w, h) = (width as f32, height as f32);
    if w / h > aspect {
        let cw = ((h * aspect).round() as u32).clamp(1, width);
        ((width - cw) / 2, 0, cw, height)
    } else {
        let ch = ((w / aspect).round() as u32).clamp(1, height);
        (0, (height - ch) / 2, width, ch)
    }
}

/// The vertical field of view of [`crop_to`] of a picture whose own vertical
/// field of view is `fov_y_deg`: unchanged when the crop takes the sides,
/// narrower when it takes the top and bottom.
#[must_use]
pub fn cropped_fov_y_deg(width: u32, height: u32, aspect: f32, fov_y_deg: f32) -> f32 {
    let (_, _, _, ch) = crop_to(width, height, aspect);
    let half = (fov_y_deg.to_radians() / 2.0).tan() * ch as f32 / height.max(1) as f32;
    2.0 * half.atan().to_degrees()
}

/// The vertical field of view to give a camera drawing into a `width ×
/// height` window so that the window's [`crop_to`] `aspect` sees `fov_y_deg`
/// vertically — the inverse of [`cropped_fov_y_deg`]. A phone held upright is
/// taller than 3:4, so its window must see more than the picture does.
#[must_use]
pub fn window_fov_y_deg(width: u32, height: u32, aspect: f32, fov_y_deg: f32) -> f32 {
    let (_, _, _, ch) = crop_to(width, height, aspect);
    let half = (fov_y_deg.to_radians() / 2.0).tan() * height as f32 / ch.max(1) as f32;
    2.0 * half.atan().to_degrees()
}

/// The pose's JSON in a line, wherever the marker stands in it. Not only at
/// the start: a caption with no note *is* the pose line, and the chat line it
/// becomes is `Picture: cartopolis:pose {…}` — one line, marker inside.
fn pose_json(line: &str) -> Option<&str> {
    marker_at(line).map(|(i, m)| &line[i + m.len()..])
}

/// Where a marker, the current one or the field check's, stands in a line.
fn marker_at(line: &str) -> Option<(usize, &'static str)> {
    [CAPTION_MARKER, LEGACY_MARKER]
        .into_iter()
        .find_map(|m| line.find(m).map(|i| (i, m)))
}

/// A caption as a person should read it: everything but the pose.
#[must_use]
pub fn display_caption(caption: &str) -> String {
    caption
        .lines()
        .map(|l| match marker_at(l) {
            Some((i, _)) => l[..i].trim_end(),
            None => l,
        })
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Whether a caption, or a chat line made of one, carries a pose.
#[must_use]
pub fn has_pose(caption: &str) -> bool {
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

    fn grote_markt() -> PhotoPose {
        PhotoPose {
            lat: 53.2183,
            lng: 6.5679,
            height_m: EYE_HEIGHT_M,
            heading_deg: 9.0,
            pitch_deg: 14.0,
            roll_deg: 0.4,
            fov_y_deg: 68.0,
            aspect: PICTURE_ASPECT,
            mode: Some("mixed".into()),
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
            place: None,
            client: "410d46f4".into(),
        }
    }

    #[test]
    fn a_picture_is_cropped_to_three_by_four_and_its_lens_follows() {
        // Already 3:4: nothing cut, nothing changes.
        assert_eq!(crop_to(1440, 1920, PICTURE_ASPECT), (0, 0, 1440, 1920));
        assert!((cropped_fov_y_deg(1440, 1920, PICTURE_ASPECT, 70.0) - 70.0).abs() < 1e-3);
        // A tall phone window: the top and bottom go, the angle narrows.
        assert_eq!(crop_to(1080, 2400, PICTURE_ASPECT), (0, 480, 1080, 1440));
        let narrowed = cropped_fov_y_deg(1080, 2400, PICTURE_ASPECT, 100.0);
        assert!(narrowed < 100.0);
        // …and the window's lens is the inverse: give the window this, and its
        // crop sees exactly what the photograph did.
        let window = window_fov_y_deg(1080, 2400, PICTURE_ASPECT, narrowed);
        assert!((window - 100.0).abs() < 1e-3, "{window}");
        // A landscape desktop window: the sides go, the angle is kept.
        assert_eq!(crop_to(1600, 900, PICTURE_ASPECT), (462, 0, 675, 900));
        assert!((window_fov_y_deg(1600, 900, PICTURE_ASPECT, 60.0) - 60.0).abs() < 1e-3);
    }

    #[test]
    fn the_shutter_time_reads_back_as_a_date_and_an_hour() {
        let p = grote_markt();
        assert_eq!(p.taken_utc(), (2026, 10, 15, 12, 3, 11));
        assert_eq!(p.date(), "2026-10-15");
        assert!((p.hours_utc() - (12.0 + 3.0 / 60.0 + 11.0 / 3600.0)).abs() < 1e-4);
        assert_eq!(p.name(), "photo-2026-10-15-120311");
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
        assert!(has_pose(&caption));
        let (note, back) = PhotoPose::from_caption(&caption).expect("reads back");
        assert_eq!(note, "the tower is all wrong");
        assert_eq!(back, p);
        assert_eq!(display_caption(&caption), "the tower is all wrong");
    }

    /// A field check sent before the camera replaced it still reads, as 3:4.
    #[test]
    fn a_field_check_caption_still_reads() {
        let old = "look\ncartopolis:field-check {\"lat\":53.2,\"lng\":6.5,\"height_m\":1.5,\
                   \"heading_deg\":9,\"pitch_deg\":0,\"roll_deg\":0,\"fov_y_deg\":60,\
                   \"camera\":\"rear\",\"taken_ms\":0,\"source\":\"sensors\",\"client\":\"x\"}";
        let (note, pose) = PhotoPose::from_caption(old).expect("reads");
        assert_eq!(note, "look");
        assert!((pose.aspect - PICTURE_ASPECT).abs() < 1e-6);
        assert_eq!(display_caption(old), "look");
    }

    #[test]
    fn a_caption_with_no_note_is_the_pose_alone() {
        let caption = grote_markt().caption("");
        assert!(caption.starts_with(CAPTION_MARKER));
        assert_eq!(display_caption(&caption), "");
        assert_eq!(PhotoPose::from_caption(&caption).unwrap().0, "");
    }

    #[test]
    fn the_pose_is_found_in_the_chat_line_a_caption_with_no_note_becomes() {
        // `cartopolis_chat::picture_caption` puts the caption after a prefix.
        let line = format!("Picture: {}", grote_markt().caption(""));
        assert!(has_pose(&line));
        let (note, pose) = PhotoPose::from_caption(&line).expect("found mid-line");
        assert_eq!(pose, grote_markt());
        assert_eq!(note, "Picture:");
    }

    #[test]
    fn an_ordinary_caption_is_not_a_field_check() {
        assert!(!has_pose("look at this"));
        assert!(PhotoPose::from_caption("look at this").is_none());
        assert_eq!(display_caption("look at this"), "look at this");
    }

    #[test]
    fn a_pose_that_is_not_a_place_is_refused() {
        let mut p = grote_markt();
        p.lat = 91.0;
        assert!(PhotoPose::from_caption(&p.caption("x")).is_none());
        let mut p = grote_markt();
        p.fov_y_deg = f32::NAN;
        assert!(PhotoPose::from_caption(&p.caption("x")).is_none());
    }

    #[test]
    fn the_preset_carries_the_view_and_the_measurements_apart() {
        let t = grote_markt().preset_toml();
        assert!(t.starts_with("[[shot]]\nname = \"photo-2026-10-15-120311\"\ngroup = \"photo\"\n"));
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
        assert_eq!(a, "53.21830 N  6.56790 E  ·  RTK fixed, 1 cm");
        assert_eq!(
            b,
            "heading 9° ± 3°  ·  pitch 14°  ·  rear camera, 68° high  ·  \
             2026-10-15 12:03:11Z  ·  client 410d46f4"
        );
    }

    #[test]
    fn the_place_leads_the_strip_and_a_long_one_is_cut() {
        let mut p = grote_markt();
        p.place = Some("Grote Markt 1, Groningen".into());
        let [a, _] = p.strip_lines();
        assert!(
            a.starts_with("Grote Markt 1, Groningen  ·  53.21830 N"),
            "{a}"
        );
        p.place = Some("x".repeat(80));
        let [a, _] = p.strip_lines();
        assert!(
            a.starts_with(&format!("{}…  ·  ", "x".repeat(PLACE_MAX_CHARS - 1))),
            "{a}"
        );
        // It names the preset too, and an old caption without one still reads.
        p.place = Some("Grote Markt 1, Groningen".into());
        assert!(p
            .preset_toml()
            .contains("description = \"Grote Markt 1, Groningen\"\n"));
        assert_eq!(PhotoPose::from_caption(&p.caption("")).unwrap().1, p);
    }

    /// A field from an EXIF block by walking it as a reader would: IFD0, then
    /// the IFD a pointer tag names. `(type, count, value bytes)`.
    fn exif_field(tiff: &[u8], path: &[u16]) -> (u16, u32, Vec<u8>) {
        let u16_at = |o: usize| u16::from_le_bytes([tiff[o], tiff[o + 1]]);
        let u32_at =
            |o: usize| u32::from_le_bytes([tiff[o], tiff[o + 1], tiff[o + 2], tiff[o + 3]]);
        assert_eq!(&tiff[..4], b"II*\0");
        let mut ifd = u32_at(4) as usize;
        for (i, &want) in path.iter().enumerate() {
            let n = u16_at(ifd) as usize;
            let mut tags = Vec::new();
            let entry = (0..n)
                .map(|k| ifd + 2 + 12 * k)
                .find(|&e| {
                    tags.push(u16_at(e));
                    u16_at(e) == want
                })
                .unwrap_or_else(|| panic!("no tag {want:#06x}"));
            assert!(tags.windows(2).all(|w| w[0] < w[1]), "tags out of order");
            let (kind, count) = (u16_at(entry + 2), u32_at(entry + 4));
            if i + 1 < path.len() {
                ifd = u32_at(entry + 8) as usize;
                continue;
            }
            let size = match kind {
                1 | 2 | 7 => 1,
                3 => 2,
                4 => 4,
                _ => 8,
            } * count as usize;
            let at = if size <= 4 {
                entry + 8
            } else {
                u32_at(entry + 8) as usize
            };
            return (kind, count, tiff[at..at + size].to_vec());
        }
        unreachable!()
    }

    fn rational(b: &[u8], i: usize) -> f64 {
        let n = u32::from_le_bytes(b[i * 8..i * 8 + 4].try_into().unwrap());
        let d = u32::from_le_bytes(b[i * 8 + 4..i * 8 + 8].try_into().unwrap());
        f64::from(n) / f64::from(d)
    }

    #[test]
    fn a_saved_picture_says_where_it_was_taken() {
        let mut p = grote_markt();
        p.place = Some("Grote Markt 1, Groningen".into());
        let tiff = p.exif();
        let (_, _, lat_ref) = exif_field(&tiff, &[0x8825, 0x0001]);
        assert_eq!(lat_ref, b"N\0");
        let (kind, count, lat) = exif_field(&tiff, &[0x8825, 0x0002]);
        assert_eq!((kind, count), (5, 3));
        let lat = rational(&lat, 0) + rational(&lat, 1) / 60.0 + rational(&lat, 2) / 3600.0;
        assert!((lat - 53.2183).abs() < 1e-7, "{lat}");
        let (_, _, lng) = exif_field(&tiff, &[0x8825, 0x0004]);
        let lng = rational(&lng, 0) + rational(&lng, 1) / 60.0 + rational(&lng, 2) / 3600.0;
        assert!((lng - 6.5679).abs() < 1e-7, "{lng}");
        // Facing 9°, magnetic, as the phone's compass measures it.
        assert_eq!(exif_field(&tiff, &[0x8825, 0x0010]).2, b"M\0");
        assert!((rational(&exif_field(&tiff, &[0x8825, 0x0011]).2, 0) - 9.0).abs() < 1e-6);
        // The ground in the height map plus the camera's height above it.
        let alt = rational(&exif_field(&tiff, &[0x8825, 0x0006]).2, 0);
        assert!((alt - (6.97 + EYE_HEIGHT_M)).abs() < 0.01, "{alt}");
        assert_eq!(exif_field(&tiff, &[0x8825, 0x001D]).2, b"2026:10:15\0");
        assert_eq!(exif_field(&tiff, &[0x0132]).2, b"2026:10:15 12:03:11\0");
        assert_eq!(
            exif_field(&tiff, &[0x010E]).2,
            b"Grote Markt 1, Groningen\0"
        );
        // Upright already, and a 68° upright frame is a 27 mm lens.
        assert_eq!(exif_field(&tiff, &[0x0112]), (3, 1, vec![1, 0]));
        assert_eq!(exif_field(&tiff, &[0x8769, 0xA405]), (3, 1, vec![27, 0]));
        // The angles EXIF has no tag for are in the XMP.
        let xmp = p.xmp();
        assert!(xmp.contains("cartopolis:PitchDeg=\"14.00\""), "{xmp}");
        assert!(xmp.contains("cartopolis:RollDeg=\"0.40\""), "{xmp}");
        assert!(xmp.contains("cartopolis:Mode=\"mixed\""), "{xmp}");
        // The whole pose rides along, readable as a caption is.
        let (kind, _, comment) = exif_field(&tiff, &[0x8769, 0x9286]);
        assert_eq!(kind, 7);
        assert_eq!(&comment[..8], b"ASCII\0\0\0");
        let back = PhotoPose::from_caption(std::str::from_utf8(&comment[8..]).unwrap());
        assert_eq!(back.map(|(_, pose)| pose), Some(p));
    }

    #[test]
    fn south_and_west_are_written_as_such() {
        let mut p = grote_markt();
        p.lat = -33.8568;
        p.lng = -151.2153;
        p.source = PoseSource::View;
        p.ground_ahn_m = None;
        let tiff = p.exif();
        assert_eq!(exif_field(&tiff, &[0x8825, 0x0001]).2, b"S\0");
        assert_eq!(exif_field(&tiff, &[0x8825, 0x0003]).2, b"W\0");
        assert_eq!(exif_field(&tiff, &[0x8825, 0x0010]).2, b"T\0");
        let (_, _, lat) = exif_field(&tiff, &[0x8825, 0x0002]);
        let lat = rational(&lat, 0) + rational(&lat, 1) / 60.0 + rational(&lat, 2) / 3600.0;
        assert!((lat - 33.8568).abs() < 1e-7, "{lat}");
    }
}
