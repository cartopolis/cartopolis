//! Selection handles for a text field, because a fingertip cannot hit a caret.
//!
//! egui already selects text by dragging across it, and on a touch screen it even
//! waits until the field has focus before letting a drag select rather than
//! scroll (`allow_drag_to_select` in `text_edit::builder`). What it has no notion
//! of is a **handle**: the caret is a one-pixel line and the ends of a selection
//! are nothing at all, so on glass you can start a selection and then never
//! adjust it, and you can never put the caret between two particular characters.
//! On the two platforms that have no system IME either
//! ([`crate::systems::onscreen_keyboard`]) that leaves "clear the field and type
//! it again" as the only edit, which is what the sign-in card was reduced to.
//!
//! So [`field`] wraps a `TextEdit` and draws what egui does not: a teardrop under
//! the caret, or one under each end of a selection, each of them a drag target
//! `HANDLE_TOUCH_PTS` across. Dragging one moves that end of the selection; the
//! other stays where it is, which is the whole point of two of them.
//!
//! ## Three things about this are not obvious
//!
//! **A handle is a widget, so dragging one takes focus off the field** — the same
//! trap the on-screen keyboard documents at length, and the same fix: put the
//! focus back in the same pass, with the arrow-key event filter re-locked
//! (`onscreen_keyboard::restore_focus`). Without it, a drag that ends without
//! having moved is a *click*, egui surrenders the field's focus on clicks, and
//! adjusting a selection puts the keyboard away.
//!
//! **The handles must be drawn after the field and read the same pass's galley.**
//! `TextEdit::show` hands back the galley, where it landed, and the cursor range
//! it settled on *after* handling this frame's events — so a handle drawn from it
//! is never a frame behind the text it points at. A version built on
//! `TextEditState::load` alone cannot do this: the state has char indices and no
//! geometry, and the geometry is what a finger is aiming at.
//!
//! **Only where a finger is the pointer.** On a desktop the handles are noise
//! over a caret that is already reachable, so they are gated on the same
//! predicate the on-screen keyboard uses ([`onscreen_keyboard::platform_wants_keyboard`]),
//! including its `CARTO_OSK` override — one switch turns the whole phone text
//! stack on for a workstation, rather than two that can disagree.

use crate::systems::onscreen_keyboard;
use crate::systems::theme;

/// Diameter of a handle's touch target, in points. Smaller than the 48 dp a key
/// gets: a handle is dragged, not tapped, and a drag corrects itself as it goes —
/// but it still has to be *found*, so it is not the 6 pt the caret is.
const HANDLE_TOUCH_PTS: f32 = 30.0;
/// Radius of the drawn knob. The touch target is much larger than the paint, the
/// way every finger-sized control on this client is.
const HANDLE_RADIUS: f32 = 7.0;
/// How far below the text's baseline row the knob's centre sits, so the knob does
/// not cover the character it is pointing at. Enough that the touch target clears
/// most of the text row too: the target is registered after the field and would
/// otherwise win the hit test over the very characters you are trying to tap.
const HANDLE_DROP: f32 = 8.0;

/// Which end of the selection a handle drags.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum End {
    /// The end that moves under shift+arrows, and the caret when there is no
    /// selection.
    Primary,
    /// The anchor.
    Secondary,
}

impl End {
    fn id_salt(self) -> &'static str {
        match self {
            Self::Primary => "text_handle_primary",
            Self::Secondary => "text_handle_secondary",
        }
    }
}

/// Show a text field with finger-sized selection handles.
///
/// A drop-in for `ui.add(TextEdit::…)`: same builder, same `Response`.
pub fn field(ui: &mut egui::Ui, edit: egui::TextEdit<'_>) -> egui::Response {
    let out = edit.show(ui);
    let response = out.response.response.clone();
    if !onscreen_keyboard::platform_wants_keyboard() {
        return response;
    }
    let id = response.id;
    if !ui.memory(|m| m.has_focus(id)) {
        return response;
    }
    let Some(range) = out.cursor_range else {
        return response;
    };

    let ctx = ui.ctx().clone();
    let galley = out.galley.clone();
    let origin = out.galley_pos;
    // Where a char index is on screen, as the row it sits in.
    let row_of =
        |cursor: egui::text::CCursor| galley.pos_from_cursor(cursor).translate(origin.to_vec2());

    let mut moved = None;
    let mut interacted = false;

    // One handle when the two ends coincide (a caret), two when they do not. The
    // caret handle is not decoration: without it there is no way to put the caret
    // anywhere but where the tap landed, and a tap is several characters wide.
    let ends: &[End] = if range.primary == range.secondary {
        &[End::Primary]
    } else {
        &[End::Primary, End::Secondary]
    };

    for &end in ends {
        let cursor = match end {
            End::Primary => range.primary,
            End::Secondary => range.secondary,
        };
        let row = row_of(cursor);
        let knob = egui::pos2(row.center().x, row.bottom() + HANDLE_DROP);
        let target = egui::Rect::from_center_size(knob, egui::Vec2::splat(HANDLE_TOUCH_PTS));
        let resp = ui.interact(target, id.with(end.id_salt()), egui::Sense::drag());

        if resp.dragged() {
            if let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) {
                // The pointer is on the knob, which is *below* the text; aim at
                // the row the handle is attached to rather than at the finger, or
                // every drag falls off the bottom of a single-line field and
                // clamps to the last character.
                let aim = egui::pos2(pos.x, row.center().y);
                moved = Some((end, galley.cursor_from_pos(aim - origin)));
            }
        }
        interacted |= resp.dragged() || resp.drag_stopped();

        paint_handle(ui, knob, resp.dragged());
    }

    if let Some((end, cursor)) = moved {
        let mut range = range;
        match end {
            End::Primary => range.primary = cursor,
            End::Secondary => range.secondary = cursor,
        }
        let mut state = out.state.clone();
        state.cursor.set_char_range(Some(range));
        state.store(&ctx, id);
        ctx.request_repaint();
    }

    if interacted {
        // A drag that ends where it started is a click, and egui answers a click
        // outside a focused widget by taking its focus away — which would put the
        // on-screen keyboard down every time a selection is adjusted.
        onscreen_keyboard::restore_focus(&ctx, id);
    }

    response
}

/// A teardrop: a disc with a corner pointing up at the character it marks.
fn paint_handle(ui: &egui::Ui, centre: egui::Pos2, dragging: bool) {
    let colour = if dragging {
        theme::ACCENT_BRIGHT
    } else {
        theme::ACCENT
    };
    let p = ui.painter();
    // The stem, drawn first so the disc covers where it meets the text.
    p.line_segment(
        [
            egui::pos2(centre.x, centre.y - HANDLE_RADIUS - HANDLE_DROP),
            centre,
        ],
        egui::Stroke::new(1.5, colour),
    );
    p.circle(
        centre,
        HANDLE_RADIUS,
        colour,
        egui::Stroke::new(1.0, theme::INK),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The knob is a finger-sized target hanging below a caret that is one point
    /// wide. If these ever converge, the feature is gone: the handle is only
    /// worth drawing because it can be grabbed where the caret cannot.
    #[test]
    fn a_handle_is_far_larger_than_the_caret_it_stands_for() {
        assert!(HANDLE_TOUCH_PTS >= 4.0 * HANDLE_RADIUS);
        // And it hangs clear of the text row rather than over the character it
        // points at.
        assert!(HANDLE_DROP > 0.0);
    }
}
