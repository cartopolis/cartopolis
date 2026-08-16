//! Touch input — the finger equivalents of the mouse verbs.
//!
//! **Why this file has to exist at all.** winit's web backend routes every
//! pointer whose `pointerType` is `"touch"` to `WindowEvent::Touch` and emits
//! **no** `CursorMoved` and **no** `MouseInput` for it (see its
//! `platform_impl/web/event_loop/window_target.rs`: the mouse path and the touch
//! path are separate callbacks, and only the mouse one synthesises a cursor).
//! Bevy passes that straight through as [`TouchInput`]. So a client that only
//! reads `ButtonInput<MouseButton>` + `CursorMoved` — which is every input
//! consumer we had — is not *degraded* on a phone, it is completely inert: no
//! camera, no buttons, no panels, nothing. The same is true of a native
//! touchscreen; the web is just where it is guaranteed to be hit.
//!
//! Two bridges, deliberately separate because they answer different questions:
//!
//! 1. **Mouse emulation** (here) — the primary finger drives `ButtonInput<
//!    MouseButton>::Left` and the window's cursor position, so every existing
//!    click consumer (`picking`, `gizmo`, `navigation`'s route pick,
//!    `data_layers`' map pick, the globe's zoom-toward-cursor) works untouched.
//!    A long press stands in for the right button, i.e. the context menu.
//! 2. **Gestures** ([`TouchGestures`]) — pinch, twist and the two-finger drag
//!    have no mouse equivalent to emulate, so they are recognised into a
//!    resource and read by `free_camera::camera_controls` per camera mode.
//!
//! egui is a *third* bridge and is not here: it reads the ordered window-event
//! stream directly, so its touch handling lives beside the rest of that stream
//! in `egui_manager::pointer_events`.
//!
//! Deltas are in **logical points**, the same unit `CursorMoved::position` uses,
//! so a finger and a mouse move the camera by the same amount per point and the
//! existing sensitivity constants carry over unchanged.
//!
//! ## A finger belongs to one thing at a time ([`UiClaimRegions`])
//!
//! egui has exactly **one** pointer, and both bridges above were written around
//! it: the pointer-holding finger drives the widgets, and everything that reads
//! the world asks `egui_wants_pointer_input()` — one boolean, for the whole
//! screen — before acting. That is right for a mouse and wrong for a hand: with a
//! thumb resting on the on-screen stick (`systems::map_osd`) the boolean is true
//! for as long as it rests there, so a *second* finger anywhere else on the glass
//! could not turn the view. The stick was a modal control by accident — the one
//! thing you hold down continuously, and holding it froze the camera.
//!
//! So the glass is partitioned instead of arbitrated. Whoever draws an on-screen
//! control publishes its rects into [`UiClaimRegions`]; a finger that **lands**
//! inside one is claimed by the UI for as long as it stays down (it keeps the
//! claim when it slides out of the well, exactly as an egui drag keeps its
//! widget), and every claimed finger is then invisible to the gesture recogniser
//! and to the emulated mouse. What is left is the map's, and
//! [`TouchGestures::ui_blocked`] is what the camera asks instead of the bare
//! `egui_wants_pointer_input()`: a pointer egui only "wants" because a claimed
//! thumb is holding it does not speak for the fingers beside it.
//!
//! The claimed fingers are not thrown away — [`TouchGestures::ui_held`] and
//! [`TouchGestures::ui_released`] hand them back to the control that owns them,
//! which is how `map_osd` steers the stick from a finger that egui's single
//! pointer has never heard of.

use bevy::input::mouse::MouseButtonInput;
use bevy::input::touch::Touches;
use bevy::input::ButtonState;
use bevy::prelude::*;

/// How far (points) a finger may travel and still count as held in place. Used
/// for the long-press verdict; generous, because a finger resting on glass
/// wanders a few points on its own.
const HOLD_SLOP_PTS: f32 = 14.0;

/// How long the primary finger must stay put before it counts as a right-click.
const LONG_PRESS_SECS: f32 = 0.55;

/// Two-finger metrics below this separation (points) are refused: as the fingers
/// meet, the pinch ratio and especially the twist angle go to infinity on
/// sub-pixel noise.
const MIN_PAIR_SEPARATION_PTS: f32 = 24.0;

/// Per-frame pinch ratio clamp. A frame dropped mid-gesture (very ordinary on
/// the web, where a tile decode can eat 100 ms) otherwise arrives as one huge
/// ratio and teleports the camera.
const MAX_PINCH_PER_FRAME: f32 = 1.6;

/// The parts of the glass an on-screen control owns, in **window** logical
/// points (what [`Touches`] reports), rewritten every frame by whoever draws
/// them — `systems::map_osd` is the only writer so far.
///
/// Read one frame late: the controls are drawn in `Update` and this is consumed
/// in `PreUpdate`. That is harmless because every region here is anchored to a
/// window edge and does not move; the only stale frame is the one a control
/// appears or disappears on, where a finger landing in that exact millisecond is
/// attributed to whichever of the two the previous frame believed. Publishing
/// from the draw is still the right way round — the rects are the layout's, and
/// a second copy of the anchor arithmetic over here is how the hit area and the
/// picture drift apart.
///
/// **One writer, who rewrites the whole list.** A second publisher (the dock, the
/// HUD) would need slots keyed by owner, and is not worth adding until something
/// besides the on-screen controls is held down *while the other hand works* —
/// which is the only situation any of this exists for. What the other chrome
/// gets instead is the status quo: with a claimed thumb down, a tap on it is a
/// free finger, so it reaches neither the widget (egui's pointer is elsewhere)
/// nor, a tap carrying no drag, the camera in any visible amount.
#[derive(Resource, Default, Debug, Clone, PartialEq)]
pub struct UiClaimRegions {
    pub rects: Vec<Rect>,
}

impl UiClaimRegions {
    /// Does any region contain this point?
    fn claims(&self, at: Vec2) -> bool {
        self.rects.iter().any(|r| r.contains(at))
    }
}

/// This frame's recognised touch gestures, in logical points.
///
/// Every field is rewritten each frame by [`recognize_touch_gestures`]; there is
/// no accumulation, so a consumer that skips a frame simply misses that frame's
/// motion rather than receiving it late.
///
/// The camera reads these *inside* the same branches that gate the mouse on
/// `egui_wants_pointer_input()`, so a finger that egui has claimed (a panel
/// button, a slider) never also moves the world — the gate works for touch for
/// the same reason it works for the mouse, because the egui bridge holds a
/// pointer button down for the whole gesture. It goes through
/// [`TouchGestures::ui_blocked`] rather than being asked directly, because that
/// one boolean cannot answer for two fingers at once (see the module note).
#[derive(Resource, Debug)]
pub struct TouchGestures {
    /// Fingers down this frame that the UI has **not** claimed — the ones this
    /// resource speaks for. A thumb parked on the on-screen stick is not counted
    /// here, which is what lets the finger beside it be an ordinary one-finger
    /// drag rather than half of a pinch.
    pub fingers: usize,
    /// One-finger drag this frame. Zero unless exactly one finger is down — a
    /// second finger arriving ends the drag rather than jumping it to the
    /// centroid.
    pub drag: Vec2,
    /// Motion of the two-finger centroid this frame. The tilt gesture: only the
    /// vertical component is used by the camera, because the horizontal one
    /// cannot be told apart from a slightly off-centre twist.
    pub two_drag: Vec2,
    /// Spread ratio this frame — `>1` fingers moving apart (zoom in), `<1`
    /// together. Exactly `1.0` when there is no pair, which is the identity for
    /// every consumer, so no consumer needs to branch on "is there a pinch".
    pub pinch: f32,
    /// Twist this frame in radians, **positive clockwise on screen** (screen y
    /// points down). Chosen to carry the same sign as a rightward turn-drag, so
    /// the two gestures agree about which way "turn the view right" is.
    pub twist: f32,

    /// Fingers an on-screen control owns, and where each is now — claimed at the
    /// moment it landed inside a [`UiClaimRegions`] rect and held until it
    /// lifts. Id-sorted, like every other list here.
    ui_held: Vec<(u64, Vec2)>,
    /// Claimed fingers that lifted **this frame**, at their last position, so a
    /// control whose verb is an edge (a jump, a toggle) can see the release it
    /// would otherwise have to learn from egui's pointer.
    ui_released: Vec<(u64, Vec2)>,

    // ── recogniser state ────────────────────────────────────────────────
    /// Last frame's finger positions, id-sorted. Every finger, claimed or not:
    /// this is what says which ids are *new* this frame, and a claim is decided
    /// on the frame a finger lands.
    ///
    /// Kept here rather than read from [`bevy::input::touch::Touch::delta`],
    /// which is only refreshed on frames that carry touch *events*: hold a
    /// finger still for a frame and `delta()` hands back the previous frame's
    /// delta a second time, which reads as the map continuing to slide under a
    /// motionless finger.
    seen: Vec<(u64, Vec2)>,
    /// Which finger is standing in for the mouse. The first one down; it keeps
    /// the role until it lifts, so a pinch never re-targets the cursor.
    primary: Option<u64>,
    /// Where and when the primary finger went down, and how far it has strayed.
    press: Option<Press>,
    /// A `Left` release owed to the next frame — see [`emulate_mouse`].
    deferred_release: bool,
    /// Has a finger touched the glass at all this session? A latch, never
    /// cleared.
    ///
    /// It exists because the *device* question and the *layout* question have
    /// different right answers. `quality::is_phone` (through
    /// `theme::touch_layout`) decides the layout, and deliberately says no to a
    /// large tablet and to a touchscreen laptop — both of which render at desktop
    /// resolution and should not get finger-sized widgets. But a run that has
    /// seen a finger genuinely has no keyboard verbs available to it, whatever
    /// its resolution, so the on-screen controls (`systems::map_osd`) appear the
    /// moment one is used. This is not a second detector answering the same
    /// question: it answers "has this session been touched", which no media
    /// query can.
    ever_touched: bool,
}

#[derive(Debug, Clone, Copy)]
struct Press {
    at: Vec2,
    held: f32,
    strayed: f32,
    long_press_sent: bool,
    /// Set once the `Left` press has actually been written, so the matching
    /// release can never precede it.
    pressed: bool,
}

impl Default for TouchGestures {
    fn default() -> Self {
        Self {
            fingers: 0,
            drag: Vec2::ZERO,
            two_drag: Vec2::ZERO,
            // Multiplicative identity, not zero — see the field's doc.
            pinch: 1.0,
            twist: 0.0,
            ui_held: Vec::new(),
            ui_released: Vec::new(),
            seen: Vec::new(),
            primary: None,
            press: None,
            deferred_release: false,
            ever_touched: false,
        }
    }
}

impl TouchGestures {
    /// True when a finger is on the glass. The camera clears its mouse drag
    /// anchor on these frames: the emulated `Left` button is held for the whole
    /// gesture but no `CursorMoved` accompanies it, so an anchor left over from
    /// the last real mouse drag would survive to jump the view on the next one.
    pub fn active(&self) -> bool {
        self.fingers > 0 || !self.ui_held.is_empty()
    }

    /// Fingers an on-screen control owns, and where they are now (window points).
    /// Its own hit-testing is the control's business — this only says which
    /// fingers are *available* to it, i.e. the ones no gesture will also consume.
    pub fn ui_held(&self) -> &[(u64, Vec2)] {
        &self.ui_held
    }

    /// Claimed fingers that lifted this frame, at their last position — the
    /// release edge for a control whose verb is an event rather than a rate.
    pub fn ui_released(&self) -> &[(u64, Vec2)] {
        &self.ui_released
    }

    /// True when a finger the UI owns is down *beside* one it does not.
    ///
    /// This is the case `egui_wants_pointer_input()` gets wrong: egui's single
    /// pointer is held by the claimed thumb, so the boolean says "the UI has the
    /// pointer" on behalf of a finger that is not the one asking.
    fn escapes_ui(&self) -> bool {
        !self.ui_held.is_empty() && self.fingers > 0
    }

    /// Should a world consumer stand down, given egui's own verdict?
    ///
    /// The gate every camera gesture goes through instead of calling
    /// `egui_wants_pointer_input()` directly. Takes the verdict as a bool rather
    /// than an `egui::Context` so the rule stays a pure function.
    pub fn ui_blocked(&self, egui_wants_pointer: bool) -> bool {
        egui_wants_pointer && !self.escapes_ui()
    }

    /// True when exactly two fingers are down, i.e. the pinch/twist/tilt
    /// gesture owns the frame and the one-finger verb must not also fire.
    pub fn pair(&self) -> bool {
        self.fingers == 2
    }

    /// Has this session ever been touched? See [`TouchGestures::ever_touched`].
    pub fn ever(&self) -> bool {
        self.ever_touched
    }
}

/// Recognise this frame's gestures and emulate the mouse from the primary
/// finger. Runs in `PreUpdate` after `InputSystems`, so [`Touches`] already
/// holds this frame's events.
pub fn recognize_touch_gestures(
    touches: Res<Touches>,
    time: Res<Time>,
    regions: Res<UiClaimRegions>,
    mut gestures: ResMut<TouchGestures>,
    mut mouse_button: MessageWriter<MouseButtonInput>,
    mut windows: Query<(Entity, &mut Window)>,
) {
    // A frame with no window at all (shutdown, or a `--shot` run, which has no
    // touchscreen by construction) has nothing to emulate against.
    let Some((window_entity, mut window)) = windows.iter_mut().next() else {
        return;
    };

    let mut fingers: Vec<(u64, Vec2)> = touches.iter().map(|t| (t.id(), t.position())).collect();
    // Id order, so "the pair" is the same two fingers frame to frame and the
    // angle between them doesn't flip sign when the iteration order changes.
    fingers.sort_unstable_by_key(|(id, _)| *id);

    let previous = std::mem::take(&mut gestures.seen);

    // Hand the UI its fingers first: what is left is the map's, and neither
    // bridge below ever sees the ones the UI took. `previous` is the *whole*
    // previous frame either way — it is only ever looked up by id, and it is
    // what says which fingers are new (i.e. which are claimable) this frame.
    let mut ui_held = std::mem::take(&mut gestures.ui_held);
    let mut ui_released = std::mem::take(&mut gestures.ui_released);
    let free = update_claims(
        &mut ui_held,
        &mut ui_released,
        &previous,
        &fingers,
        &regions,
    );

    let read = recognize(&previous, &free);

    gestures.fingers = free.len();
    gestures.ever_touched |= !fingers.is_empty();
    gestures.drag = read.drag;
    gestures.pinch = read.pinch;
    gestures.twist = read.twist;
    gestures.two_drag = read.two_drag;
    gestures.ui_held = ui_held;
    gestures.ui_released = ui_released;

    let emulated = emulate_mouse(&mut gestures, &free, time.delta_secs());
    if let Some(aim) = emulated.cursor {
        // Physical pixels: that is the window's own storage, and
        // `cursor_position()` divides by the scale on the way back out.
        let physical = (aim * window.resolution.scale_factor()).as_dvec2();
        window.set_physical_cursor_position(Some(physical));
    }
    for (button, state) in emulated.buttons {
        mouse_button.write(MouseButtonInput {
            button,
            state,
            window: window_entity,
        });
    }

    gestures.seen = fingers;
}

/// Move this frame's fingers between the UI's set and the map's, and hand back
/// the map's. All three lists are id-sorted, and stay so.
///
/// A claim is decided **once, on the frame the finger lands**, and then held
/// until it lifts. Re-testing the region every frame would be the other obvious
/// rule and it is the wrong one twice over: a thumb pushed past the stick's rim
/// — which is the ordinary way to ask for full throttle — would hand itself to
/// the camera mid-push, and a finger dragging the map *into* the stick's corner
/// would be swallowed by it.
fn update_claims(
    held: &mut Vec<(u64, Vec2)>,
    released: &mut Vec<(u64, Vec2)>,
    previous: &[(u64, Vec2)],
    fingers: &[(u64, Vec2)],
    regions: &UiClaimRegions,
) -> Vec<(u64, Vec2)> {
    let position_of = |id: u64| fingers.iter().find(|(f, _)| *f == id).map(|(_, p)| *p);

    // Follow the fingers already claimed: still down means a new position, gone
    // means a release, reported for exactly the frame it happened on.
    released.clear();
    for (id, last) in std::mem::take(held) {
        match position_of(id) {
            Some(now) => held.push((id, now)),
            None => released.push((id, last)),
        }
    }

    // Claim the newcomers that landed on a control.
    for (id, pos) in fingers {
        let landed = !previous.iter().any(|(seen, _)| seen == id);
        if landed && regions.claims(*pos) {
            held.push((*id, *pos));
        }
    }
    held.sort_unstable_by_key(|(id, _)| *id);

    fingers
        .iter()
        .filter(|(id, _)| !held.iter().any(|(owned, _)| owned == id))
        .copied()
        .collect()
}

/// What one frame's finger positions say, given the previous frame's. The pure
/// half of [`recognize_touch_gestures`] — both id lists must be id-sorted.
#[derive(Debug, PartialEq)]
struct Read {
    drag: Vec2,
    two_drag: Vec2,
    pinch: f32,
    twist: f32,
}

impl Default for Read {
    /// "Nothing happened this frame" — note `pinch`, which is a ratio and whose
    /// null value is 1, not 0. See [`TouchGestures::pinch`].
    fn default() -> Self {
        Self {
            drag: Vec2::ZERO,
            two_drag: Vec2::ZERO,
            pinch: 1.0,
            twist: 0.0,
        }
    }
}

fn recognize(previous: &[(u64, Vec2)], fingers: &[(u64, Vec2)]) -> Read {
    let delta_of = |id: u64, pos: Vec2| {
        previous
            .iter()
            .find(|(prev_id, _)| *prev_id == id)
            .map(|(_, prev)| pos - *prev)
    };

    let mut read = Read::default();

    match fingers {
        // One finger: a drag, and only if we saw this same finger last frame —
        // the frame it lands on has no delta, and reporting one from thin air
        // would flick the map on every tap.
        [(id, pos)] => read.drag = delta_of(*id, *pos).unwrap_or(Vec2::ZERO),
        // Exactly two: the pinch/twist/tilt gesture. Three or more is left
        // inert rather than guessed at — nothing in the client wants a
        // three-finger verb, and picking two of them would make the reading
        // depend on which fingers happened to be lowest-id.
        [(id_a, a), (id_b, b)] => {
            let (Some(da), Some(db)) = (delta_of(*id_a, *a), delta_of(*id_b, *b)) else {
                return read;
            };
            let (now, was) = (*b - *a, (*b - db) - (*a - da));
            if now.length() < MIN_PAIR_SEPARATION_PTS || was.length() < MIN_PAIR_SEPARATION_PTS {
                return read;
            }
            read.pinch =
                (now.length() / was.length()).clamp(1.0 / MAX_PINCH_PER_FRAME, MAX_PINCH_PER_FRAME);
            // Shortest way round, so a twist across the atan2 seam is a small
            // step and not a full turn of the view.
            read.twist = wrap_pi(now.to_angle() - was.to_angle());
            read.two_drag = (da + db) * 0.5;
        }
        _ => {}
    }
    read
}

/// Drive the emulated mouse from the primary finger: the window cursor tracks
/// it, `Left` is held for the length of the gesture, and holding still long
/// enough additionally fires `Right` (the context menu).
///
/// Ordering note. This runs *after* `InputSystems`, so the `MouseButtonInput`
/// messages written here reach `ButtonInput<MouseButton>` on the **next** frame,
/// one frame after the cursor position moved with the finger. That is a
/// deliberate trade — running before `InputSystems` instead would land the
/// button in the same frame but would have to read the raw event stream rather
/// than the settled [`Touches`] set, and duplicate the pair bookkeeping above —
/// and the lag is invisible at any frame rate a phone renders at.
///
/// Press and release are never written in the same frame ([`Press::pressed`] +
/// `deferred_release`). A tap short enough to start and finish inside one frame
/// — not rare on the web, where a frame can run 100 ms — would otherwise arrive
/// as `just_pressed` on a frame where egui has already seen the pointer *lift*,
/// and `egui_wants_pointer_input()` reports "egui wants it" whenever no pointer
/// button is down. The tap would be swallowed, intermittently and only on slow
/// frames.
///
/// Returns what to *do* rather than doing it, so the state machine — which is
/// where the ordering rules above live — is unit-testable without a `World`.
#[derive(Debug, Default, PartialEq)]
struct Emulated {
    /// In order. Bevy applies them in order too, so a press followed by a
    /// release within one frame settles as "not pressed, but `just_pressed`".
    buttons: Vec<(MouseButton, ButtonState)>,
    /// Where to put the window cursor, in logical points.
    cursor: Option<Vec2>,
}

fn emulate_mouse(
    gestures: &mut TouchGestures,
    fingers: &[(u64, Vec2)],
    delta_secs: f32,
) -> Emulated {
    let mut out = Emulated::default();

    // A release owed from last frame, paid before anything else may press.
    let paid_release = gestures.deferred_release;
    if paid_release {
        gestures.deferred_release = false;
        out.buttons.push((MouseButton::Left, ButtonState::Released));
    }

    // Has the primary finger lifted (or been cancelled)?
    if let Some(id) = gestures.primary {
        if !fingers.iter().any(|(other, _)| *other == id) {
            gestures.primary = None;
            if gestures.press.take().is_some_and(|p| p.pressed) {
                gestures.deferred_release = true;
            }
        }
    }

    // Adopt a new primary finger — but only while the emulated button is
    // genuinely idle. A release written this frame has not reached
    // `ButtonInput` yet, so pressing on top of it would deliver press-then-
    // release and leave the button *up* with a finger still on the glass. That
    // is the lift-one-finger-of-a-pinch case: the survivor takes the pointer,
    // but two frames later. `first()` is the lowest id, which for a fresh
    // gesture is the finger that landed first.
    if gestures.primary.is_none() && !paid_release && !gestures.deferred_release {
        if let Some((id, pos)) = fingers.first().copied() {
            gestures.primary = Some(id);
            gestures.press = Some(Press {
                at: pos,
                held: 0.0,
                strayed: 0.0,
                long_press_sent: false,
                pressed: false,
            });
        }
    }

    let Some(primary_id) = gestures.primary else {
        return out;
    };
    let Some(pos) = fingers
        .iter()
        .find(|(id, _)| *id == primary_id)
        .map(|(_, p)| *p)
    else {
        return out;
    };

    // The cursor follows the finger on the very frame it lands, so the button
    // that arrives next frame is already aimed. Under a two-finger gesture it
    // follows the *centroid* instead — no button is coming, and the one thing
    // that reads the cursor mid-pinch is the globe's zoom-toward-cursor, which
    // wants the point between the fingers, not one of them.
    let aim = match fingers {
        [(_, a), (_, b)] => (*a + *b) * 0.5,
        _ => pos,
    };
    out.cursor = Some(aim);

    let Some(press) = gestures.press.as_mut() else {
        return out;
    };
    press.held += delta_secs;
    press.strayed = press.strayed.max(pos.distance(press.at));

    if !press.pressed {
        press.pressed = true;
        out.buttons.push((MouseButton::Left, ButtonState::Pressed));
        return out;
    }

    // Long press → the right button, once. `Left` stays down underneath: the
    // context menu opens on `just_pressed(Right)` and nothing reads the two as
    // a chord, and synthesising a `Left` release here would read as a click on
    // whatever the menu is about to cover.
    //
    // One finger only. A pinch held steady strays no further than a resting
    // thumb does, so without this a paused two-finger gesture pops the context
    // menu open under the fingers.
    if fingers.len() == 1
        && !press.long_press_sent
        && press.held >= LONG_PRESS_SECS
        && press.strayed <= HOLD_SLOP_PTS
    {
        press.long_press_sent = true;
        out.buttons.push((MouseButton::Right, ButtonState::Pressed));
        out.buttons
            .push((MouseButton::Right, ButtonState::Released));
    }
    out
}

/// Wrap an angle difference into `(-π, π]`.
fn wrap_pi(a: f32) -> f32 {
    (a + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_pi_takes_the_short_way_round() {
        let eps = 1e-5;
        assert!((wrap_pi(0.1) - 0.1).abs() < eps);
        // A twist that crosses the atan2 seam is a small step, not a full turn.
        assert!((wrap_pi(std::f32::consts::TAU - 0.1) + 0.1).abs() < eps);
        assert!((wrap_pi(-std::f32::consts::TAU + 0.1) - 0.1).abs() < eps);
    }

    /// One step of the recogniser: what `to` reads as, given `from`.
    fn step(from: &[(u64, Vec2)], to: &[(u64, Vec2)]) -> Read {
        recognize(from, to)
    }

    /// A symmetric pair `half` points either side of (200, 200).
    fn pair(half: f32) -> Vec<(u64, Vec2)> {
        vec![
            (0, Vec2::new(200.0 - half, 200.0)),
            (1, Vec2::new(200.0 + half, 200.0)),
        ]
    }

    #[test]
    fn a_single_finger_reports_its_own_delta_and_no_pinch() {
        let read = step(
            &[(0, Vec2::new(100.0, 100.0))],
            &[(0, Vec2::new(110.0, 130.0))],
        );
        assert_eq!(read.drag, Vec2::new(10.0, 30.0));
        assert_eq!(read.pinch, 1.0);
        assert_eq!(read.twist, 0.0);
        assert_eq!(read.two_drag, Vec2::ZERO);
    }

    /// The frame a finger lands on has nothing to measure against, and inventing
    /// a delta there would flick the map on every tap.
    #[test]
    fn a_finger_that_was_not_down_last_frame_reports_no_drag() {
        assert_eq!(step(&[], &[(0, Vec2::new(100.0, 100.0))]).drag, Vec2::ZERO);
    }

    #[test]
    fn fingers_moving_apart_report_a_pinch_above_one() {
        let read = step(&pair(50.0), &pair(60.0));
        assert!((read.pinch - 1.2).abs() < 1e-4, "pinch = {}", read.pinch);
        assert!(read.twist.abs() < 1e-5);
        // A symmetric spread does not move the centroid.
        assert!(read.two_drag.length() < 1e-4);
        // ...and a pair is never also a one-finger drag.
        assert_eq!(read.drag, Vec2::ZERO);
    }

    #[test]
    fn fingers_meeting_report_a_pinch_below_one() {
        let read = step(&pair(60.0), &pair(50.0));
        assert!(
            (read.pinch - 50.0 / 60.0).abs() < 1e-4,
            "pinch = {}",
            read.pinch
        );
    }

    /// A dropped frame must not arrive as one enormous zoom step.
    #[test]
    fn a_huge_spread_in_one_frame_is_clamped() {
        let read = step(&pair(20.0), &pair(400.0));
        assert!(
            (read.pinch - MAX_PINCH_PER_FRAME).abs() < 1e-5,
            "pinch = {}",
            read.pinch
        );
    }

    /// Rotating the pair clockwise on screen (y points down) is a positive
    /// twist — the same sign a rightward turn-drag carries.
    #[test]
    fn a_clockwise_twist_is_positive() {
        let read = step(
            &[(0, Vec2::new(150.0, 200.0)), (1, Vec2::new(250.0, 200.0))],
            // The right-hand finger swings down: as drawn on screen, the A→B
            // vector has rotated clockwise.
            &[(0, Vec2::new(150.0, 200.0)), (1, Vec2::new(250.0, 250.0))],
        );
        assert!(read.twist > 0.0, "twist = {}", read.twist);
    }

    #[test]
    fn two_fingers_moving_together_report_the_tilt_drag_only() {
        let read = step(
            &[(0, Vec2::new(150.0, 200.0)), (1, Vec2::new(250.0, 200.0))],
            &[(0, Vec2::new(150.0, 240.0)), (1, Vec2::new(250.0, 240.0))],
        );
        assert_eq!(read.two_drag, Vec2::new(0.0, 40.0));
        assert!((read.pinch - 1.0).abs() < 1e-5);
        assert!(read.twist.abs() < 1e-5);
    }

    /// Fingers close enough together that the angle between them is noise are
    /// refused outright rather than reported as a violent twist.
    #[test]
    fn a_pair_that_is_too_close_reports_nothing() {
        let read = step(
            &[(0, Vec2::new(200.0, 200.0)), (1, Vec2::new(205.0, 200.0))],
            &[(0, Vec2::new(200.0, 200.0)), (1, Vec2::new(200.0, 205.0))],
        );
        assert_eq!(read, Read::default());
    }

    // ── the emulated mouse ───────────────────────────────────────────────

    const DT: f32 = 1.0 / 60.0;

    /// Run frames of finger positions through the emulator and hand back the
    /// buttons each frame produced.
    fn emulate(frames: &[Vec<(u64, Vec2)>]) -> Vec<Vec<(MouseButton, ButtonState)>> {
        let mut g = TouchGestures::default();
        frames
            .iter()
            .map(|fingers| emulate_mouse(&mut g, fingers, DT).buttons)
            .collect()
    }

    fn one(x: f32, y: f32) -> Vec<(u64, Vec2)> {
        vec![(0, Vec2::new(x, y))]
    }

    #[test]
    fn a_finger_holds_the_left_button_for_the_length_of_the_gesture() {
        let out = emulate(&[
            one(10.0, 10.0),
            one(30.0, 40.0),
            one(50.0, 60.0),
            vec![],
            vec![],
        ]);
        assert_eq!(out[0], vec![(MouseButton::Left, ButtonState::Pressed)]);
        assert!(out[1].is_empty(), "held, not re-pressed: {:?}", out[1]);
        assert!(out[2].is_empty());
        // The lift is *owed*, not paid — see `emulate_mouse`.
        assert!(out[3].is_empty(), "release must not share the lift frame");
        assert_eq!(out[4], vec![(MouseButton::Left, ButtonState::Released)]);
    }

    /// The reason the release is deferred: a tap that lands and lifts inside one
    /// frame (not rare on the web, where a frame can run 100 ms) must still
    /// deliver `just_pressed` on a frame where a pointer button is down, or
    /// `egui_wants_pointer_input()` swallows it.
    #[test]
    fn a_tap_inside_a_single_frame_still_presses_before_it_releases() {
        let out = emulate(&[one(10.0, 10.0), vec![]]);
        assert_eq!(out[0], vec![(MouseButton::Left, ButtonState::Pressed)]);
        assert!(out[1].is_empty());
    }

    #[test]
    fn holding_still_fires_the_right_button_once() {
        let held: Vec<_> = (0..60).map(|_| one(10.0, 10.0)).collect();
        let out = emulate(&held);
        let rights: Vec<_> = out
            .iter()
            .flatten()
            .filter(|(b, _)| *b == MouseButton::Right)
            .collect();
        assert_eq!(
            rights,
            vec![
                &(MouseButton::Right, ButtonState::Pressed),
                &(MouseButton::Right, ButtonState::Released)
            ]
        );
    }

    #[test]
    fn a_finger_that_wanders_never_fires_the_right_button() {
        let dragged: Vec<_> = (0..60).map(|i| one(10.0 + i as f32 * 4.0, 10.0)).collect();
        assert!(
            !emulate(&dragged)
                .iter()
                .flatten()
                .any(|(b, _)| *b == MouseButton::Right),
            "a drag is not a long press"
        );
    }

    /// A pinch strays no further than a resting thumb, so without the
    /// one-finger rule a paused two-finger gesture pops the context menu open.
    #[test]
    fn a_held_pinch_never_fires_the_right_button() {
        let mut frames = vec![one(180.0, 200.0)];
        frames.extend((0..60).map(|_| pair(50.0)));
        assert!(!emulate(&frames)
            .iter()
            .flatten()
            .any(|(b, _)| *b == MouseButton::Right));
    }

    /// Lifting one finger of a pinch must not press on top of the release it
    /// owes — that would order press-then-release and leave the button *up*
    /// with a finger still on the glass.
    #[test]
    fn the_survivor_of_a_pinch_never_presses_before_the_release_lands() {
        let survivor = vec![(1, Vec2::new(250.0, 200.0))];
        let out = emulate(&[
            vec![(0, Vec2::new(150.0, 200.0))],
            pair(50.0),
            survivor.clone(),
            survivor.clone(),
            survivor.clone(),
        ]);
        let flat: Vec<_> = out.iter().flatten().copied().collect();
        assert_eq!(
            flat,
            vec![
                (MouseButton::Left, ButtonState::Pressed),
                (MouseButton::Left, ButtonState::Released),
                (MouseButton::Left, ButtonState::Pressed),
            ],
            "every press must be separated from the release before it"
        );
    }

    #[test]
    fn the_cursor_follows_the_centroid_under_two_fingers() {
        let mut g = TouchGestures::default();
        emulate_mouse(&mut g, &one(180.0, 200.0), DT);
        let aimed = emulate_mouse(&mut g, &pair(50.0), DT).cursor;
        assert_eq!(aimed, Some(Vec2::new(200.0, 200.0)));
    }

    // ── the UI's fingers ─────────────────────────────────────────────────

    /// A stick-sized well in the bottom-left corner of a 400×800 screen.
    fn stick_region() -> UiClaimRegions {
        UiClaimRegions {
            rects: vec![Rect::new(20.0, 690.0, 130.0, 780.0)],
        }
    }

    /// Drive whole frames of finger positions through the split, returning what
    /// the map was left with each frame.
    fn claims(frames: &[Vec<(u64, Vec2)>]) -> (TouchGestures, Vec<Vec<(u64, Vec2)>>) {
        let mut g = TouchGestures::default();
        let regions = stick_region();
        let mut previous: Vec<(u64, Vec2)> = Vec::new();
        let free = frames
            .iter()
            .map(|fingers| {
                let mut held = std::mem::take(&mut g.ui_held);
                let mut released = std::mem::take(&mut g.ui_released);
                let free = update_claims(&mut held, &mut released, &previous, fingers, &regions);
                g.ui_held = held;
                g.ui_released = released;
                g.fingers = free.len();
                previous = fingers.clone();
                free
            })
            .collect();
        (g, free)
    }

    fn thumb() -> (u64, Vec2) {
        (0, Vec2::new(70.0, 740.0))
    }

    fn on_map(id: u64, x: f32, y: f32) -> (u64, Vec2) {
        (id, Vec2::new(x, y))
    }

    /// The whole point: a thumb on the stick and a finger on the map are two
    /// separate inputs, and the map's one is an ordinary one-finger drag — not
    /// half of a pinch, which is what the pair used to be read as.
    #[test]
    fn a_finger_beside_the_stick_is_still_a_one_finger_drag() {
        let (g, free) = claims(&[
            vec![thumb()],
            vec![thumb(), on_map(1, 300.0, 200.0)],
            vec![thumb(), on_map(1, 320.0, 210.0)],
        ]);
        assert_eq!(g.ui_held().len(), 1, "the stick keeps its thumb");
        assert_eq!(free[2], vec![on_map(1, 320.0, 210.0)]);
        let read = recognize(&[thumb(), on_map(1, 300.0, 200.0)], &free[2]);
        assert_eq!(read.drag, Vec2::new(20.0, 10.0));
        assert_eq!(read.pinch, 1.0);
    }

    /// egui's verdict speaks for its one pointer. While that pointer is a thumb
    /// the UI has claimed, it does not speak for the fingers beside it.
    #[test]
    fn a_claimed_thumb_does_not_gate_the_other_fingers() {
        let (g, _) = claims(&[vec![thumb()], vec![thumb(), on_map(1, 300.0, 200.0)]]);
        assert!(!g.ui_blocked(true), "the map finger must still be heard");
        // …and with nothing but the thumb down, egui's verdict stands.
        let (alone, _) = claims(&[vec![thumb()]]);
        assert!(alone.ui_blocked(true));
        // A finger that never touched a control is egui's business as before.
        let (map_only, _) = claims(&[vec![on_map(1, 300.0, 200.0)]]);
        assert!(map_only.ui_blocked(true));
        assert!(!map_only.ui_blocked(false));
    }

    /// Pushing the stick past its rim is how you ask for full throttle; the
    /// claim has to survive it, or the camera takes the thumb mid-push.
    #[test]
    fn a_thumb_dragged_off_the_well_keeps_the_stick() {
        let strayed = (0, Vec2::new(200.0, 600.0));
        let (g, free) = claims(&[vec![thumb()], vec![strayed]]);
        assert_eq!(g.ui_held(), &[strayed]);
        assert!(free[1].is_empty(), "the map gets nothing: {:?}", free[1]);
    }

    /// The mirror of it: a drag that wanders *into* the corner belongs to the
    /// map it started on.
    #[test]
    fn a_map_drag_that_crosses_the_stick_is_not_claimed() {
        let (g, free) = claims(&[
            vec![on_map(1, 300.0, 200.0)],
            vec![on_map(1, 70.0, 740.0)], // inside the well now
        ]);
        assert!(g.ui_held().is_empty());
        assert_eq!(free[1].len(), 1);
    }

    /// A release is reported once, on the frame it happens, so a control whose
    /// verb is an edge can read it without keeping its own state.
    #[test]
    fn a_claimed_finger_reports_its_release_once() {
        let (g, _) = claims(&[vec![thumb()], vec![thumb()], vec![]]);
        assert_eq!(g.ui_released(), &[thumb()]);
        let (later, _) = claims(&[vec![thumb()], vec![], vec![]]);
        assert!(later.ui_released().is_empty(), "not re-reported");
        assert!(later.ui_held().is_empty());
    }

    /// A claimed thumb never becomes the emulated mouse: the world's click
    /// consumers must not see a press at the stick's corner.
    #[test]
    fn the_emulated_mouse_ignores_a_claimed_finger() {
        let (_, free) = claims(&[vec![thumb()], vec![thumb()]]);
        let mut g = TouchGestures::default();
        let out: Vec<_> = free.iter().map(|f| emulate_mouse(&mut g, f, DT)).collect();
        assert!(out
            .iter()
            .all(|e| e.buttons.is_empty() && e.cursor.is_none()));
    }

    /// End to end, through the real system and Bevy's own `Touches`: a thumb
    /// parked on a control and a finger dragging the map are two live inputs.
    ///
    /// The pure tests above pin the split; this pins the *wiring* — that the
    /// system reads [`UiClaimRegions`], that the claimed thumb survives from one
    /// frame to the next, and that what the camera reads on the far side is a
    /// one-finger drag it is allowed to act on.
    #[test]
    fn the_system_gives_the_ui_its_thumb_and_the_map_the_rest() {
        use bevy::input::touch::{TouchInput, TouchPhase};
        use bevy::window::{PrimaryWindow, WindowPlugin};

        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            bevy::input::InputPlugin,
            WindowPlugin::default(),
        ))
        .init_resource::<TouchGestures>()
        .insert_resource(stick_region())
        // `InputPlugin` fills `Touches` in `PreUpdate`; `Update` is after it,
        // which is the ordering the real app spells out explicitly.
        .add_systems(Update, recognize_touch_gestures);
        let window = app
            .world_mut()
            .query_filtered::<Entity, With<PrimaryWindow>>()
            .single(app.world())
            .expect("primary window");
        let mut finger = |id: u64, phase: TouchPhase, pos: Vec2| {
            app.world_mut().write_message(TouchInput {
                phase,
                position: pos,
                window,
                force: None,
                id,
            });
            app.update();
        };

        let thumb = Vec2::new(70.0, 740.0);
        finger(0, TouchPhase::Started, thumb);
        finger(1, TouchPhase::Started, Vec2::new(300.0, 200.0));
        finger(1, TouchPhase::Moved, Vec2::new(340.0, 230.0));

        let g = app.world().resource::<TouchGestures>();
        assert_eq!(g.ui_held(), &[(0, thumb)], "the control keeps its thumb");
        assert_eq!(g.fingers, 1, "the map is left one finger, not a pair");
        assert_eq!(g.drag, Vec2::new(40.0, 30.0));
        assert_eq!(g.pinch, 1.0, "two fingers, but not a pinch");
        assert!(!g.ui_blocked(true), "…and the camera may act on it");
    }

    /// Three fingers are deliberately inert — see `recognize`.
    #[test]
    fn three_fingers_report_nothing() {
        let from = vec![
            (0, Vec2::new(100.0, 200.0)),
            (1, Vec2::new(200.0, 200.0)),
            (2, Vec2::new(300.0, 200.0)),
        ];
        let to: Vec<_> = from
            .iter()
            .map(|(i, p)| (*i, *p + Vec2::splat(20.0)))
            .collect();
        assert_eq!(step(&from, &to), Read::default());
    }
}
