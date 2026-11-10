//! Time and curve, in one place — as [`theme`](super::theme) is for colour.
//!
//! Five durations and two curves, the midas hub's own (`web/src/app.css`:
//! `--dur-press` 120 ms, `--dur-state` 200 ms, `--dur-enter` 360 ms,
//! `--ease-out`, `--ease-spring`), so the app and the web UI move alike. A
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

/// How far a surface that arrives from below starts under its resting place.
pub const LIFT: f32 = 16.0;
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

/// [`ENTER`] under the setting.
#[must_use]
pub fn enter_time() -> f32 {
    if reduced() {
        0.0
    } else {
        ENTER
    }
}

/// [`LEAVE`] under the setting.
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

/// The hub's `--ease-spring`: `cubic-bezier(0.34, 1.56, 0.64, 1)`, which
/// overshoots by 6 % and comes back. For what is *new* and small — a marker
/// appearing, a count arriving — never for a surface.
#[must_use]
pub fn spring(t: f32) -> f32 {
    bezier(0.34, 1.56, 0.64, 1.0, t)
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
    /// Still worth drawing: open, or not yet gone.
    #[must_use]
    pub fn visible(&self) -> bool {
        self.open || self.linear > 0.001
    }
}

/// The presence of a surface keyed by `id`: [`ENTER`] towards shown, [`LEAVE`]
/// towards gone. A surface first seen while open starts from nothing, as
/// egui's own `Area` fade-in does, rather than appearing settled.
///
/// A caller keeps drawing while [`Presence::visible`], and makes the surface
/// non-interactive (`Area::interactable(pres.open)`, or `ui.disable()`) while
/// it leaves, so a tap in the last 240 ms lands on what is underneath.
pub fn presence(ctx: &Context, id: Id, open: bool) -> Presence {
    let id = id.with("presence");
    let seen_id = id.with("seen");
    let seen =
        ctx.data_mut(|d| std::mem::replace(d.get_temp_mut_or_insert_with(seen_id, || false), true));
    if !seen && open {
        ctx.animate_bool_with_time(id, false, 0.0);
    }
    let secs = if open { enter_time() } else { leave_time() };
    let linear = ctx.animate_bool_with_time(id, open, secs);
    Presence {
        open,
        linear,
        t: ease_out(linear),
    }
}

/// Keep the last value of something that comes and goes for as long as its
/// presence says to draw it, so a card whose state has already been cleared
/// can still be drawn on its way out.
///
/// `Some(current)` while the thing is shown, `None` once it is not; the answer
/// is what to draw (the current value, or the last one) and its presence — or
/// `None` once it is gone.
pub fn linger<T: Clone + Send + Sync + 'static>(
    ctx: &Context,
    id: Id,
    current: Option<T>,
) -> Option<(T, Presence)> {
    let id = id.with("linger");
    let open = current.is_some();
    if let Some(v) = current.clone() {
        ctx.data_mut(|d| d.insert_temp(id, v));
    }
    let pres = presence(ctx, id, open);
    if !pres.visible() {
        ctx.data_mut(|d| d.remove::<T>(id));
        return None;
    }
    let value = current.or_else(|| ctx.data(|d| d.get_temp::<T>(id)))?;
    Some((value, pres))
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

/// Apply a presence to a floating `Area` that is *new* and small, the ring and
/// a marker: it pops about `about` (scale 0.6 → 1 on [`spring`]) and fades
/// in; it leaves by shrinking and fading on [`ease_out`].
pub fn pop(ui: &mut Ui, pres: Presence, about: egui::Pos2) {
    let scale = if pres.open {
        0.6 + 0.4 * spring(pres.linear)
    } else {
        0.6 + 0.4 * pres.t
    };
    ui.multiply_opacity(pres.t.clamp(0.0, 1.0));
    let layer = ui.layer_id();
    let about = about.to_vec2();
    ui.ctx().set_transform_layer(
        layer,
        TSTransform::from_translation(about)
            * TSTransform::from_scaling(scale)
            * TSTransform::from_translation(-about),
    );
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

/// A value that glides to `target` over `secs`: the only way to animate a
/// number outside this module.
#[must_use]
pub fn value(ctx: &Context, id: Id, target: f32, secs: f32) -> f32 {
    ctx.animate_value_with_time(id, target, secs)
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

/// What a sheet does once the finger lets go of its grabber.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SheetMove {
    Stay,
    To(Rest),
    Close,
}

/// What a sheet remembers between frames about the finger on its grabber.
#[derive(Clone, Copy, Debug, Default)]
pub struct SheetDrag {
    /// How far the finger has moved the sheet from its rest this drag, in
    /// points, down positive. The sheet follows it 1:1 (`follow`).
    offset: f32,
    /// Where a let-go sheet glides back to its rest from; consumed by
    /// `follow` on the next pass.
    settle_from: Option<f32>,
}

/// The grabber row: as tall as a finger needs, the bar 40 × 4.
pub const GRABBER_ROW: f32 = 16.0;
const GRABBER_WIDTH: f32 = 40.0;
const GRABBER_THICKNESS: f32 = 4.0;
/// How far a sheet has to be taken (finger plus a fifth of a second of its
/// speed) to go to the next rest rather than back to this one.
const DRAG_STEP: f32 = 48.0;
/// How far ahead the finger's speed is read, so a short fast flick counts.
const PROJECT_S: f32 = 0.2;
/// A flick down faster than this closes the sheet from any rest.
pub const FLICK_CLOSE: f32 = 1200.0;
/// How much of an upward pull past the top rest the sheet gives.
const RUBBER: f32 = 0.35;

/// The bar that says a sheet moves, and moves it. It follows the finger
/// (`follow` draws the offset), and on release goes where the finger's speed
/// says: a step up or down `rests` (lowest first), back to `rest`, or
/// closed — a flick down faster than [`FLICK_CLOSE`] closes from any rest, a
/// slower one from the lowest. A tap steps up, and from the top back to the
/// bottom.
pub fn grabber(
    ui: &mut Ui,
    drag: &mut SheetDrag,
    rest: Rest,
    rests: &[Rest],
    name: &str,
) -> SheetMove {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), GRABBER_ROW),
        egui::Sense::click_and_drag(),
    );
    let resp = super::theme::named(ui, resp, name);
    let live = resp.hovered() || resp.dragged();
    ui.painter().rect_filled(
        egui::Rect::from_center_size(rect.center(), egui::vec2(GRABBER_WIDTH, GRABBER_THICKNESS)),
        egui::CornerRadius::same(super::theme::RADIUS_PILL),
        if live {
            super::theme::p().accent
        } else {
            super::theme::p().line
        },
    );
    let at = rests.iter().position(|r| *r == rest).unwrap_or(0);
    let up = rests.get(at + 1).copied();
    let down = at.checked_sub(1).map(|i| rests[i]);
    if resp.dragged() {
        drag.offset += resp.drag_delta().y;
    }
    if resp.drag_stopped() {
        let speed = ui.ctx().input(|i| i.pointer.velocity().y);
        let projected = drag.offset + speed * PROJECT_S;
        drag.settle_from = Some(drag.offset);
        drag.offset = 0.0;
        return if speed >= FLICK_CLOSE {
            SheetMove::Close
        } else if projected <= -DRAG_STEP {
            up.map_or(SheetMove::Stay, SheetMove::To)
        } else if projected >= DRAG_STEP {
            down.map_or(SheetMove::Close, SheetMove::To)
        } else {
            SheetMove::Stay
        };
    }
    if resp.clicked() {
        return SheetMove::To(up.unwrap_or_else(|| rests.first().copied().unwrap_or(rest)));
    }
    SheetMove::Stay
}

/// Where the sheet is drawn this pass: its entrance (`enter`), plus the
/// finger's offset while it is held — rubber-banded past the top rest when
/// `at_top` — plus the glide back to rest after a release. First thing
/// inside the sheet's `Area`, instead of `enter`.
pub fn follow(ui: &mut Ui, id: Id, pres: Presence, drag: &mut SheetDrag, lift: f32, at_top: bool) {
    let ctx = ui.ctx().clone();
    let settle_id = id.with("settle");
    if let Some(from) = drag.settle_from.take() {
        // Seed the glide at where the finger left the sheet, then let it go.
        let _ = ctx.animate_value_with_time(settle_id, from, 0.0);
    }
    let settle = value(&ctx, settle_id, 0.0, LEAVE);
    let held = if at_top && drag.offset < 0.0 {
        drag.offset * RUBBER
    } else {
        drag.offset
    };
    let t = pres.t.clamp(0.0, 1.0);
    ui.multiply_opacity(t);
    let layer = ui.layer_id();
    ctx.set_transform_layer(
        layer,
        TSTransform::from_translation(egui::vec2(0.0, (1.0 - t) * lift + held + settle)),
    );
}

/// A page turn in progress: which page is leaving, how deep it was, when it
/// started.
#[derive(Clone, Copy, Debug)]
struct Turn<K> {
    from: K,
    from_depth: u8,
    started: f64,
}

/// How far the two pages of a turn stand from their places, as fractions of
/// the width, and how dark the parent is, at eased progress `e`. Going deeper
/// the new page comes in from the right while the parent gives way a third
/// and dims; coming back it is the other way round.
fn turn_offsets(deeper: bool, e: f32) -> (f32, f32, f32) {
    if deeper {
        (-PARENT_GIVES * e, 1.0 - e, PARENT_DIM * e)
    } else {
        (
            1.0 - (1.0 - e),
            -PARENT_GIVES * (1.0 - e),
            PARENT_DIM * (1.0 - e),
        )
    }
}

/// How much of its width the page underneath gives way during a turn.
const PARENT_GIVES: f32 = 0.3;
/// How dark the page underneath goes, as ink's alpha.
const PARENT_DIM: f32 = 0.3;

/// Draw the page `key` of a stack with `draw`, turning from the last page
/// drawn under `id` when the key changes: the new page slides in from the
/// right over a parent that gives way and dims when `depth` grew, and back out
/// to the right when it shrank. Both pages are drawn, untouchable, for
/// [`ENTER`]; `draw` is therefore asked for the old key as well as the new one
/// while a turn runs, and must be able to draw either.
pub fn page_turn<K: Copy + PartialEq + Send + Sync + 'static>(
    ui: &mut Ui,
    id: Id,
    key: K,
    depth: u8,
    mut draw: impl FnMut(&mut Ui, K),
) {
    let ctx = ui.ctx().clone();
    let now = ctx.input(|i| i.time);
    let id = id.with("page_turn");
    let last_id = id.with("last");
    let turn_id = id.with("turn");
    let last: Option<(K, u8)> = ctx.data(|d| d.get_temp(last_id));
    let mut turn: Option<Turn<K>> = ctx.data(|d| d.get_temp(turn_id));
    match last {
        Some((from, from_depth)) if from != key => {
            turn = Some(Turn {
                from,
                from_depth,
                started: now,
            });
        }
        _ => {}
    }
    ctx.data_mut(|d| d.insert_temp(last_id, (key, depth)));

    let secs = enter_time();
    let progress = turn.map(|t| {
        if secs <= 0.0 {
            1.0
        } else {
            ((now - t.started) as f32 / secs).clamp(0.0, 1.0)
        }
    });
    let rect = ui.available_rect_before_wrap();
    let clip = ui.clip_rect();
    let mut page = |ui: &mut Ui, k: K, salt: &str, dx: f32, live: bool, dim: f32| {
        let shifted = rect.translate(egui::vec2(dx * rect.width(), 0.0));
        let r = ui.scope_builder(
            egui::UiBuilder::new().max_rect(shifted).id_salt(salt),
            |ui| {
                ui.set_clip_rect(clip);
                if !live {
                    ui.disable();
                }
                draw(ui, k);
            },
        );
        if dim > 0.0 {
            let ink = super::theme::p().ink.gamma_multiply(dim);
            ui.painter()
                .rect_filled(r.response.rect.intersect(clip), 0, ink);
        }
    };
    match (turn, progress) {
        (Some(t), Some(p)) if p < 1.0 => {
            ctx.data_mut(|d| d.insert_temp(turn_id, t));
            ctx.request_repaint();
            let (old_dx, new_dx, dim) = turn_offsets(depth > t.from_depth, ease_out(p));
            let (old_dim, new_dim) = if depth > t.from_depth {
                (dim, 0.0)
            } else {
                (0.0, dim)
            };
            // The one underneath first, so the one on top paints over it.
            if depth > t.from_depth {
                page(ui, t.from, "page_leaving", old_dx, false, old_dim);
                page(ui, key, "page", new_dx, false, new_dim);
            } else {
                page(ui, key, "page", new_dx, false, new_dim);
                page(ui, t.from, "page_leaving", old_dx, false, old_dim);
            }
        }
        _ => {
            if turn.is_some() {
                ctx.data_mut(|d| d.remove::<Turn<K>>(turn_id));
            }
            page(ui, key, "page", 0.0, true, 0.0);
        }
    }
}

/// Run `f` — a `Panel::show_collapsible`, whose slide reads
/// `Style::animation_time` — with the time a surface takes to come or go,
/// and put the style's [`STATE`] back after.
pub fn with_panel_time<R>(ctx: &Context, open: bool, f: impl FnOnce() -> R) -> R {
    let secs = if open { enter_time() } else { leave_time() };
    ctx.all_styles_mut(|s| s.animation_time = secs.max(1e-3));
    let r = f();
    ctx.all_styles_mut(|s| s.animation_time = STATE);
    r
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
        // The spring overshoots and comes back.
        let peak = (0..100)
            .map(|i| spring(i as f32 / 100.0))
            .fold(0.0, f32::max);
        assert!(peak > 1.04 && peak < 1.12, "{peak}");
        assert_eq!(spring(1.0), 1.0);
    }

    #[test]
    fn a_turn_starts_off_screen_and_ends_in_place() {
        // Deeper: the new page comes from the right, the parent gives a third.
        let (old, new, dim) = turn_offsets(true, 0.0);
        assert_eq!((old, new, dim), (0.0, 1.0, 0.0));
        let (old, new, dim) = turn_offsets(true, 1.0);
        assert!((old + PARENT_GIVES).abs() < 1e-6 && new == 0.0 && (dim - PARENT_DIM).abs() < 1e-6);
        // Back: the parent returns from a third left, undimming.
        let (old, new, dim) = turn_offsets(false, 0.0);
        assert!(old == 0.0 && (new + PARENT_GIVES).abs() < 1e-6 && (dim - PARENT_DIM).abs() < 1e-6);
        let (old, new, dim) = turn_offsets(false, 1.0);
        assert!((old - 1.0).abs() < 1e-6 && new == 0.0 && dim == 0.0);
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
            let pres = presence(&ctx, id, open);
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

    #[test]
    fn linger_keeps_the_last_value_until_the_presence_is_gone() {
        let ctx = Context::default();
        let id = Id::new("place");
        let at = |time: f64, current: Option<&str>| {
            ctx.begin_pass(egui::RawInput {
                time: Some(time),
                ..Default::default()
            });
            let got = linger(&ctx, id, current.map(str::to_owned));
            let _ = ctx.end_pass_unpainted();
            got
        };
        set_reduced(false);
        let (v, pres) = at(0.0, Some("Martinitoren")).expect("shown");
        assert_eq!(v, "Martinitoren");
        assert!(pres.open);
        let _ = at(1.0, Some("Martinitoren"));
        let (v, pres) = at(1.1, None).expect("still leaving");
        assert_eq!(v, "Martinitoren", "the last value is drawn on the way out");
        assert!(!pres.open);
        assert!(at(2.0, None).is_none(), "and then it is gone");
    }
}
