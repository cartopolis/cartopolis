//! Time and curve, in one place — as [`theme`](super::theme) is for colour.
//!
//! Five durations and a curve, the midas hub's own (`web/src/app.css`:
//! `--dur-press` 120 ms, `--dur-state` 200 ms, `--dur-enter` 360 ms,
//! `--ease-out`), so the app and the web UI move alike. A
//! panel never writes a time of its own: it says what kind of change this is
//! (a press, a state, a surface coming or going) and takes the duration from
//! here. `docs/mockups/fluid-ui/` is the design; the second sheet has one
//! rule per layer.
//!
//! Everything goes through egui's own `animate_*`, which asks for a repaint
//! on every pass an animation is in progress and on no other, so the quiet-frame
//! skip in `egui_manager::end_egui_frame` (`egui-quiet-frame-staleness.md`)
//! stays exactly as useful as it was: a still UI costs nothing, a moving one
//! redraws at the display's rate. Nothing here reads the clock itself.
//!
//! **Reduced motion** (`AppPrefs::motion`, Settings ▸ General) zeroes coming
//! and going and keeps press and state: those are feedback, not decoration. It
//! follows the setting and never the frame rate (the rule `effects are
//! settings, not fps`): a phone that stutters gets a faster UI (the third
//! sheet), not a stiller one.

use std::sync::atomic::{AtomicBool, Ordering};

use egui::emath::TSTransform;
use egui::{Context, Id, Response, Ui};

/// A control under the finger: scale 0.94, back on release.
pub const PRESS: f32 = 0.12;
/// A state change: colour and rim of on/off, hover, a tab, a label appearing,
/// the crossfade between the two spaces. Also egui's `Style::animation_time`.
pub const STATE: f32 = 0.2;
/// A surface arriving: a card, a page, a cover, the keyboard.
pub const ENTER: f32 = 0.36;
/// A surface leaving: two thirds of arriving. Nobody follows what goes; it has
/// to be out of the way before the next tap.
pub const LEAVE: f32 = 0.24;
/// A change nobody asked for — light to dark as the sun sets. (Its reader,
/// the palette crossfade in `ui_theme`, is the design's fifth step.)
#[allow(dead_code)]
pub const AMBIENT: f32 = 0.6;

/// How much a pressed control shrinks.
const PRESS_SCALE: f32 = 0.06;

static REDUCED: AtomicBool = AtomicBool::new(false);

/// Set from the preference once a frame (`apply_pref`).
pub fn set_reduced(on: bool) {
    REDUCED.store(on, Ordering::Relaxed);
}

/// Whether coming and going are switched off.
#[must_use]
pub fn reduced() -> bool {
    REDUCED.load(Ordering::Relaxed)
}

/// [`ENTER`] under the setting. (Its egui readers, the panels' slide, went
/// with egui's panels; kept beside [`ambient_time`] as the vocabulary's.)
#[allow(dead_code)]
#[must_use]
pub fn enter_time() -> f32 {
    if reduced() {
        0.0
    } else {
        ENTER
    }
}

/// [`LEAVE`] under the setting. (As [`enter_time`].)
#[allow(dead_code)]
#[must_use]
pub fn leave_time() -> f32 {
    if reduced() {
        0.0
    } else {
        LEAVE
    }
}

/// [`AMBIENT`] under the setting.
#[allow(dead_code)]
#[must_use]
pub fn ambient_time() -> f32 {
    if reduced() {
        0.0
    } else {
        AMBIENT
    }
}

/// The preference, applied each frame before any panel draws.
pub fn apply_pref(prefs: bevy::prelude::Res<crate::resources::AppPrefs>) {
    set_reduced(prefs.motion == crate::resources::MotionPref::Reduced);
}

/// The hub's `--ease-out`: `cubic-bezier(0.2, 0.8, 0.2, 1)`. After a third of
/// the time 86 % of the way is done, which is why 360 ms does not feel slow.
#[must_use]
pub fn ease_out(t: f32) -> f32 {
    bezier(0.2, 0.8, 0.2, 1.0, t)
}

/// A CSS cubic Bézier timing function: `y` at the `s` where `x(s) == t`, with
/// the endpoints (0, 0) and (1, 1). Newton on `x`, eight steps, which is
/// well under a thousandth for these two curves.
fn bezier(x1: f32, y1: f32, x2: f32, y2: f32, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t <= 0.0 || t >= 1.0 {
        return t;
    }
    let poly = |a: f32, b: f32, s: f32| {
        let u = 1.0 - s;
        3.0 * u * u * s * a + 3.0 * u * s * s * b + s * s * s
    };
    let slope = |a: f32, b: f32, s: f32| {
        let u = 1.0 - s;
        3.0 * u * u * a + 6.0 * u * s * (b - a) + 3.0 * s * s * (1.0 - b)
    };
    let mut s = t;
    for _ in 0..8 {
        let dx = poly(x1, x2, s) - t;
        let d = slope(x1, x2, s);
        if d.abs() < 1e-6 {
            break;
        }
        s = (s - dx / d).clamp(0.0, 1.0);
    }
    poly(y1, y2, s)
}

/// How far a surface is between gone (0) and there (1), and which way it is going.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Presence {
    /// What the caller wants: shown or not.
    pub open: bool,
    /// Linear progress towards `open`, 0‥1.
    pub linear: f32,
    /// [`ease_out`] of `linear`: the opacity, and the fraction of the way in.
    pub t: f32,
}

impl Presence {
    /// Still worth drawing: open, or not yet gone. (Read by the tests only
    /// since `linger`'s cards went with egui's screens.)
    #[allow(dead_code)]
    #[must_use]
    pub fn visible(&self) -> bool {
        self.open || self.linear > 0.001
    }
}

/// The presence of a surface keyed by `id`, `enter` towards shown, `leave`
/// towards gone ([`ENTER`] and [`LEAVE`] for a card; the keyboard answers a
/// tap and takes [`LEAVE`] to come). A surface first seen while open starts
/// from nothing, as egui's own `Area` fade-in does, rather than appearing
/// settled. Both go to nothing under reduced motion.
///
/// A caller keeps drawing while [`Presence::visible`], and makes the surface
/// non-interactive (`Area::interactable(pres.open)`, or `ui.disable()`) while
/// it leaves, so a tap in the last 240 ms lands on what is underneath.
pub fn presence_timed(ctx: &Context, id: Id, open: bool, enter: f32, leave: f32) -> Presence {
    let id = id.with("presence");
    let seen_id = id.with("seen");
    let seen =
        ctx.data_mut(|d| std::mem::replace(d.get_temp_mut_or_insert_with(seen_id, || false), true));
    if !seen && open {
        ctx.animate_bool_with_time(id, false, 0.0);
    }
    let secs = if reduced() {
        0.0
    } else if open {
        enter
    } else {
        leave
    };
    let linear = ctx.animate_bool_with_time(id, open, secs);
    Presence {
        open,
        linear,
        t: ease_out(linear),
    }
}

/// Forget a surface's presence, so its next appearance under `id` arrives
/// from nothing again — for a surface that is not drawn at all while away
/// (the keyboard), whose `presence` would otherwise find it already seen.
pub fn forget(ctx: &Context, id: Id) {
    let id = id.with("presence");
    ctx.data_mut(|d| {
        d.remove::<bool>(id.with("seen"));
    });
}

/// Apply a presence to the floating `Area` `ui` draws in: it comes up
/// `lift` points from under its resting place and fades in, and goes the same
/// way back. Call it first thing inside the area's closure. (Its first
/// readers, the dock and the menu, come once session s651's redraw of both
/// has landed; the cards use [`follow`], which is this plus the finger.)
#[allow(dead_code)]
pub fn enter(ui: &mut Ui, pres: Presence, lift: f32) {
    let t = pres.t.clamp(0.0, 1.0);
    ui.multiply_opacity(t);
    let layer = ui.layer_id();
    ui.ctx().set_transform_layer(
        layer,
        TSTransform::from_translation(egui::vec2(0.0, (1.0 - t) * lift)),
    );
}

/// Only the fade of a presence, for a surface that moves its paint rather
/// than its layer (`shift_painted`).
pub fn fade(ui: &mut Ui, pres: Presence) {
    ui.multiply_opacity(pres.t.clamp(0.0, 1.0));
}

/// Move what `layer` has painted this pass `lift` points towards where it
/// came from, by the presence's remaining distance — the picture moves and
/// the hit boxes do not. For a surface whose taps must land where it will
/// rest from its first frame (the keyboard: a finger is already on its way
/// to a key while it rises). Call it after the area has been shown.
pub fn shift_painted(ctx: &Context, layer: egui::LayerId, pres: Presence, lift: f32) {
    let t = pres.t.clamp(0.0, 1.0);
    if t < 1.0 {
        ctx.transform_layer_shapes(
            layer,
            TSTransform::from_translation(egui::vec2(0.0, (1.0 - t) * lift)),
        );
    }
}

/// The scale of a control under the finger: 1 at rest, 0.94 pressed, over
/// [`PRESS`] each way. Read from the response of the previous pass, as every
/// hand-painted control reads its hover.
#[must_use]
pub fn press(resp: &Response) -> f32 {
    let down = resp.is_pointer_button_down_on();
    let t =
        resp.ctx
            .animate_bool_with_time_and_easing(resp.id.with("press"), down, PRESS, ease_out);
    1.0 - PRESS_SCALE * t
}

/// A state that is on or off, eased over `secs` (a [`STATE`] unless the site
/// has a reason): the only way to animate a bool outside this module.
#[must_use]
pub fn toggle(ctx: &Context, id: Id, on: bool, secs: f32) -> f32 {
    ctx.animate_bool_with_time_and_easing(id, on, secs, ease_out)
}

/// A sheet's resting heights, lowest first: the title and the one action; the
/// tiles and the first facts; everything, scrolling.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub enum Rest {
    #[default]
    Peek,
    Half,
    Full,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::systems::egui_manager::UnpaintedPasses;

    #[test]
    fn the_curves_hit_their_ends_and_the_hubs_numbers() {
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        // After a third of the time, 86 % of the way (the figure the design quotes).
        assert!(
            (ease_out(1.0 / 3.0) - 0.86).abs() < 0.02,
            "{}",
            ease_out(1.0 / 3.0)
        );
        assert!(
            (ease_out(2.0 / 3.0) - 0.98).abs() < 0.02,
            "{}",
            ease_out(2.0 / 3.0)
        );
    }

    #[test]
    fn the_curves_are_monotonic_where_they_should_be() {
        let mut last = 0.0;
        for i in 1..=100 {
            let y = ease_out(i as f32 / 100.0);
            assert!(y >= last - 1e-4, "ease_out dips at {i}: {y} < {last}");
            last = y;
        }
    }

    #[test]
    fn reduced_motion_keeps_press_and_state() {
        set_reduced(true);
        assert_eq!(enter_time(), 0.0);
        assert_eq!(leave_time(), 0.0);
        assert_eq!(ambient_time(), 0.0);
        assert_eq!(PRESS, 0.12);
        assert_eq!(STATE, 0.2);
        set_reduced(false);
        assert_eq!(enter_time(), ENTER);
    }

    /// Drive a context by hand: a presence first seen open starts from
    /// nothing, is in progress for [`ENTER`], and asks for repaints while it is.
    #[test]
    fn a_surface_first_seen_open_arrives_rather_than_appears() {
        let ctx = Context::default();
        let id = Id::new("card");
        let at = |time: f64, open: bool| {
            let raw = egui::RawInput {
                time: Some(time),
                ..Default::default()
            };
            ctx.begin_pass(raw);
            let pres = presence_timed(&ctx, id, open, ENTER, LEAVE);
            let out = ctx.end_pass_unpainted();
            let delay = out
                .viewport_output
                .get(&egui::ViewportId::ROOT)
                .map_or(std::time::Duration::MAX, |v| v.repaint_delay);
            (pres, delay)
        };
        set_reduced(false);
        let (first, delay) = at(0.0, true);
        assert!(first.open && first.linear < 0.5, "{first:?}");
        assert!(first.visible());
        assert!(
            delay.is_zero(),
            "an arriving surface asks to be drawn again: {delay:?}"
        );
        let (mid, _) = at(0.12, true);
        assert!(mid.linear > 0.2 && mid.linear < 0.6, "{mid:?}");
        assert!(mid.t > mid.linear, "ease-out runs ahead of linear: {mid:?}");
        let (done, _) = at(1.0, true);
        assert!(done.open && done.linear >= 1.0, "{done:?}");
        // A settled surface no longer asks to be drawn *now*. (Not `MAX`: a
        // fresh context asks for a look or two of its own in its first passes.)
        let (_, delay) = at(1.5, true);
        let (_, delay_later) = at(2.0, true);
        assert!(
            !delay.is_zero() || !delay_later.is_zero(),
            "a settled surface asks for nothing: {delay:?} / {delay_later:?}"
        );
        // Leaving: still visible for a while, then gone.
        let (leaving, _) = at(2.1, false);
        assert!(!leaving.open && leaving.visible(), "{leaving:?}");
        let (gone, _) = at(2.1 + f64::from(LEAVE) + 0.1, false);
        assert!(!gone.visible(), "{gone:?}");
    }
}
