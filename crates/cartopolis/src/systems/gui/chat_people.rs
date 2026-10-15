//! The people in a private chat, on its World view: a marker where each one's
//! avatar is, and what they last said floating over it (design
//! `docs/mockups/chat-world/v5`, screen B: "the people are where they are, and
//! new messages float over whoever said them").
//!
//! Drawn from `place_pins`, which is the last UI system and already projects
//! the chat's points; this module is only the painting and the choosing.
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
//! **A balloon is painted, never interactive**, for `chat_overlay`'s reason:
//! it floats over the world, where a tap means something else. The marker
//! itself can be tapped, and flies the camera to that person.

use bevy::math::DVec3;

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
const BALLOON_WIDTH: f32 = 190.0;

/// The marker's disc diameter, its stem, and the ring round it.
const DISC: f32 = 36.0;
const STEM: f32 = 12.0;
const RING: f32 = 3.0;

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
    fn colour(self) -> egui::Color32 {
        match self {
            Self::Avatar => theme::BRAND,
            Self::Phone => theme::REAL_LOCATION,
        }
    }
}

/// The marker: the design's `.mk` — the person's disc in their colour with a
/// ring, a short stem down to a shadow at `tip`, and their name under it. The
/// ring and stem are orange at an avatar and blue at a phone ([`MarkerKind`]).
/// Returns whether it was tapped.
pub fn marker(
    ctx: &egui::Context,
    map_rect: egui::Rect,
    tip: egui::Pos2,
    name: &str,
    kind: MarkerKind,
) -> bool {
    let font = egui::FontId::proportional(11.5);
    let disc_r = DISC / 2.0;
    let height = DISC + 2.0 * RING + STEM;
    let colour = kind.colour();
    // Keyed by kind as well as name: somebody sharing their phone's location
    // with a chat whose World view is open stands on the map twice.
    let response = egui::Area::new(egui::Id::new(("chat_person", kind, name.to_lowercase())))
        .fixed_pos(tip)
        .pivot(egui::Align2::CENTER_BOTTOM)
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            ui.set_clip_rect(map_rect);
            let (_rect, response) =
                ui.allocate_exact_size(egui::vec2(DISC + 2.0 * RING, height), egui::Sense::click());
            // Painted from the exact tip, as `place_pins` paints its pins, so
            // the marker does not step a whole pixel at a time.
            let p = ui.painter();
            p.add(egui::Shape::ellipse_filled(
                tip,
                egui::vec2(6.0, 2.5),
                egui::Color32::from_black_alpha(90),
            ));
            let stem_top = tip.y - STEM;
            p.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(tip.x - 1.5, stem_top),
                    egui::pos2(tip.x + 1.5, tip.y),
                ),
                egui::CornerRadius::ZERO,
                colour,
            );
            let centre = egui::pos2(tip.x, stem_top - RING - disc_r);
            // The glow, then the ring, then the disc (`.av.ring`).
            p.circle_filled(
                centre,
                disc_r + RING + 5.0,
                egui::Color32::from_rgba_unmultiplied(colour.r(), colour.g(), colour.b(), 70),
            );
            p.circle_filled(centre, disc_r + RING, colour);
            crate::systems::chats::paint_avatar(
                p,
                centre,
                name,
                crate::systems::chats::ChatKind::Private,
                DISC,
            );
            if response.hovered() {
                p.circle_stroke(centre, disc_r + RING, egui::Stroke::new(1.5, theme::CREAM));
            }
            // The name, in a dark chip just under the tip.
            let galley = p.layout_no_wrap(name.to_string(), font.clone(), egui::Color32::WHITE);
            let chip = egui::Rect::from_center_size(
                egui::pos2(tip.x, tip.y + 5.0 + galley.size().y / 2.0),
                galley.size() + egui::vec2(14.0, 2.0),
            );
            p.rect_filled(
                chip,
                egui::CornerRadius::same(8),
                egui::Color32::from_rgba_unmultiplied(10, 14, 16, 200),
            );
            p.galley(
                chip.center() - galley.size() / 2.0,
                galley,
                egui::Color32::WHITE,
            );
            response
        })
        .inner;
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    // The colour says which position this is; the hover says it in words, as
    // the teardrop this replaced did.
    let response = match kind {
        MarkerKind::Avatar => response,
        MarkerKind::Phone => response.on_hover_text("Where their phone is"),
    };
    response.clicked()
}

/// The height of a [`marker`] above its tip, so a balloon can sit over it.
#[must_use]
pub fn marker_height() -> f32 {
    DISC + 2.0 * RING + STEM
}

/// A speech balloon (the design's `.bub`): white with ink text, the tail at its
/// lower left pointing down at `anchor`; in the accent colour when it is your
/// own line (`.bub.me`). Never interactive.
pub fn balloon(
    ctx: &egui::Context,
    map_rect: egui::Rect,
    key: &str,
    anchor: egui::Pos2,
    text: &str,
    alpha: f32,
    mine: bool,
) {
    /// From the balloon's left edge to its tail, and the tail's size.
    const TAIL_X: f32 = 18.0;
    const TAIL: f32 = 6.0;
    let fill = if mine {
        theme::ACCENT
    } else {
        egui::Color32::WHITE
    };
    egui::Area::new(egui::Id::new(("chat_person_balloon", key.to_lowercase())))
        .order(egui::Order::Middle)
        .interactable(false)
        // Pivoted on the bottom left, so a balloon that grows a line grows
        // upwards and its tail stays on the person.
        .pivot(egui::Align2::LEFT_BOTTOM)
        .fixed_pos(egui::pos2(anchor.x - TAIL_X, anchor.y - TAIL))
        .constrain_to(map_rect)
        .show(ctx, |ui| {
            ui.set_opacity(alpha);
            let frame = egui::Frame::new()
                .fill(fill)
                .corner_radius(14.0)
                .inner_margin(egui::Margin::symmetric(11, 7))
                .shadow(egui::epaint::Shadow {
                    offset: [0, 6],
                    blur: 16,
                    spread: 0,
                    color: egui::Color32::from_black_alpha(75),
                })
                .show(ui, |ui| {
                    ui.set_max_width(BALLOON_WIDTH - 22.0);
                    ui.add(
                        egui::Label::new(egui::RichText::new(text).size(13.5).color(theme::INK))
                            .wrap(),
                    );
                });
            let r = frame.response.rect;
            let x = r.left() + TAIL_X;
            ui.painter().add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(x - TAIL, r.bottom() - 0.5),
                    egui::pos2(x + TAIL, r.bottom() - 0.5),
                    egui::pos2(x, r.bottom() + TAIL),
                ],
                fill,
                egui::Stroke::NONE,
            ));
        });
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
