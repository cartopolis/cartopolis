//! Two generic things a script can build an instrument — or a doorbell, or a
//! dowsing rod — out of (protocol 36, `docs/notes/hands-and-tones.md`):
//!
//! * **hands** — a script asks for the hands near its prim (`sense_hands`),
//!   and every client within that range whose user has switched on
//!   high-precision tracking reports where its hand is relative to the prim
//!   ([`HandPose`]); the script reads them in `on_hand`;
//! * **a tone** — a prim may sound a synthesised tone ([`ToneSpec`]) whose pitch
//!   and loudness the script sets.
//!
//! Neither knows what it is for. A theremin is a script that takes the first
//! hand to arrive as its player, and sets the tone's pitch from the hand's
//! distance to one point and its gain from the distance to another; all of that
//! — including who gets to play — is the script's.
//!
//! **What that costs is latency.** The hand travels to the simulator, the script
//! runs on its next tick, and the tone travels back: on the order of 100–150 ms
//! from a hand moving to the player hearing it. [`ToneSpec::glide_s`] is what
//! keeps the steps between updates from being audible.

use serde::{Deserialize, Serialize};

use crate::sounds::{DEFAULT_SOUND_RANGE_M, MAX_SOUND_RANGE_M, MIN_SOUND_RANGE_M};

/// The waveform a tone is made of. **Append only** — postcard encodes the
/// variant index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Voice {
    /// A sine with a touch of second and third harmonic: pure, singing.
    Sine,
    /// Organ-pipe harmonics: steady and round.
    Organ,
    /// A sawtooth: bright and buzzing.
    Saw,
    /// A square: hollow and reedy.
    Square,
}

impl Voice {
    pub const ALL: [Voice; 4] = [Self::Sine, Self::Organ, Self::Saw, Self::Square];

    #[must_use]
    pub fn slug(self) -> &'static str {
        match self {
            Self::Sine => "sine",
            Self::Organ => "organ",
            Self::Saw => "saw",
            Self::Square => "square",
        }
    }

    #[must_use]
    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.slug() == slug)
    }

    /// Every voice a script may name, comma-separated, for an error message.
    #[must_use]
    pub fn catalogue() -> String {
        Self::ALL.map(Self::slug).join(", ")
    }
}

/// The span a tone's pitch may take.
pub const MIN_PITCH_HZ: f32 = 20.0;
pub const MAX_PITCH_HZ: f32 = 4_000.0;
/// Longest pitch glide a script may ask for.
pub const MAX_GLIDE_S: f32 = 1.0;
/// The widest range a script may sense hands over (`sense_hands`). A hand is
/// something you bring *to* an object; past arm's reach and a few steps it is a
/// position report, which is not what anybody switched tracking on to share.
pub const MAX_HAND_RANGE_M: f32 = 5.0;
/// How often a client reports a hand to one prim, at most.
pub const HAND_HZ: f32 = 20.0;

/// A tone a prim is sounding, as its script last set it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ToneSpec {
    pub voice: Voice,
    pub pitch_hz: f32,
    /// 0..=1, at the prim.
    pub gain: f32,
    /// Seconds to slide to a new pitch rather than jump.
    pub glide_s: f32,
    /// Vibrato depth, 0..=1 (1 is a semitone either way).
    pub vibrato: f32,
    /// Metres at which listeners hear it fade to nothing.
    pub range_m: f32,
}

impl Default for ToneSpec {
    /// A steady A4 sine at half volume.
    fn default() -> Self {
        Self {
            voice: Voice::Sine,
            pitch_hz: 440.0,
            gain: 0.5,
            glide_s: 0.05,
            vibrato: 0.0,
            range_m: DEFAULT_SOUND_RANGE_M,
        }
    }
}

fn finite_or(v: f32, fallback: f32) -> f32 {
    if v.is_finite() {
        v
    } else {
        fallback
    }
}

impl ToneSpec {
    /// Clamp every number into range, mapping a non-finite one to the default.
    #[must_use]
    pub fn sanitized(self) -> Self {
        let d = Self::default();
        Self {
            voice: self.voice,
            pitch_hz: finite_or(self.pitch_hz, d.pitch_hz).clamp(MIN_PITCH_HZ, MAX_PITCH_HZ),
            gain: finite_or(self.gain, d.gain).clamp(0.0, 1.0),
            glide_s: finite_or(self.glide_s, d.glide_s).clamp(0.0, MAX_GLIDE_S),
            vibrato: finite_or(self.vibrato, d.vibrato).clamp(0.0, 1.0),
            range_m: finite_or(self.range_m, DEFAULT_SOUND_RANGE_M)
                .clamp(MIN_SOUND_RANGE_M, MAX_SOUND_RANGE_M),
        }
    }
}

/// Where a hand is relative to a prim that senses hands: metres from the
/// prim's centre in the prim's own frame (turned with it, not scaled with it).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HandPose {
    pub prim_id: i64,
    pub offset: [f32; 3],
    /// The client knows where the hand is to within centimetres — the phone's
    /// high-precision tracking, where the view *is* the hand holding the phone.
    /// `false` is a camera standing in for a hand (a developer switch, for trying
    /// a script from a desktop). The script decides what to do with an
    /// imprecise hand.
    pub precise: bool,
}

impl HandPose {
    /// Metres from the prim's centre; infinite for a non-finite offset, so a
    /// range test refuses it.
    #[must_use]
    pub fn distance(&self) -> f32 {
        let [x, y, z] = self.offset;
        let d = (x * x + y * y + z * z).sqrt();
        if d.is_finite() {
            d
        } else {
            f32::INFINITY
        }
    }
}

/// The frequency of a note written the way a musician writes it: `"A4"`,
/// `"C#3"`, `"Eb5"` — scientific pitch, A4 = 440 Hz, equal temperament.
#[must_use]
pub fn note_hz(note: &str) -> Option<f32> {
    let mut chars = note.trim().chars();
    let base = match chars.next()?.to_ascii_uppercase() {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let rest: String = chars.collect();
    let (shift, octave) = match rest.chars().next() {
        Some('#') => (1, &rest[1..]),
        Some('b') => (-1, &rest[1..]),
        _ => (0, rest.as_str()),
    };
    let octave: i32 = octave.parse().ok()?;
    if !(0..=8).contains(&octave) {
        return None;
    }
    let semitones_from_a4 = (octave - 4) * 12 + base + shift - 9;
    Some(440.0 * 2f32.powf(semitones_from_a4 as f32 / 12.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_land_on_their_frequencies() {
        let close = |n: &str, hz: f32| (note_hz(n).unwrap() - hz).abs() < 0.05;
        assert!(close("A4", 440.0));
        assert!(close("C4", 261.63));
        assert!(close("C#4", 277.18));
        assert!(close("Db4", 277.18));
        assert!(close("a3", 220.0));
        assert!(close("C6", 1046.50));
        assert_eq!(note_hz("H4"), None);
        assert_eq!(note_hz("A"), None);
        assert_eq!(note_hz("A9"), None);
        assert_eq!(note_hz(""), None);
    }

    #[test]
    fn sanitizing_a_tone_clamps_and_fixes_non_finite() {
        let s = ToneSpec {
            pitch_hz: 1e9,
            gain: f32::NAN,
            glide_s: -1.0,
            vibrato: 7.0,
            range_m: f32::INFINITY,
            ..ToneSpec::default()
        }
        .sanitized();
        assert_eq!(s.pitch_hz, MAX_PITCH_HZ);
        assert_eq!(s.gain, ToneSpec::default().gain);
        assert_eq!((s.glide_s, s.vibrato), (0.0, 1.0));
        assert_eq!(s.range_m, DEFAULT_SOUND_RANGE_M);
    }

    #[test]
    fn a_hand_that_is_not_a_number_is_out_of_any_range() {
        let pose = |offset| HandPose {
            prim_id: 1,
            offset,
            precise: true,
        };
        assert!((pose([3.0, 4.0, 0.0]).distance() - 5.0).abs() < 1e-6);
        assert_eq!(pose([0.0, f32::NAN, 0.0]).distance(), f32::INFINITY);
    }

    #[test]
    fn every_voice_slug_round_trips() {
        for v in Voice::ALL {
            assert_eq!(Voice::from_slug(v.slug()), Some(v));
        }
        assert_eq!(Voice::from_slug("theremin"), None);
    }
}
