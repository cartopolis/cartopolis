//! The sounds a script can play (`play_sound`, `loop_sound`), shared by the
//! simulator that validates a name and the client that synthesises and plays it.
//!
//! **A closed library addressed by name, not a URL or an upload.** A URL would
//! make every client fetch whatever a stranger's script points it at, and an
//! uploaded clip needs a store, a size ceiling and a decoder per platform. The
//! client synthesises every one of these at startup, as it does its footsteps
//! (`systems::soundscape` has why: a shipped audio file is a Git LFS object with
//! a silent failure mode), so a sound is always loaded before anybody plays it —
//! which is also why there is no `preload_sound`.
//!
//! An unknown name is a script **error**, never silence: a typo that quietly
//! plays nothing costs an author far more than one that says so.

use serde::{Deserialize, Serialize};

/// One sound in the library.
///
/// **Append only.** postcard encodes the variant index, so inserting one in the
/// middle renames every sound after it on an older client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SoundName {
    /// A firework shell bursting: a sharp crack, then a low boom that rolls away.
    Firework,
    /// The glittering crackle a shell leaves behind it.
    Crackle,
    /// A short, dry pop — a cork, a bubble, a small charge.
    Pop,
    /// A struck note, bright and short: a doorbell, a notification.
    Chime,
    /// A large bell, low and slow to decay.
    Bell,
    /// A mechanical click: a switch, a button, a latch.
    Click,
    /// Air rushing past — something launching or flying by.
    Whoosh,
}

impl SoundName {
    /// Every sound, in the order the reference lists them.
    pub const ALL: [SoundName; 7] = [
        Self::Firework,
        Self::Crackle,
        Self::Pop,
        Self::Chime,
        Self::Bell,
        Self::Click,
        Self::Whoosh,
    ];

    /// The name a script writes.
    #[must_use]
    pub fn slug(self) -> &'static str {
        match self {
            Self::Firework => "firework",
            Self::Crackle => "crackle",
            Self::Pop => "pop",
            Self::Chime => "chime",
            Self::Bell => "bell",
            Self::Click => "click",
            Self::Whoosh => "whoosh",
        }
    }

    #[must_use]
    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.slug() == slug)
    }

    /// Every name a script may use, comma-separated, for an error message.
    #[must_use]
    pub fn catalogue() -> String {
        Self::ALL.map(Self::slug).join(", ")
    }
}

/// How far a sound carries when the script does not say: the `say` radius, so
/// "how far does this reach" has one answer across the whole language.
pub const DEFAULT_SOUND_RANGE_M: f32 = 50.0;
/// The furthest any sound carries: the `shout` radius.
pub const MAX_SOUND_RANGE_M: f32 = 300.0;
/// The nearest a sound may be limited to. Below this it would be inaudible from
/// a camera standing right beside the prim.
pub const MIN_SOUND_RANGE_M: f32 = 2.0;

/// A sound being played by a prim: which one, how loud at the prim, and how far
/// away it fades to nothing.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PrimSound {
    pub sound: SoundName,
    /// 0..=1, at the prim itself.
    pub volume: f32,
    /// Metres at which it has faded to silence.
    pub range_m: f32,
}

impl PrimSound {
    /// Clamp the numbers into range, mapping a non-finite one to its default.
    #[must_use]
    pub fn sanitized(self) -> Self {
        let volume = if self.volume.is_finite() {
            self.volume.clamp(0.0, 1.0)
        } else {
            1.0
        };
        let range_m = if self.range_m.is_finite() {
            self.range_m.clamp(MIN_SOUND_RANGE_M, MAX_SOUND_RANGE_M)
        } else {
            DEFAULT_SOUND_RANGE_M
        };
        Self {
            sound: self.sound,
            volume,
            range_m,
        }
    }

    /// How loud this is `distance_m` from the prim, 0..=1.
    ///
    /// Squared falloff to zero at `range_m`: loudness is heard roughly
    /// logarithmically, so a linear ramp sounds as if the sound stays at full
    /// level and then drops off a cliff at the edge.
    #[must_use]
    pub fn gain_at(&self, distance_m: f32) -> f32 {
        if !distance_m.is_finite() || distance_m >= self.range_m {
            return 0.0;
        }
        let t = 1.0 - (distance_m.max(0.0) / self.range_m);
        self.volume * t * t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_slug_round_trips_and_is_unique() {
        for s in SoundName::ALL {
            assert_eq!(SoundName::from_slug(s.slug()), Some(s));
        }
        let mut slugs: Vec<_> = SoundName::ALL.map(SoundName::slug).to_vec();
        slugs.sort_unstable();
        slugs.dedup();
        assert_eq!(slugs.len(), SoundName::ALL.len());
        assert_eq!(SoundName::from_slug("nope"), None);
    }

    #[test]
    fn gain_is_full_at_the_prim_and_silent_at_the_edge() {
        let s = PrimSound {
            sound: SoundName::Bell,
            volume: 0.8,
            range_m: 100.0,
        };
        assert!((s.gain_at(0.0) - 0.8).abs() < 1e-6);
        assert_eq!(s.gain_at(100.0), 0.0);
        assert_eq!(s.gain_at(500.0), 0.0);
        assert!(s.gain_at(25.0) > s.gain_at(50.0));
        assert_eq!(s.gain_at(f32::NAN), 0.0);
    }

    #[test]
    fn sanitizing_clamps_and_fixes_non_finite() {
        let s = PrimSound {
            sound: SoundName::Pop,
            volume: 4.0,
            range_m: 10_000.0,
        }
        .sanitized();
        assert_eq!(s.volume, 1.0);
        assert_eq!(s.range_m, MAX_SOUND_RANGE_M);
        let s = PrimSound {
            sound: SoundName::Pop,
            volume: f32::NAN,
            range_m: f32::INFINITY,
        }
        .sanitized();
        assert_eq!(s.volume, 1.0);
        assert_eq!(s.range_m, DEFAULT_SOUND_RANGE_M);
    }
}
