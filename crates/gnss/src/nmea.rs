//! NMEA 0183, the receiver's text output: framing out of a mixed byte stream,
//! checksums, and the three sentences a position needs.
//!
//! **The stream is not all text.** The same port carries the receiver's replies
//! to commands, and on many boards binary messages too (Unicore's own binary
//! log, an echo of the RTCM we write). So the framer looks for `$`, takes
//! printable bytes up to the line end, and drops anything that turns binary or
//! runs too long, rather than assuming every line is a sentence.
//!
//! Three sentences make a solution, each for a different part of it:
//!
//! * **GGA** — position, height, fix *quality* (single, DGPS, RTK float, RTK
//!   fixed), satellites, HDOP and the age of the corrections. The epoch.
//! * **RMC** — speed and course over ground, which GGA does not carry.
//! * **GST** — the receiver's own 1σ error estimate in metres. Without it the
//!   accuracy has to be guessed from the fix quality and HDOP, and a guess is
//!   all an RTK fixed solution gets from most phones' APIs; the receiver knows.

/// Longest line kept. NMEA's own limit is 82 bytes; Unicore's `$command`
/// replies echo the command and run longer.
const MAX_LINE: usize = 256;

/// Splits a byte stream into checksummed NMEA lines.
#[derive(Default)]
pub struct Framer {
    line: Vec<u8>,
    /// Inside a `$…` line.
    open: bool,
    /// Lines whose checksum did not match — a noisy cable, or the wrong baud.
    pub bad_checksums: u64,
}

impl Framer {
    /// Feed bytes; `out` is called with the body of every line whose checksum
    /// matched (between `$` and `*`, e.g. `GNGGA,123519.00,…`).
    pub fn push(&mut self, bytes: &[u8], mut out: impl FnMut(&str)) {
        for &b in bytes {
            if b == b'$' {
                self.line.clear();
                self.open = true;
                continue;
            }
            if !self.open {
                continue;
            }
            if b == b'\r' || b == b'\n' {
                self.open = false;
                match checked_body(&self.line) {
                    Some(Ok(body)) => out(body),
                    Some(Err(())) => self.bad_checksums += 1,
                    None => {}
                }
                self.line.clear();
                continue;
            }
            if !(0x20..0x7f).contains(&b) || self.line.len() >= MAX_LINE {
                // Binary, or a line with no end: not a sentence.
                self.open = false;
                self.line.clear();
                continue;
            }
            self.line.push(b);
        }
    }
}

/// The body of a line, if its `*HH` checksum matches. `None` for a line with no
/// checksum at all (not NMEA), `Some(Err)` for one that does not match.
fn checked_body(line: &[u8]) -> Option<Result<&str, ()>> {
    let text = std::str::from_utf8(line).ok()?;
    let (body, sum) = text.rsplit_once('*')?;
    let sum = u8::from_str_radix(sum.get(..2)?, 16).ok()?;
    if checksum(body) == sum {
        return Some(Ok(body));
    }
    // Unicore's command replies checksum the `$` as well, unlike NMEA proper
    // (measured on a UM980, R4.10 Build 20739). Only for those: on any other
    // sentence a match this way is a corruption that happens to agree.
    if body.starts_with("command,") && checksum(body) ^ b'$' == sum {
        return Some(Ok(body));
    }
    Some(Err(()))
}

/// XOR of every byte between `$` and `*`.
#[must_use]
pub fn checksum(body: &str) -> u8 {
    body.bytes().fold(0, |a, b| a ^ b)
}

/// `$body*HH\r\n` — a sentence ready to send, e.g. a GGA for a caster.
#[must_use]
pub fn sentence(body: &str) -> String {
    format!("${body}*{:02X}\r\n", checksum(body))
}

/// GGA's quality indicator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FixQuality {
    /// No position.
    #[default]
    None,
    /// Autonomous: metres.
    Single,
    /// Code corrections — DGPS or SBAS: around a metre.
    Dgps,
    /// Carrier phase with the ambiguities not yet fixed: decimetres.
    RtkFloat,
    /// Carrier phase with the ambiguities fixed: centimetres.
    RtkFixed,
    /// Dead reckoning, manual input or simulation — not a measurement.
    Estimated,
}

impl FixQuality {
    #[must_use]
    pub fn from_gga(q: u32) -> Self {
        match q {
            1 => Self::Single,
            // 3 is PPS, 9 is SBAS on some receivers: both are code-level.
            2 | 3 | 9 => Self::Dgps,
            4 => Self::RtkFixed,
            5 => Self::RtkFloat,
            6..=8 => Self::Estimated,
            _ => Self::None,
        }
    }

    /// How a person would name it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::None => "No fix",
            Self::Single => "GNSS",
            Self::Dgps => "DGNSS",
            Self::RtkFloat => "RTK float",
            Self::RtkFixed => "RTK fixed",
            Self::Estimated => "Estimated",
        }
    }

    /// Whether this is a position at all.
    #[must_use]
    pub const fn has_position(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// A GGA sentence.
#[derive(Clone, Debug, PartialEq)]
pub struct Gga {
    /// UTC seconds since midnight.
    pub utc_s: Option<f64>,
    pub lat: Option<f64>,
    pub lng: Option<f64>,
    pub quality: FixQuality,
    pub satellites: Option<u32>,
    pub hdop: Option<f64>,
    /// Antenna height above mean sea level (the geoid), metres.
    pub altitude_m: Option<f64>,
    /// Geoid height above the ellipsoid, metres.
    pub geoid_separation_m: Option<f64>,
    /// Seconds since the last correction the solution used.
    pub correction_age_s: Option<f64>,
    pub station: Option<u32>,
}

/// An RMC sentence: the parts GGA lacks.
#[derive(Clone, Debug, PartialEq)]
pub struct Rmc {
    pub utc_s: Option<f64>,
    /// `A` (valid) rather than `V`.
    pub valid: bool,
    pub speed_ms: Option<f64>,
    /// Course over ground, degrees clockwise from true north.
    pub course_deg: Option<f64>,
}

/// A GST sentence: the receiver's 1σ errors, metres.
#[derive(Clone, Debug, PartialEq)]
pub struct Gst {
    pub utc_s: Option<f64>,
    pub lat_sigma_m: f64,
    pub lng_sigma_m: f64,
    pub alt_sigma_m: Option<f64>,
}

/// One parsed line.
#[derive(Clone, Debug, PartialEq)]
pub enum Sentence {
    Gga(Gga),
    Rmc(Rmc),
    Gst(Gst),
    /// A Unicore command reply (`$command,GPGGA 0.2,response: OK`), as the text
    /// after the command.
    Reply(String),
    /// Anything else — GSV, GSA, a vendor sentence.
    Other,
}

const KNOT_MS: f64 = 1852.0 / 3600.0;

/// Parse a checksummed body from [`Framer`].
#[must_use]
pub fn parse(body: &str) -> Sentence {
    let mut fields = body.split(',');
    let Some(head) = fields.next() else {
        return Sentence::Other;
    };
    if head.eq_ignore_ascii_case("command") {
        return Sentence::Reply(body.split_once(',').map_or("", |(_, r)| r).to_string());
    }
    // The talker (GP, GN, GL, …) is the first two letters; the type the rest.
    let kind = head.get(2..).unwrap_or("");
    let f: Vec<&str> = fields.collect();
    let get = |i: usize| f.get(i).copied().unwrap_or("");
    match kind {
        "GGA" => Sentence::Gga(Gga {
            utc_s: utc(get(0)),
            lat: coordinate(get(1), get(2), 2),
            lng: coordinate(get(3), get(4), 3),
            quality: num::<u32>(get(5)).map_or(FixQuality::None, FixQuality::from_gga),
            satellites: num(get(6)),
            hdop: num(get(7)),
            altitude_m: num(get(8)),
            geoid_separation_m: num(get(10)),
            correction_age_s: num(get(12)),
            station: num(get(13)),
        }),
        "RMC" => Sentence::Rmc(Rmc {
            utc_s: utc(get(0)),
            valid: get(1) == "A",
            speed_ms: num::<f64>(get(6)).map(|kn| kn * KNOT_MS),
            course_deg: num(get(7)),
        }),
        "GST" => {
            // The error ellipse, when there is one, is the figure to believe:
            // the UM980 (R4.10 Build 20739) updates it every epoch but can
            // leave the latitude and longitude columns frozen — measured in RTK
            // float at 0.16 × 0.03 m beside columns stuck at 24.9 / 74.0 m. The
            // two describe one error, so the columns are derived from the
            // ellipse by rotating it into north and east.
            let ellipse = match (num::<f64>(get(2)), num::<f64>(get(3)), num::<f64>(get(4))) {
                (Some(a), Some(b), Some(deg)) if a > 0.0 && b >= 0.0 => {
                    let (sin, cos) = deg.to_radians().sin_cos();
                    // σ_north² = a²cos²θ + b²sin²θ, σ_east² = a²sin²θ + b²cos²θ
                    Some(((a * cos).hypot(b * sin), (a * sin).hypot(b * cos)))
                }
                _ => None,
            };
            match ellipse.or_else(|| Some((num(get(5))?, num(get(6))?))) {
                Some((lat_sigma_m, lng_sigma_m)) => Sentence::Gst(Gst {
                    utc_s: utc(get(0)),
                    lat_sigma_m,
                    lng_sigma_m,
                    alt_sigma_m: num(get(7)),
                }),
                None => Sentence::Other,
            }
        }
        _ => Sentence::Other,
    }
}

fn num<T: std::str::FromStr>(s: &str) -> Option<T> {
    if s.is_empty() {
        None
    } else {
        s.parse().ok()
    }
}

/// `hhmmss.ss` → seconds since midnight.
fn utc(s: &str) -> Option<f64> {
    if s.len() < 6 {
        return None;
    }
    let h: f64 = s.get(0..2)?.parse().ok()?;
    let m: f64 = s.get(2..4)?.parse().ok()?;
    let sec: f64 = s.get(4..)?.parse().ok()?;
    Some(h * 3600.0 + m * 60.0 + sec)
}

/// `ddmm.mmmm` / `dddmm.mmmm` plus a hemisphere → signed degrees. The degree
/// digits are split off as text, so no precision is lost to a division: the
/// UM980 gives eight decimals of minutes, about a tenth of a millimetre.
fn coordinate(value: &str, hemisphere: &str, degree_digits: usize) -> Option<f64> {
    let degrees: f64 = value.get(..degree_digits)?.parse().ok()?;
    let minutes: f64 = value.get(degree_digits..)?.parse().ok()?;
    if !(0.0..60.0).contains(&minutes) {
        return None;
    }
    let v = degrees + minutes / 60.0;
    match hemisphere {
        "N" | "E" => Some(v),
        "S" | "W" => Some(-v),
        _ => None,
    }
}

/// Degrees → `ddmm.mmmmmmm,N` (or `dddmm…,E` with `degree_digits` 3), for the
/// GGA sent to a caster.
#[must_use]
pub fn format_coordinate(
    value: f64,
    degree_digits: usize,
    positive: char,
    negative: char,
) -> String {
    let hemisphere = if value < 0.0 { negative } else { positive };
    let v = value.abs();
    let mut degrees = v.trunc();
    let mut minutes = (v - degrees) * 60.0;
    // Rounding at seven decimals can carry into a whole degree.
    if minutes >= 59.999_999_95 {
        degrees += 1.0;
        minutes = 0.0;
    }
    format!(
        "{:0dw$}{:010.7},{hemisphere}",
        degrees as u32,
        minutes,
        dw = degree_digits
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A GGA as the UM980 writes one through an RTK fix: eight decimals of
    /// minutes, quality 4, and the correction age filled in.
    const GGA_FIXED: &str =
        "GNGGA,101530.00,5313.16488123,N,00633.99012345,E,4,32,0.6,8.123,M,40.512,M,1.0,0000";

    fn line(body: &str) -> String {
        format!("${body}*{:02X}\r\n", checksum(body))
    }

    fn frames(bytes: &[u8]) -> (Vec<String>, u64) {
        let mut f = Framer::default();
        let mut out = Vec::new();
        f.push(bytes, |b| out.push(b.to_string()));
        (out, f.bad_checksums)
    }

    #[test]
    fn a_recorded_fixed_gga_parses() {
        let (bodies, _) = frames(line(GGA_FIXED).as_bytes());
        let Sentence::Gga(g) = parse(&bodies[0]) else {
            panic!("not a GGA: {bodies:?}");
        };
        assert_eq!(g.quality, FixQuality::RtkFixed);
        assert_eq!(g.satellites, Some(32));
        assert!((g.lat.unwrap() - (53.0 + 13.16488123 / 60.0)).abs() < 1e-12);
        assert!((g.lng.unwrap() - (6.0 + 33.99012345 / 60.0)).abs() < 1e-12);
        assert_eq!(g.utc_s, Some(10.0 * 3600.0 + 15.0 * 60.0 + 30.0));
        assert_eq!(g.correction_age_s, Some(1.0));
        assert_eq!(g.altitude_m, Some(8.123));
    }

    #[test]
    fn the_framer_survives_binary_noise_and_split_reads() {
        let a = line("GNRMC,101530.00,A,5313.16488,N,00633.99012,E,10.0,90.0,131026,,,R,V");
        let mut stream = vec![0xD3, 0x00, 0x13, b'$', 0xFF, 0x00];
        stream.extend_from_slice(a.as_bytes());
        stream.extend_from_slice(&[0xD3, b'$', b'G']);
        stream.extend_from_slice(a.as_bytes());
        // Fed one byte at a time, as a slow port delivers it.
        let mut f = Framer::default();
        let mut out = Vec::new();
        for b in &stream {
            f.push(std::slice::from_ref(b), |s| out.push(s.to_string()));
        }
        assert_eq!(out.len(), 2, "{out:?}");
        let Sentence::Rmc(r) = parse(&out[0]) else {
            panic!()
        };
        assert!(r.valid);
        assert!((r.speed_ms.unwrap() - 5.144_444).abs() < 1e-5);
        assert_eq!(r.course_deg, Some(90.0));
    }

    #[test]
    fn a_wrong_checksum_is_counted_not_parsed() {
        let (out, bad) = frames(b"$GNGGA,1,2,3*00\r\n");
        assert!(out.is_empty());
        assert_eq!(bad, 1);
    }

    #[test]
    fn southern_and_western_hemispheres_are_negative() {
        assert!((coordinate("3352.0000", "S", 2).unwrap() + (33.0 + 52.0 / 60.0)).abs() < 1e-12);
        assert!((coordinate("15112.0000", "W", 3).unwrap() + (151.0 + 12.0 / 60.0)).abs() < 1e-12);
        assert_eq!(coordinate("", "N", 2), None);
    }

    #[test]
    fn a_gga_with_no_fix_has_no_position() {
        let (out, _) = frames(line("GNGGA,,,,,,0,00,99.99,,,,,,").as_bytes());
        let Sentence::Gga(g) = parse(&out[0]) else {
            panic!()
        };
        assert_eq!(g.quality, FixQuality::None);
        assert_eq!(g.lat, None);
    }

    #[test]
    fn gst_gives_metres() {
        // No ellipse: the columns are all there is.
        let (out, _) = frames(line("GNGST,101530.00,0.9,,,,0.012,0.009,0.021").as_bytes());
        let Sentence::Gst(g) = parse(&out[0]) else {
            panic!()
        };
        assert_eq!(g.lat_sigma_m, 0.012);
        assert_eq!(g.lng_sigma_m, 0.009);
        assert_eq!(g.alt_sigma_m, Some(0.021));
    }

    /// The UM980 in RTK float, indoors in Groningen (2026-10-13): the
    /// ellipse is live at 0.16 × 0.03 m while the latitude and longitude
    /// columns sit frozen at 24.9 / 74.0 m. The ellipse wins, and its rotation
    /// into north and east keeps the total: √(σn² + σe²) = √(a² + b²).
    #[test]
    fn the_live_ellipse_wins_over_frozen_columns() {
        let recorded = "$GNGST,173510.20,4.18,0.16,0.03,68.4092,24.860,74.027,40.633*";
        let body = recorded.trim_start_matches('$').trim_end_matches('*');
        let (out, _) = frames(line(body).as_bytes());
        let Sentence::Gst(g) = parse(&out[0]) else {
            panic!()
        };
        let total = g.lat_sigma_m.hypot(g.lng_sigma_m);
        assert!((total - 0.16f64.hypot(0.03)).abs() < 1e-9, "{total}");
        // The long axis points ~68° east of north, so most of it is east.
        assert!(g.lng_sigma_m > g.lat_sigma_m);
        assert_eq!(g.alt_sigma_m, Some(40.633));
    }

    /// The single-fix sentence recorded earlier the same day, where ellipse
    /// and columns still agreed: the derived figure matches the columns.
    #[test]
    fn where_the_columns_are_live_the_ellipse_agrees() {
        let (out, _) =
            frames(line("GNGST,163751.00,3.07,20.09,7.03,59.1094,10.023,18.576,16.623").as_bytes());
        let Sentence::Gst(g) = parse(&out[0]) else {
            panic!()
        };
        let from_ellipse = g.lat_sigma_m.hypot(g.lng_sigma_m);
        let from_columns = 10.023f64.hypot(18.576);
        assert!(
            (from_ellipse - from_columns).abs() < 0.3,
            "{from_ellipse} vs {from_columns}"
        );
    }

    /// Lines recorded from the UM980 Handheld Surveyor Kit on 2026-10-13,
    /// indoors, checksums as the receiver wrote them.
    const RECORDED: &str = "$command,GPGGA 0.2,response: OK*04\r\n\
        $GNGST,163751.00,3.07,20.09,7.03,59.1094,10.023,18.576,16.623*78\r\n\
        $GNRMC,163752.00,A,5313.14270470,N,00634.07844981,E,0.785,251.6,131026,3.0,E,A,C*47\r\n\
        $GNGGA,163752.00,5313.14270470,N,00634.07844981,E,1,12,2.2,-8.0411,M,40.9105,M,,*54\r\n\
        $GBGSV,2,2,05,30,16,036,32,1*43\r\n";

    #[test]
    fn the_receivers_own_output_parses_including_its_replies() {
        let (out, bad) = frames(RECORDED.as_bytes());
        assert_eq!(bad, 0, "{out:?}");
        assert_eq!(out.len(), 5);
        assert_eq!(
            parse(&out[0]),
            Sentence::Reply("GPGGA 0.2,response: OK".into())
        );
        let Sentence::Gst(g) = parse(&out[1]) else {
            panic!()
        };
        // From the ellipse (20.09 × 7.03 m): the same total as the columns.
        assert!((g.lat_sigma_m.hypot(g.lng_sigma_m) - 20.09f64.hypot(7.03)).abs() < 1e-9);
        let Sentence::Rmc(r) = parse(&out[2]) else {
            panic!()
        };
        assert_eq!(r.course_deg, Some(251.6));
        let Sentence::Gga(g) = parse(&out[3]) else {
            panic!()
        };
        assert_eq!(g.quality, FixQuality::Single);
        assert_eq!(g.altitude_m, Some(-8.0411));
        assert_eq!(parse(&out[4]), Sentence::Other);
    }

    #[test]
    fn the_dollar_checksum_is_accepted_only_on_command_replies() {
        let body = "GNGGA,1,2";
        let wrong = format!("${body}*{:02X}\r\n", checksum(body) ^ b'$');
        let (out, bad) = frames(wrong.as_bytes());
        assert!(out.is_empty());
        assert_eq!(bad, 1);
    }

    #[test]
    fn command_replies_are_kept() {
        let (out, _) = frames(line("command,GPGGA 0.2,response: OK").as_bytes());
        assert_eq!(
            parse(&out[0]),
            Sentence::Reply("GPGGA 0.2,response: OK".into())
        );
    }

    #[test]
    fn coordinates_format_back_to_what_they_parse_from() {
        let lat = 53.0 + 13.16488 / 60.0;
        let s = format_coordinate(lat, 2, 'N', 'S');
        assert_eq!(s, "5313.1648800,N");
        let (v, h) = s.split_once(',').unwrap();
        assert!((coordinate(v, h, 2).unwrap() - lat).abs() < 1e-9);
        assert_eq!(format_coordinate(-6.5, 3, 'E', 'W'), "00630.0000000,W");
    }
}
