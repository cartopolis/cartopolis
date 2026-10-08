//! The ring of actions: round buttons around the thing that was tapped, the
//! primary one in brand orange at the top, ✕ in the middle where the thumb
//! already is, and a pill underneath saying what was tapped
//! (`docs/mockups/layers-and-ring/v1.png`, drawn first for people in chat-world
//! v4). One widget for every subject — a point on the map, an object, a
//! satellite — so the gesture means the same thing wherever it lands.
//!
//! Drawing only: the caller decides which actions exist, and acts on the key
//! [`show`] hands back. It is a floating `Area` on the foreground order, outside
//! the panel chain, like the place and building cards.

use egui::{Align2, Color32, FontId, Id, Pos2, Rect, Sense, Stroke, Vec2};

use crate::systems::theme;

/// One button on the ring.
#[derive(Clone, Copy, Debug)]
pub struct RingAction {
    /// What the caller matches on when it is pressed.
    pub key: &'static str,
    pub icon: &'static str,
    /// Short — it is set under the icon inside a 56 pt disc.
    pub label: &'static str,
    /// Hover text, for the desktop.
    pub hint: &'static str,
}

/// What happened to the ring this frame.
#[derive(Debug, Default, PartialEq, Eq)]
pub enum RingOutcome {
    #[default]
    Open,
    /// A button was pressed: its key. The caller closes the ring.
    Chosen(&'static str),
    /// ✕, Escape, or a press anywhere off the ring.
    Dismissed,
}

/// Radius from the centre to each button's centre, points.
const RADIUS: f32 = 78.0;
/// A button's radius, points: 56 pt across, above the 48 dp touch floor.
const BUTTON_R: f32 = 28.0;
/// The centre ✕'s radius.
const CLOSE_R: f32 = 17.0;

/// Where the `i`th of `n` buttons sits: evenly round the circle, the first (the
/// primary) straight up.
#[must_use]
pub fn slot(i: usize, n: usize) -> Vec2 {
    let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * i as f32 / n.max(1) as f32;
    Vec2::new(a.cos(), a.sin()) * RADIUS
}

/// The ring's centre, pulled in from the screen edges so every button and the
/// pill stay on screen.
#[must_use]
pub fn clamp_centre(at: Pos2, screen: Rect) -> Pos2 {
    let margin = RADIUS + BUTTON_R + 6.0;
    let below = RADIUS + BUTTON_R + 54.0;
    Pos2::new(
        at.x.clamp(
            screen.left() + margin,
            (screen.right() - margin).max(screen.left() + margin),
        ),
        at.y.clamp(
            screen.top() + margin,
            (screen.bottom() - below).max(screen.top() + margin),
        ),
    )
}

/// Draw the ring around `at` (ui points) and report what was pressed. `id`
/// must be stable for as long as this ring is open, and new for a new one: the
/// press that opened it is still down on the first frame, and is not a press
/// "off the ring".
pub fn show(
    ctx: &egui::Context,
    id: Id,
    at: Pos2,
    title: &str,
    detail: &str,
    actions: &[RingAction],
) -> RingOutcome {
    let screen = ctx.viewport_rect();
    let centre = clamp_centre(at, screen);
    let extent = Vec2::splat(RADIUS + BUTTON_R + 4.0);
    let mut outcome = RingOutcome::Open;

    // A press that began before the ring existed — the one that opened it — is
    // not a dismissal. Remember the frame the ring first showed.
    let frame = ctx.cumulative_pass_nr();
    let first = ctx.data_mut(|d| *d.get_temp_mut_or_insert_with(id.with("opened"), || frame));

    let area = egui::Area::new(id)
        .order(egui::Order::Foreground)
        .fixed_pos(centre - extent)
        .show(ctx, |ui| {
            let (rect, _) =
                ui.allocate_exact_size(extent * 2.0 + Vec2::new(0.0, 60.0), Sense::hover());
            let c = rect.min + extent;
            let painter = ui.painter().clone();
            painter.circle_stroke(c, RADIUS, Stroke::new(1.0, Color32::from_white_alpha(40)));

            for (i, action) in actions.iter().enumerate() {
                let p = c + slot(i, actions.len());
                let hit = Rect::from_center_size(p, Vec2::splat(BUTTON_R * 2.0));
                let resp = ui
                    .interact(hit, id.with(action.key), Sense::click())
                    .on_hover_text(action.hint);
                let primary = i == 0;
                let fill = match (primary, resp.hovered()) {
                    (true, false) => theme::BRAND,
                    (true, true) => theme::ACCENT_PRESSED,
                    (false, false) => theme::GLASS,
                    (false, true) => theme::SURFACE_HOVER,
                };
                let ink = if primary { Color32::WHITE } else { theme::TEXT };
                painter.circle(p, BUTTON_R, fill, Stroke::new(1.0, theme::OVERLAY_LINE));
                painter.text(
                    p - Vec2::new(0.0, 6.0),
                    Align2::CENTER_CENTER,
                    action.icon,
                    FontId::proportional(19.0),
                    ink,
                );
                painter.text(
                    p + Vec2::new(0.0, 12.0),
                    Align2::CENTER_CENTER,
                    action.label,
                    FontId::proportional(9.5),
                    ink,
                );
                if resp.clicked() {
                    outcome = RingOutcome::Chosen(action.key);
                }
            }

            // ✕ in the middle: painted from two segments, as `theme::close_button`
            // is, because the bundled fonts have no ✕.
            let close = ui.interact(
                Rect::from_center_size(c, Vec2::splat(CLOSE_R * 2.0)),
                id.with("close"),
                Sense::click(),
            );
            painter.circle_filled(
                c,
                CLOSE_R,
                if close.hovered() {
                    theme::SURFACE_HOVER
                } else {
                    theme::LABEL_CHIP
                },
            );
            let k = 5.5;
            let s = Stroke::new(1.8, theme::TEXT_DIM);
            painter.line_segment([c + Vec2::new(-k, -k), c + Vec2::new(k, k)], s);
            painter.line_segment([c + Vec2::new(k, -k), c + Vec2::new(-k, k)], s);
            if close.clicked() {
                outcome = RingOutcome::Dismissed;
            }

            // The pill: what this ring is about, so the buttons never carry text
            // longer than a word.
            let title_g = painter.layout_no_wrap(
                title.to_owned(),
                FontId::proportional(theme::SIZE_BODY),
                theme::CREAM,
            );
            let detail_g = painter.layout_no_wrap(
                detail.to_owned(),
                FontId::proportional(theme::SIZE_CHIP),
                theme::TEXT_DIM,
            );
            let w = title_g.size().x.max(detail_g.size().x) + 24.0;
            let h = title_g.size().y
                + if detail.is_empty() {
                    0.0
                } else {
                    detail_g.size().y
                }
                + 12.0;
            let top = c.y + RADIUS + BUTTON_R + 8.0;
            let pill = Rect::from_center_size(Pos2::new(c.x, top + h / 2.0), Vec2::new(w, h));
            painter.rect_filled(pill, 12.0, theme::GLASS);
            painter.rect_stroke(
                pill,
                12.0,
                Stroke::new(1.0, theme::OVERLAY_LINE),
                egui::StrokeKind::Inside,
            );
            let title_h = title_g.size().y;
            painter.galley(
                Pos2::new(c.x - title_g.size().x / 2.0, pill.top() + 6.0),
                title_g,
                theme::CREAM,
            );
            if !detail.is_empty() {
                painter.galley(
                    Pos2::new(c.x - detail_g.size().x / 2.0, pill.top() + 6.0 + title_h),
                    detail_g,
                    theme::TEXT_DIM,
                );
            }
            c
        });

    let c = area.inner;
    if outcome == RingOutcome::Open {
        let (pressed, at_ptr, escape) = ctx.input(|i| {
            (
                i.pointer.any_pressed(),
                i.pointer.interact_pos(),
                i.key_pressed(egui::Key::Escape),
            )
        });
        let off_ring = at_ptr.is_some_and(|p| p.distance(c) > RADIUS + BUTTON_R + 4.0);
        if escape || (pressed && off_ring && frame > first) {
            outcome = RingOutcome::Dismissed;
        }
    }
    if outcome != RingOutcome::Open {
        ctx.data_mut(|d| d.remove::<u64>(id.with("opened")));
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_primary_is_on_top_and_the_rest_go_round() {
        let top = slot(0, 5);
        assert!(top.x.abs() < 1e-3 && top.y < 0.0, "{top:?}");
        // Evenly spaced: every neighbour is the same chord apart.
        let chord = (slot(1, 5) - slot(0, 5)).length();
        for i in 1..5 {
            let d = (slot((i + 1) % 5, 5) - slot(i, 5)).length();
            assert!((d - chord).abs() < 1e-3);
        }
    }

    #[test]
    fn the_ring_is_pulled_on_screen() {
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 800.0));
        let c = clamp_centre(Pos2::new(2.0, 790.0), screen);
        assert!(c.x - RADIUS - BUTTON_R >= 0.0);
        assert!(c.y + RADIUS + BUTTON_R + 40.0 <= 800.0);
    }
}
