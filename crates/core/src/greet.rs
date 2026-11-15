//! Greeting another avatar: a handshake or a high five, offered by one and
//! accepted by the other (protocol 49, `docs/notes/paired-gestures.md`).
//!
//! The sim decides everything and starts both bodies at once: it stands the
//! two where the clip needs them ([`pair_up`]), turns them, and gives both the
//! same emote with a fresh sequence in the same tick, so every viewer plays
//! the pair in step. This module is the arithmetic both ends share.

use serde::{Deserialize, Serialize};

use crate::protocol::Emote;

/// A gesture two people make together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Greeting {
    Handshake,
    HighFive,
}

impl Greeting {
    pub const ALL: [Self; 2] = [Self::Handshake, Self::HighFive];

    /// The clip both avatars play.
    #[must_use]
    pub const fn emote(self) -> Emote {
        match self {
            Self::Handshake => Emote::Handshake,
            Self::HighFive => Emote::HighFive,
        }
    }

    /// Where the partner stands, in metres in the greeter's own frame: `x` to
    /// the greeter's left, `z` ahead of the greeter.
    ///
    /// Both avatars play the same clip — one subject's side of a two-person
    /// take — stood point-symmetric about the middle between them, so each
    /// right palm meets the other's halfway. That holds when the partner is
    /// at twice where the palm reaches, across the floor; `human-pack
    /// --add-clip` prints it for the default man (1.73 m), and the pack's
    /// `greetings_meet_halfway` test holds the pack to these numbers.
    /// Measured 2026-11-15 at the moment of contact (the frame each cut is
    /// planted at): the handshake's palm 0.41 m ahead and 0.13 m to the
    /// right at 0.96 m up, the high five's 0.35 m ahead and 0.21 m to the
    /// right at 1.71 m up — so 0.85 m and 0.81 m apart, each partner 18° and
    /// 31° to the right of straight ahead. The takes' own subjects stood
    /// 0.74 m and 0.9 m apart at contact, so the layout is the capture's,
    /// not an invention.
    #[must_use]
    pub const fn partner_at(self) -> [f32; 2] {
        match self {
            Self::Handshake => [-0.26, 0.81],
            Self::HighFive => [-0.41, 0.70],
        }
    }

    /// What the offer and the prompt call it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Handshake => "Shake hands",
            Self::HighFive => "High five",
        }
    }
}

/// How far apart two avatars may be (flat, metres, between where the sim holds
/// them) to offer and to start a greeting: two short paces, the reach a
/// pick-up has (`carry::GRAB_REACH_M`).
pub const GREET_REACH_M: f32 = 2.5;

/// How much higher one may stand than the other, metres: on the same floor,
/// not one mid-jump.
pub const GREET_STEP_M: f32 = 0.5;

/// How long an offer waits for an answer, seconds.
pub const OFFER_SECS: f32 = 10.0;

/// How long the sim holds both avatars' facing once a greeting starts,
/// seconds, so an intent already on its way with the old facing does not turn
/// one away mid-gesture. The emote's own replication time: the clips are a
/// little shorter.
pub const GREET_HOLD_SECS: f32 = crate::protocol::EMOTE_REPLICATE_SECS;

/// Where two avatars stand and which way they face for a greeting.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pairing {
    /// Metres in the frame `a` and `b` were given in (x east, z south).
    pub a: [f32; 2],
    pub b: [f32; 2],
    /// Facings, as `ClientIntent::display_yaw`: 0 faces +z (south).
    pub yaw_a: f32,
    pub yaw_b: f32,
}

/// Stand `a` and `b` for `greeting`: about the middle between them, along the
/// line between them, at the clip's own spacing, each turned so the other
/// stands where [`Greeting::partner_at`] says. Point-symmetric, so `b` faces
/// the opposite way to `a`. Two avatars on the same spot pair along +z.
#[must_use]
pub fn pair_up(a: [f32; 2], b: [f32; 2], greeting: Greeting) -> Pairing {
    let [px, pz] = greeting.partner_at();
    let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
    let len = dx.hypot(dz);
    let (ux, uz) = if len > 1.0e-3 {
        (dx / len, dz / len)
    } else {
        (0.0, 1.0)
    };
    let mid = [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
    let half = px.hypot(pz) * 0.5;
    let yaw_a = crate::wrap_angle_pi(ux.atan2(uz) - px.atan2(pz));
    Pairing {
        a: [mid[0] - ux * half, mid[1] - uz * half],
        b: [mid[0] + ux * half, mid[1] + uz * half],
        yaw_a,
        yaw_b: crate::wrap_angle_pi(yaw_a + std::f32::consts::PI),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `v` in a body frame turned to `yaw`, in the world (x east, z south).
    fn turned(yaw: f32, v: [f32; 2]) -> [f32; 2] {
        let (s, c) = yaw.sin_cos();
        [v[0] * c + v[1] * s, -v[0] * s + v[1] * c]
    }

    fn near(a: [f32; 2], b: [f32; 2]) -> bool {
        (a[0] - b[0]).hypot(a[1] - b[1]) < 1.0e-4
    }

    /// Each finds the other where the clip expects a partner, and they keep
    /// their middle: neither is carried off to one side of where they were.
    #[test]
    fn each_finds_the_other_where_the_clip_wants_them() {
        for g in Greeting::ALL {
            for (a, b) in [
                ([0.0, 0.0], [0.0, 2.0]),
                ([3.0, -1.0], [1.5, 0.2]),
                ([0.0, 0.0], [0.4, -0.1]),
            ] {
                let p = pair_up(a, b, g);
                let ab = [p.b[0] - p.a[0], p.b[1] - p.a[1]];
                let ba = [-ab[0], -ab[1]];
                assert!(near(turned(p.yaw_a, g.partner_at()), ab), "{g:?} a sees b");
                assert!(near(turned(p.yaw_b, g.partner_at()), ba), "{g:?} b sees a");
                let mid = [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
                let new_mid = [(p.a[0] + p.b[0]) * 0.5, (p.a[1] + p.b[1]) * 0.5];
                assert!(near(mid, new_mid), "{g:?} keeps the middle");
            }
        }
    }

    /// The right palms meet: each one's palm, at half the partner offset in its
    /// own frame, lands on the same spot.
    #[test]
    fn the_palms_meet_halfway() {
        for g in Greeting::ALL {
            let p = pair_up([1.0, 1.0], [2.0, -0.5], g);
            let half = [g.partner_at()[0] * 0.5, g.partner_at()[1] * 0.5];
            let pa = turned(p.yaw_a, half);
            let pb = turned(p.yaw_b, half);
            assert!(near(
                [p.a[0] + pa[0], p.a[1] + pa[1]],
                [p.b[0] + pb[0], p.b[1] + pb[1]]
            ));
        }
    }

    /// Facing each other, more or less: the greeter's face points within 40°
    /// of the partner, as people offset to shake right hands do.
    #[test]
    fn they_face_each_other() {
        for g in Greeting::ALL {
            let [x, z] = g.partner_at();
            assert!(z > 0.0, "{g:?}: the partner is ahead");
            assert!(x.atan2(z).abs() < 40f32.to_radians(), "{g:?}");
            assert!(
                (0.5..1.2).contains(&x.hypot(z)),
                "{g:?}: arm's length apart, not touching"
            );
        }
    }
}
