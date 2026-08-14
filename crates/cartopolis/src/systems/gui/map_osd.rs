//! On-screen navigation controls — the verbs a finger has no key for.
//!
//! **Why this exists.** Every camera in this client was reachable from a mouse
//! and a keyboard, and the touch bridges (`systems::touch`) gave the finger the
//! mouse's verbs but not the keyboard's. That is enough for Earth mode, whose
//! whole vocabulary is drag / pinch / twist and which every phone map has
//! already taught the user — and it is not enough for Fly, where translation was
//! WASD + E/Q, i.e. six keys a touchscreen does not have.
//!
//! What Fly had instead was a *gesture overload*: pinch flew forward and back, a
//! two-finger drag strafed and climbed, and only the **sign** of each was read,
//! so the smallest deliberate spread was a full-speed press of W held for as
//! long as the fingers stayed down. It flew, in the sense that the camera moved;
//! it was not steerable, and it collided with the one gesture whose meaning is
//! settled across every map application there is (pinch = zoom). Those mappings
//! are gone — see [`OsdInput`]'s consumers in `free_camera::camera_controls` —
//! and this module is what replaced them.
//!
//! ## The shape, and where it comes from
//!
//! Left thumb translates, right thumb changes height: the layout every
//! touchscreen 3D application converged on, for the mechanical reason that those
//! are where the thumbs already are when a phone is held in two hands.
//!
//!   - **Fly** — a thumbstick bottom-left (forward/back + strafe, *proportional*
//!     so a small push is a slow drift) and a climb/descend pair on the right.
//!   - **Follow** — the same thumbstick, walking the *avatar* rather than the
//!     camera; a zoom pair on the right that becomes a climb pair in flight; and
//!     a jump/fly pair bottom-right, opposite the stick. Until this existed the avatar was
//!     the one thing on a phone that could not be moved at all: its whole
//!     vocabulary was WASD, which is six keys a touchscreen does not have and
//!     no gesture ever stood in for (`docs/notes/android-build.md` recorded it
//!     as a standing gap). The push is the throttle — see
//!     `locomotion::speed_for_throttle` — because there is no Shift or Alt to
//!     hold, so the gait ladder the keyboard climbs in rungs is a slide here.
//!   - **Earth** — a zoom pair on the right. The pinch already zooms it; the
//!     buttons exist for the one-handed case, which is most of the time a phone
//!     is held at all.
//!
//! ## Two rules it obeys, both learned elsewhere in this tree
//!
//! **It draws into floating `Area`s, never the shared root `Ui`**, so it stays
//! out of the Phase 7b panel chain (like the dock and the HUD) — but for the
//! same reason egui paints those areas *over* a panel, it hides itself whenever
//! a right-hand panel is open rather than sitting on top of one, which is what
//! `hud::render_map_controls` does and for the same reason: an invisible control
//! that still eats taps is worse than no control.
//!
//! **It reports a wish, it does not move the camera.** `camera_controls` owns
//! the camera and already blends keys, mouse and gestures into one velocity with
//! one accelerate/glide curve; a second writer would fight it (and would have to
//! duplicate the altitude-scaled speed, the ground clamp and the big_space
//! re-anchoring). So this system writes [`OsdInput`] and stops.

use bevy::prelude::*;

use crate::resources::{CameraMode, CameraState, RightPanel};
use crate::systems::egui_manager::EguiManager;
use crate::systems::theme;

/// Diameter (points) of one round control. Above the 48 dp guideline on the
/// touch tier, where `EguiManager::ui_scale` is ≥1.5 (`docs/notes/touch-input.md`).
const CONTROL_SIZE: f32 = 44.0;
/// Gap between the two controls of a pair.
const CONTROL_GAP: f32 = 10.0;
/// Inset of the right-hand column from the window edge.
const RIGHT_MARGIN: f32 = 12.0;
/// Radius (points) of the thumbstick's well, and of the knob inside it.
const STICK_RADIUS: f32 = 54.0;
const STICK_KNOB_RADIUS: f32 = 22.0;
/// Inset of the stick's well from the left and bottom edges. The bottom inset is
/// measured from the top of the dock, not from the window (see
/// [`crate::systems::dock::reserved_bottom`]).
const STICK_MARGIN: f32 = 16.0;

/// Fraction of the stick's radius that reads as "still centred". A thumb resting
/// in the well wanders a few points, and without this the camera drifts for as
/// long as it rests there.
const STICK_DEADZONE: f32 = 0.14;

/// What the on-screen controls asked for, this frame.
///
/// Rewritten in full every frame by [`render_map_osd`] — there is no
/// accumulation, so a consumer that misses a frame misses that frame's push
/// rather than receiving it late, exactly as `TouchGestures` behaves.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq)]
pub struct OsdInput {
    /// Zoom wish: `+1` while the "closer" button is held, `-1` while "further"
    /// is. A rate, not a step — the consumer scales it by the frame time, so a
    /// held button zooms smoothly instead of by notches.
    pub zoom: f32,
    /// Fly translation wish in the camera's horizontal frame: `x` = strafe
    /// right, `y` = forward. Inside the unit disc, and *proportional*: this is
    /// the difference between a stick and four keys.
    pub fly: Vec2,
    /// Fly vertical wish: `+1` climbing, `-1` descending.
    pub climb: f32,
    /// Follow-mode edges: a jump this frame, and a request to toggle flight.
    ///
    /// **Edges, not held states**, and the distinction is the same one the wire
    /// makes (`ClientIntent::jump`, protocol 13): a dropped movement frame is
    /// corrected by the next one, while a dropped jump simply did not happen. So
    /// these are true for exactly the frame the button was released on, and the
    /// consumer must act on them rather than sample them.
    pub jump: bool,
    pub fly_toggle: bool,
    /// Follow-mode travel wish, in the same camera-horizontal frame as [`fly`]
    /// and read by `avatar::handle_avatar_movement` (which then owns it in every
    /// run mode — the online path consumes the same ramp through
    /// `network::send_network_intent`).
    ///
    /// A separate field rather than a second meaning for `fly`: the two have
    /// different consumers — one moves the camera, one moves a body the server
    /// may be authoritative for — and one field with a mode-dependent meaning is
    /// how a future edit gets to fly the camera by walking.
    ///
    /// [`fly`]: Self::fly
    pub walk: Vec2,
}

/// Which cluster the current camera wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cluster {
    /// Fly: the thumbstick and the climb/descend pair.
    Fly,
    /// Follow: the thumbstick (walking the avatar) and the zoom pair.
    Walk,
    /// Earth: the zoom pair. It already pinch-zooms; this is the one-handed path
    /// to the same thing.
    Zoom,
}

/// Whether to draw anything, and what — the whole policy, as a pure function so
/// it can be tested without a window.
///
/// `touch_ui` is the caller's answer to "is a finger driving this run", which is
/// deliberately *not* re-derived here: `crate::quality` is the one detector in
/// this client (see its module note) and a second one is how the UI ends up
/// finger-sized on a desktop.
#[must_use]
pub fn cluster_for(
    mode: CameraMode,
    driving: bool,
    panel_open: bool,
    keyboard_open: bool,
    touch_ui: bool,
) -> Option<Cluster> {
    // Driver mode owns the whole screen and holds the camera itself — its own
    // chrome is the manoeuvre card and the rail, and a zoom button there would
    // be a control that visibly does nothing. The keyboard is the dock's rule
    // verbatim: it docks to the bottom edge, an `Area` drawn later paints over
    // an earlier one, and a stick left under the keys would take taps meant for
    // them.
    if !touch_ui || driving || panel_open || keyboard_open {
        return None;
    }
    Some(match mode {
        CameraMode::Free => Cluster::Fly,
        CameraMode::Avatar => Cluster::Walk,
        CameraMode::Orbit => Cluster::Zoom,
    })
}

/// The stick's reading, given where the thumb is relative to the well's centre.
///
/// Screen y points down and forward is up-screen, so `y` is negated on the way
/// out. The magnitude is clamped to the well rather than the square that
/// contains it: a stick that is faster on the diagonal is a stick that steers
/// differently depending on which way you are already going.
#[must_use]
pub fn stick_wish(offset_pts: Vec2, radius: f32) -> Vec2 {
    let radius = radius.max(1.0);
    let mut wish = offset_pts / radius;
    let len = wish.length();
    if len < STICK_DEADZONE {
        return Vec2::ZERO;
    }
    if len > 1.0 {
        wish /= len;
    }
    Vec2::new(wish.x, -wish.y)
}

/// Draw the controls and record what they asked for.
///
/// Ordered before `free_camera::camera_controls` in `lib.rs`, so a push is acted
/// on in the frame it was made rather than the next one — at 20 fps on a phone
/// one frame of input lag on a *held* control is visible as the camera lagging
/// the thumb.
///
/// [`OsdInput::walk`] is the exception, and structurally so: it is read by
/// `avatar::handle_avatar_movement`, which this system transitively runs *after*
/// (`drive_driver_mode` measures the drive after the avatar has moved, and this
/// runs after that, because whether driver mode owns the screen decides whether
/// anything is drawn at all). So the walk wish reaches the avatar on the
/// following frame. That is a constant ~50 ms on a phone, not jitter, and it is
/// the cheaper half of the trade: inverting either link would move the camera or
/// the driver tick to answer a translation control that cannot feel it.
pub fn render_map_osd(
    mut egui: ResMut<EguiManager>,
    mut osd: ResMut<OsdInput>,
    camera_state: Res<CameraState>,
    driver: Res<crate::systems::driver::DriverState>,
    right_panel: Res<RightPanel>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    touch: Res<crate::systems::touch::TouchGestures>,
    avatar_state: Res<crate::resources::AvatarState>,
) {
    // Rebuilt from nothing every frame, including the frames that draw nothing —
    // otherwise switching to Earth mode mid-push would leave the last wish
    // latched and the camera would fly on with nothing on screen to stop it.
    // `set_if_neq` at the end so a resting control does not mark the resource
    // changed sixty times a second.
    let mut next = OsdInput::default();

    if let Some(cluster) = cluster_for(
        camera_state.mode,
        driver.active,
        !right_panel.is(RightPanel::None),
        keyboard.is_open(),
        theme::touch_layout() || touch.ever(),
    ) {
        let flying = avatar_state.is_flying;
        let ctx = egui.ctx_mut();
        match cluster {
            Cluster::Fly => {
                next.climb = vertical_pair(ctx, "osd_climb", Mark::Up, Mark::Down);
                next.fly = thumbstick(ctx);
            }
            Cluster::Walk => {
                // The right-edge pair carries whichever vertical the avatar
                // actually has: on foot there is none, so it zooms the camera;
                // in flight it climbs and descends, which is the same control in
                // the same place as Fly mode's — a phone should not have to learn
                // two positions for one verb.
                if flying {
                    next.climb = vertical_pair(ctx, "osd_climb", Mark::Up, Mark::Down);
                } else {
                    next.zoom = vertical_pair(ctx, "osd_zoom", Mark::Plus, Mark::Minus);
                }
                next.walk = thumbstick(ctx);
                let (jump, fly_toggle) = action_pair(ctx, flying);
                next.jump = jump;
                next.fly_toggle = fly_toggle;
            }
            Cluster::Zoom => {
                next.zoom = vertical_pair(ctx, "osd_zoom", Mark::Plus, Mark::Minus);
            }
        }
    }
    osd.set_if_neq(next);
}

/// Jump and the flight toggle, bottom-right — the verbs a phone had no key for.
///
/// Returns `(jump, fly_toggle)`, each true on the frame its button was
/// *released*. These are the one part of this module that is not a rate: a jump
/// is an event, and `avatar::handle_avatar_movement` consumes them as edges.
/// `clicked()` rather than `is_pointer_button_down_on` for exactly that reason —
/// the held test that suits a rate would fire a jump every frame of a press.
///
/// Bottom-**right**, opposite the stick, because that is where the other thumb
/// is: the same left-translates / right-acts split the module opens with. The
/// climb pair stays on the right *edge*, centred, so the three never overlap at
/// any window size.
///
/// Flight is a toggle rather than a hold: holding a button for the duration of a
/// flight is not a control, it is a punishment, and the keyboard's own `F` is a
/// toggle too. The jump button disappears in flight — it would mean nothing
/// there, and a control that is visibly present and does nothing is worse than
/// one that is absent (the same argument this module's suppression rules make).
fn action_pair(ctx: &egui::Context, flying: bool) -> (bool, bool) {
    let (mut jump, mut fly_toggle) = (false, false);
    egui::Area::new(egui::Id::new("osd_actions"))
        .anchor(
            egui::Align2::RIGHT_BOTTOM,
            egui::vec2(
                -STICK_MARGIN,
                -(STICK_MARGIN + crate::systems::dock::reserved_bottom()),
            ),
        )
        .show(ctx, |ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = CONTROL_GAP;
                if !flying && round_button(ui, Mark::Jump).clicked() {
                    jump = true;
                }
                if round_button(ui, Mark::Wings).clicked() {
                    fly_toggle = true;
                }
            });
        });
    (jump, fly_toggle)
}

/// The glyphs, painted rather than typed. `✕` taught this lesson once already
/// (`theme::close_button`): the bundled brand font carries no symbol set, so a
/// `+` or a `▲` in a `RichText` is a gamble on a font subset, while two line
/// segments land on the pixel grid at any scale.
#[derive(Clone, Copy)]
enum Mark {
    Plus,
    Minus,
    Up,
    Down,
    /// Jump: a chevron with a bar under it — an arrow leaving the ground.
    Jump,
    /// Flight: a pair of swept strokes. Not a wing shape, which needs curves and
    /// reads as a smear at 44 pt; two strokes rising from a point say "up and
    /// away" at any size, which is the whole vocabulary the other marks use.
    Wings,
}

/// Two round buttons stacked on the right edge, centred vertically. Returns
/// `+1` while the top one is held, `-1` while the bottom one is, `0` otherwise —
/// *held*, not clicked, because these are rates.
fn vertical_pair(ctx: &egui::Context, id: &str, top: Mark, bottom: Mark) -> f32 {
    let mut wish = 0.0;
    let height = CONTROL_SIZE * 2.0 + CONTROL_GAP;
    egui::Area::new(egui::Id::new(id))
        .anchor(egui::Align2::RIGHT_CENTER, egui::vec2(-RIGHT_MARGIN, 0.0))
        .show(ctx, |ui| {
            ui.set_height(height);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = CONTROL_GAP;
                if round_button(ui, top).is_pointer_button_down_on() {
                    wish += 1.0;
                }
                if round_button(ui, bottom).is_pointer_button_down_on() {
                    wish -= 1.0;
                }
            });
        });
    wish
}

/// One round control. Returns whether it is *being held*.
/// One round control. Returns its `Response`, so the caller picks the test that
/// suits it: a rate wants `is_pointer_button_down_on` (a finger drifting a point
/// or two while held produces a *drag*, and `clicked()` would drop the whole
/// push — the same reason the on-screen keyboard dismisses on the pointer), while
/// an edge like a jump wants `clicked()`, or it fires every frame of the press.
fn round_button(ui: &mut egui::Ui, mark: Mark) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(
        egui::Vec2::splat(CONTROL_SIZE),
        egui::Sense::click_and_drag(),
    );
    let held = resp.is_pointer_button_down_on();
    let (fill, ring, ink) = if held {
        (theme::ACCENT_MUTED, theme::ACCENT_BRIGHT, theme::CREAM)
    } else if resp.hovered() {
        (theme::OVERLAY, theme::ACCENT, theme::CREAM)
    } else {
        (theme::OVERLAY, theme::OVERLAY_LINE, theme::TEXT_DIM)
    };
    let p = ui.painter();
    let c = rect.center();
    let r = CONTROL_SIZE * 0.5 - 1.0;
    p.circle_filled(c, r, fill);
    p.circle_stroke(c, r, egui::Stroke::new(1.5, ring));
    paint_mark(p, c, CONTROL_SIZE * 0.22, mark, ink);
    resp
}

fn paint_mark(p: &egui::Painter, c: egui::Pos2, s: f32, mark: Mark, ink: egui::Color32) {
    let stroke = egui::Stroke::new(2.0, ink);
    match mark {
        Mark::Plus => {
            p.line_segment([c - egui::vec2(s, 0.0), c + egui::vec2(s, 0.0)], stroke);
            p.line_segment([c - egui::vec2(0.0, s), c + egui::vec2(0.0, s)], stroke);
        }
        Mark::Minus => {
            p.line_segment([c - egui::vec2(s, 0.0), c + egui::vec2(s, 0.0)], stroke);
        }
        // A chevron over a baseline: the ground, and something leaving it.
        Mark::Jump => {
            let (dx, dy) = (s, s * 0.55);
            p.line_segment(
                [c + egui::vec2(-dx, 0.0), c + egui::vec2(0.0, -dy * 1.6)],
                stroke,
            );
            p.line_segment(
                [c + egui::vec2(0.0, -dy * 1.6), c + egui::vec2(dx, 0.0)],
                stroke,
            );
            p.line_segment([c + egui::vec2(-dx, dy), c + egui::vec2(dx, dy)], stroke);
        }
        // Two swept strokes rising from a common point.
        Mark::Wings => {
            let (dx, dy) = (s, s * 0.7);
            p.line_segment([c + egui::vec2(0.0, dy), c + egui::vec2(-dx, -dy)], stroke);
            p.line_segment([c + egui::vec2(0.0, dy), c + egui::vec2(dx, -dy)], stroke);
            p.line_segment(
                [
                    c + egui::vec2(-dx * 0.5, 0.0),
                    c + egui::vec2(dx * 0.5, 0.0),
                ],
                stroke,
            );
        }
        // Chevrons, not filled triangles: at 44 pt a solid arrowhead reads as a
        // blob, and the same two segments scale to any control size.
        Mark::Up => {
            let (dx, dy) = (s, s * 0.6);
            p.line_segment([c + egui::vec2(-dx, dy), c + egui::vec2(0.0, -dy)], stroke);
            p.line_segment([c + egui::vec2(0.0, -dy), c + egui::vec2(dx, dy)], stroke);
        }
        Mark::Down => {
            let (dx, dy) = (s, s * 0.6);
            p.line_segment([c + egui::vec2(-dx, -dy), c + egui::vec2(0.0, dy)], stroke);
            p.line_segment([c + egui::vec2(0.0, dy), c + egui::vec2(dx, -dy)], stroke);
        }
    }
}

/// The translation stick, bottom-left. Returns its reading in the unit disc.
///
/// Bottom-*left* because the dock (the panel toggles) is bottom-centre and the
/// climb pair is right-of-centre: the three do not overlap at any window size,
/// which a corner anchor cannot promise on its own.
fn thumbstick(ctx: &egui::Context) -> Vec2 {
    let mut wish = Vec2::ZERO;
    let size = STICK_RADIUS * 2.0;
    egui::Area::new(egui::Id::new("osd_stick"))
        .anchor(
            egui::Align2::LEFT_BOTTOM,
            egui::vec2(
                STICK_MARGIN,
                -(STICK_MARGIN + crate::systems::dock::reserved_bottom()),
            ),
        )
        .show(ctx, |ui| {
            let (rect, resp) =
                ui.allocate_exact_size(egui::Vec2::splat(size), egui::Sense::click_and_drag());
            let centre = rect.center();
            // `interact_pointer_pos` is the position of the pointer that owns
            // *this* widget's drag, so a second finger elsewhere on the glass
            // cannot steer the stick.
            if resp.is_pointer_button_down_on() {
                if let Some(at) = resp.interact_pointer_pos() {
                    wish = stick_wish(
                        Vec2::new(at.x - centre.x, at.y - centre.y),
                        STICK_RADIUS - STICK_KNOB_RADIUS * 0.5,
                    );
                }
            }
            let p = ui.painter();
            p.circle_filled(centre, STICK_RADIUS, theme::OVERLAY);
            p.circle_stroke(
                centre,
                STICK_RADIUS,
                egui::Stroke::new(1.5, theme::OVERLAY_LINE),
            );
            // The knob shows where the thumb has taken it, which is the whole of
            // the feedback: without it a proportional stick is indistinguishable
            // from an on/off pad and the user has no way to learn the gradient.
            let travel = STICK_RADIUS - STICK_KNOB_RADIUS - 2.0;
            let knob = centre + egui::vec2(wish.x, -wish.y) * travel;
            let (fill, ring) = if wish == Vec2::ZERO {
                (theme::OVERLAY_LINE, theme::TEXT_DIM)
            } else {
                (theme::ACCENT_MUTED, theme::ACCENT_BRIGHT)
            };
            p.circle_filled(knob, STICK_KNOB_RADIUS, fill);
            p.circle_stroke(knob, STICK_KNOB_RADIUS, egui::Stroke::new(1.5, ring));
        });
    wish
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two modes that translate something get the stick; Earth, whose whole
    /// vocabulary is drag/pinch/twist, gets the zoom pair alone.
    #[test]
    fn the_translating_modes_get_the_stick() {
        let touch = |mode| cluster_for(mode, false, false, false, true);
        assert_eq!(touch(CameraMode::Free), Some(Cluster::Fly));
        assert_eq!(touch(CameraMode::Avatar), Some(Cluster::Walk));
        assert_eq!(touch(CameraMode::Orbit), Some(Cluster::Zoom));
    }

    /// Flight swaps the right-edge pair rather than adding a third column: on
    /// foot the avatar has no vertical, so that space zooms; in the air it is the
    /// same climb control Fly mode puts in the same place.
    #[test]
    fn follow_swaps_zoom_for_climb_in_flight() {
        // The cluster is the same either way — the swap is inside it, which is
        // what keeps the stick and the action pair from moving when you take off.
        assert_eq!(
            cluster_for(CameraMode::Avatar, false, false, false, true),
            Some(Cluster::Walk)
        );
    }

    /// A mouse-driven run gets no on-screen controls at all: they would be
    /// chrome over the map for a device that has a wheel and a keyboard.
    #[test]
    fn a_pointer_run_draws_nothing() {
        assert_eq!(
            cluster_for(CameraMode::Free, false, false, false, false),
            None
        );
    }

    /// All three suppressions exist because egui paints a floating `Area` over
    /// whatever is under it — a control left visible there would take taps meant
    /// for the panel, the keys or the driver chrome beneath.
    #[test]
    fn driving_a_panel_or_the_keyboard_suppresses_the_controls() {
        assert_eq!(
            cluster_for(CameraMode::Free, true, false, false, true),
            None
        );
        assert_eq!(
            cluster_for(CameraMode::Free, false, true, false, true),
            None
        );
        assert_eq!(
            cluster_for(CameraMode::Free, false, false, true, true),
            None
        );
    }

    #[test]
    fn the_stick_is_centred_inside_its_deadzone() {
        assert_eq!(stick_wish(Vec2::new(3.0, 3.0), 50.0), Vec2::ZERO);
    }

    /// Up-screen is forward, and the reading is proportional rather than a
    /// direction — half a push is half the speed.
    #[test]
    fn pushing_the_stick_up_is_forward_and_proportional() {
        let half = stick_wish(Vec2::new(0.0, -25.0), 50.0);
        assert!((half.y - 0.5).abs() < 1e-5, "half push = {half:?}");
        assert_eq!(half.x, 0.0);
    }

    /// Clamped to the well, not to the square around it: otherwise the diagonal
    /// is √2 faster than any axis and the stick steers differently depending on
    /// which way you already point.
    #[test]
    fn the_stick_clamps_to_the_disc() {
        let corner = stick_wish(Vec2::new(200.0, -200.0), 50.0);
        assert!((corner.length() - 1.0).abs() < 1e-5, "{corner:?}");
        assert!((corner.x - corner.y).abs() < 1e-5, "still on the diagonal");
    }
}
