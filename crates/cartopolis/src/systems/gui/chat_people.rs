//! The people in a private chat, on its World view: a marker where each one's
//! avatar is, and what they last said floating over it (design
//! `docs/mockups/chat-world/v5`, screen B: "the people are where they are, and
//! new messages float over whoever said them").
//!
//! Placed by `place_pins`, which already projects the chat's points, and
//! drawn by the library (`lib_pins`); this module is the choosing and the
//! marker's and balloon's sizes.
//!
//! **Where a person is comes from the server** (`WherePeople`, protocol 30),
//! because their avatar is usually nowhere near this client's: it may be in
//! another city, far outside the snapshot's area of interest, and its sim
//! position is in its own client's frame. The server answers only for contacts
//! who show that they are online (`docs/notes/contacts.md`), every
//! `contacts::WHERE_EVERY_S` while the view is open, and `contacts::Whereabout::at` glides the
//! marker between answers. When the avatar is also in this client's view and
//! agrees with that answer, the marker stands on it instead, which is exact
//! and moves with the avatar frame by frame.
//!
//! With no chat's World view open, the same markers show every contact
//! (Layers ▸ Friends, on by default), without balloons.
//!
//! **A balloon is drawn, never interactive**, for `chat_overlay`'s reason:
//! it floats over the world, where a tap means something else. The marker
//! itself can be tapped, and flies the camera to that person.

use bevy::math::{DVec3, Rect, Vec2};

use crate::systems::chat::{ChatLine, ChatState};
use crate::systems::theme;

/// Where a marker's tip sits above the ground under the avatar, in metres: a
/// little over the head of a ~1.8 m character (`chat_overlay::BUBBLE_HEIGHT_M`),
/// so the disc stands over the person rather than on them.
pub const HEAD_HEIGHT_M: f64 = 2.15;

/// A person's avatar in this client's view counts as the one the server placed
/// only this close to where it said: further, and the two are in different
/// frames (each client anchors its own map), so the answer from the server wins.
pub const SAME_AVATAR_M: f64 = 60.0;

/// How long a balloon stays at full strength, and how long it takes to go.
const BALLOON_HOLD_SECS: f64 = 8.0;
const BALLOON_FADE_SECS: f64 = 1.5;
/// Widest a balloon gets, in points (the design's `.bub`, `max-width: 190px`).
pub(crate) const BALLOON_WIDTH: f32 = 190.0;
/// From a balloon's left edge to its tail, and the tail's size.
pub(crate) const BALLOON_TAIL_X: f32 = 18.0;
pub(crate) const BALLOON_TAIL: f32 = 6.0;
/// A balloon's words, in points.
pub(crate) const BALLOON_TEXT: f32 = 13.5;
/// The name chip's words under a marker, in points.
pub(crate) const NAME_TEXT: f32 = 11.5;

/// The marker's disc diameter, its stem, and the ring round it. The
/// library's edition (`lib_pins`) builds the same marker from these.
pub(crate) const DISC: f32 = 36.0;
pub(crate) const STEM: f32 = 12.0;
pub(crate) const RING: f32 = 3.0;

/// What `name` said last in `channel`, and how strongly to show it now:
/// `None` when they said nothing recently. A line that only carries a point is
/// left to the point's own pin.
#[must_use]
pub fn latest_said(chat: &ChatState, channel: &str, name: &str) -> Option<(String, f32)> {
    latest_by(chat, channel, |l| l.sender.eq_ignore_ascii_case(name))
}

/// [`latest_said`] for this account's own lines, which carry its avatar id.
#[must_use]
pub fn latest_mine(chat: &ChatState, channel: &str) -> Option<(String, f32)> {
    let me = chat.my_id.filter(|&id| id != 0)?;
    latest_by(chat, channel, |l| l.sender_id == me)
}

fn latest_by(
    chat: &ChatState,
    channel: &str,
    whose: impl Fn(&ChatLine) -> bool,
) -> Option<(String, f32)> {
    let now = chat.clock;
    let line = chat
        .channels
        .iter()
        .find(|c| c.key == channel)?
        .lines
        .iter()
        .rev()
        .find(|l| whose(l))?;
    if line.point.is_some() || line.body.trim().is_empty() || chat.is_blocked(&line.sender) {
        return None;
    }
    let alpha = fade(now - line.arrived);
    (alpha > 0.0).then(|| (line.body.clone(), alpha))
}

/// Full strength for [`BALLOON_HOLD_SECS`], then gone over
/// [`BALLOON_FADE_SECS`].
#[must_use]
fn fade(age: f64) -> f32 {
    if age < 0.0 {
        return 1.0;
    }
    (1.0 - (age - BALLOON_HOLD_SECS) / BALLOON_FADE_SECS).clamp(0.0, 1.0) as f32
}

/// Which of a person's two positions a [`marker`] stands on. The design keeps
/// them apart by colour alone (chat-world v5): where their avatar is, in the
/// brand orange, and where their phone is, in the blue that means nothing else.
/// The shape is the same for both, because both mean "a person is here" — a
/// teardrop is a *place*.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MarkerKind {
    Avatar,
    Phone,
}

impl MarkerKind {
    pub(crate) fn colour(self) -> theme::Rgba8 {
        match self {
            Self::Avatar => theme::BRAND,
            Self::Phone => theme::REAL_LOCATION,
        }
    }
}

/// The height of a person's marker (`lib_pins::person`: the disc on its ring
/// and the stem) above its tip, so a balloon can sit over it.
#[must_use]
pub fn marker_height() -> f32 {
    DISC + 2.0 * RING + STEM
}

/// The width of a marker's name chip, from the name's width at
/// [`NAME_TEXT`]: the library's edition centres the chip under the tip by it.
#[must_use]
pub fn chip_width(name_width: f32) -> f32 {
    name_width + 14.0
}

/// How wide a balloon's words may wrap, in points: [`BALLOON_WIDTH`] less
/// its margins. The caller measures the words at [`BALLOON_TEXT`] wrapped at
/// this, for [`balloon_item`].
pub const BALLOON_WRAP: f32 = BALLOON_WIDTH - 22.0;

/// A speech balloon (the design's `.bub`) placed for the library's edition
/// (`lib_pins`): its bottom-left corner [`BALLOON_TAIL_X`] left of the person
/// and [`BALLOON_TAIL`] over them, its size the words' (`words`, measured at
/// [`BALLOON_TEXT`] wrapped at [`BALLOON_WRAP`]) plus the margins, and kept
/// on the map. In the accent colour when it is your own line (`.bub.me`).
#[must_use]
pub fn balloon_item(
    words: Vec2,
    map_rect: Rect,
    key: &str,
    anchor: Vec2,
    text: &str,
    alpha: f32,
    mine: bool,
) -> crate::systems::place_pins::BalloonItem {
    let width = words.x + 22.0;
    let height = words.y + 14.0;
    let left = (anchor.x - BALLOON_TAIL_X)
        .min(map_rect.max.x - width)
        .max(map_rect.min.x);
    let bottom = (anchor.y - BALLOON_TAIL)
        .max(map_rect.min.y + height)
        .min(map_rect.max.y);
    crate::systems::place_pins::BalloonItem {
        key: if mine {
            "balloon-mine".to_string()
        } else {
            format!("balloon:{}", key.to_lowercase())
        },
        left,
        bottom,
        width,
        tail: BALLOON_TAIL_X,
        text: text.to_string(),
        alpha,
        mine,
    }
}

/// Whether an avatar this client is drawing is the one the server placed at
/// `placed`, both camera-relative: close enough on the ground to be the same.
#[must_use]
pub fn same_avatar(drawn: DVec3, placed: DVec3) -> bool {
    (drawn.x - placed.x).hypot(drawn.z - placed.z) <= SAME_AVATAR_M
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_balloon_holds_then_fades_then_is_gone() {
        assert_eq!(fade(0.0), 1.0);
        assert_eq!(fade(BALLOON_HOLD_SECS), 1.0);
        let half = fade(BALLOON_HOLD_SECS + BALLOON_FADE_SECS / 2.0);
        assert!((half - 0.5).abs() < 1e-6, "{half}");
        assert_eq!(fade(BALLOON_HOLD_SECS + BALLOON_FADE_SECS), 0.0);
    }

    #[test]
    fn the_latest_line_is_the_one_shown_and_a_point_is_left_to_its_pin() {
        let mut chat = ChatState::default();
        chat.clock = 10.0;
        chat.receive("dm:01", 0, "Sanne".into(), "dinner?".into(), None);
        chat.receive("dm:01", 0, "sanne".into(), "7 pm".into(), None);
        assert_eq!(
            latest_said(&chat, "dm:01", "SANNE").map(|(t, _)| t),
            Some("7 pm".into())
        );
        assert_eq!(latest_said(&chat, "dm:01", "joris"), None);
        chat.my_id = Some(7);
        chat.receive("dm:01", 7, "me".into(), "Perfect".into(), None);
        assert_eq!(
            latest_mine(&chat, "dm:01").map(|(t, _)| t),
            Some("Perfect".into())
        );
        chat.clock = 10.0 + BALLOON_HOLD_SECS + BALLOON_FADE_SECS + 1.0;
        assert_eq!(latest_said(&chat, "dm:01", "sanne"), None);
    }

    #[test]
    fn a_drawn_avatar_far_from_the_answer_is_somebody_else_s_frame() {
        let placed = DVec3::new(100.0, 0.0, 100.0);
        assert!(same_avatar(DVec3::new(120.0, 50.0, 90.0), placed));
        assert!(!same_avatar(DVec3::new(300.0, 0.0, 100.0), placed));
    }
}
