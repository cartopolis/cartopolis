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
/// pointer button down for the whole gesture.
#[derive(Resource, Debug)]
pub struct TouchGestures {
    /// Fingers down this frame.
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

    // ── recogniser state ────────────────────────────────────────────────
    /// Last frame's finger positions, id-sorted.
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
        self.fingers > 0
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
    let read = recognize(&previous, &fingers);

    gestures.fingers = fingers.len();
    gestures.ever_touched |= !fingers.is_empty();
    gestures.drag = read.drag;
    gestures.pinch = read.pinch;
    gestures.twist = read.twist;
    gestures.two_drag = read.two_drag;

    let emulated = emulate_mouse(&mut gestures, &fingers, time.delta_secs());
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
