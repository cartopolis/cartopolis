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

// ── the double tap ───────────────────────────────────────────────────
//
// The verdict is written as `pending_*` and only shifted into the public
// `double_tap()` by the system, a frame later — which is the frame
// `ButtonInput` reports the press on. These tests read the pending fields
// because they drive the state machine directly;
// `a_double_tap_is_reported_on_the_frame_the_click_arrives` pins the shift.

/// Land a finger. The press, and the verdict on it, both happen here.
fn land(g: &mut TouchGestures, at: Vec2) {
    emulate_mouse(g, &[(0, at)], DT);
}

/// One whole tap: land, lift, and let the owed release be paid, so the next
/// finger down is free to take the pointer.
fn tap(g: &mut TouchGestures, at: Vec2) {
    land(g, at);
    for _ in 0..2 {
        emulate_mouse(g, &[], DT);
    }
}

#[test]
fn two_taps_in_the_same_place_are_a_double_tap() {
    let mut g = TouchGestures::default();
    tap(&mut g, Vec2::new(200.0, 300.0));
    assert!(!g.pending_double, "one tap is never a double tap");
    // Not the same pixel — a finger that lifts and returns never is.
    land(&mut g, Vec2::new(206.0, 294.0));
    assert!(g.pending_click, "still an ordinary emulated press");
    assert!(g.pending_double);
}

/// And a third tap is not a second double tap, or a finger drumming on the
/// glass would fire the verb on every other beat.
#[test]
fn a_third_tap_starts_over() {
    let mut g = TouchGestures::default();
    tap(&mut g, Vec2::new(200.0, 300.0));
    tap(&mut g, Vec2::new(200.0, 300.0));
    assert!(g.pending_double, "the second tap");
    land(&mut g, Vec2::new(200.0, 300.0));
    assert!(!g.pending_double, "the third");
}

/// The case the whole thing exists for: the first frame of a pan is a press
/// too, and two pans in a row must not add up to a double tap.
#[test]
fn a_drag_is_not_half_of_a_double_tap() {
    let mut g = TouchGestures::default();
    emulate_mouse(&mut g, &[(0, Vec2::new(200.0, 300.0))], DT);
    emulate_mouse(&mut g, &[(0, Vec2::new(200.0, 380.0))], DT);
    for _ in 0..2 {
        emulate_mouse(&mut g, &[], DT);
    }
    land(&mut g, Vec2::new(200.0, 380.0));
    assert!(!g.pending_double);
}

/// A long press is the context menu's verb and is not a tap either.
#[test]
fn a_long_press_is_not_half_of_a_double_tap() {
    let mut g = TouchGestures::default();
    let at = Vec2::new(200.0, 300.0);
    for _ in 0..((LONG_PRESS_SECS / DT) as usize + 2) {
        emulate_mouse(&mut g, &[(0, at)], DT);
    }
    for _ in 0..2 {
        emulate_mouse(&mut g, &[], DT);
    }
    land(&mut g, at);
    assert!(!g.pending_double);
}

#[test]
fn two_taps_far_apart_are_two_taps() {
    let mut g = TouchGestures::default();
    tap(&mut g, Vec2::new(100.0, 100.0));
    land(&mut g, Vec2::new(300.0, 400.0));
    assert!(!g.pending_double);
}

#[test]
fn a_second_tap_that_comes_too_late_is_a_fresh_one() {
    let mut g = TouchGestures::default();
    let at = Vec2::new(200.0, 300.0);
    tap(&mut g, at);
    for _ in 0..((DOUBLE_TAP_SECS / DT) as usize + 2) {
        emulate_mouse(&mut g, &[], DT);
    }
    land(&mut g, at);
    assert!(!g.pending_double);
}

/// The wiring, and the off-by-one it would be easy to ship: the press is
/// *written* one frame before `ButtonInput` reports it, so the verdict has
/// to arrive with the click and not before it. A consumer reads both on the
/// same frame — `building_select::pick_building` is the one that does.
#[test]
fn a_double_tap_is_reported_on_the_frame_the_click_arrives() {
    use bevy::input::touch::{TouchInput, TouchPhase};
    use bevy::window::{PrimaryWindow, WindowPlugin};

    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        bevy::input::InputPlugin,
        WindowPlugin::default(),
    ))
    .init_resource::<TouchGestures>()
    .init_resource::<UiClaimRegions>()
    .add_systems(Update, recognize_touch_gestures);
    let window = app
        .world_mut()
        .query_filtered::<Entity, With<PrimaryWindow>>()
        .single(app.world())
        .expect("primary window");

    // What a consumer would see each frame — the three booleans
    // `pick_building` reads together.
    let mut seen: Vec<(bool, bool, bool)> = Vec::new();
    let mut frame = |app: &mut App, event: Option<(TouchPhase, Vec2)>| {
        if let Some((phase, position)) = event {
            app.world_mut().write_message(TouchInput {
                phase,
                position,
                window,
                force: None,
                id: 0,
            });
        }
        app.update();
        let clicked = app
            .world()
            .resource::<ButtonInput<MouseButton>>()
            .just_pressed(MouseButton::Left);
        let gestures = app.world().resource::<TouchGestures>();
        seen.push((clicked, gestures.touch_click(), gestures.double_tap()));
    };

    let at = Vec2::new(200.0, 300.0);
    frame(&mut app, Some((TouchPhase::Started, at)));
    frame(&mut app, Some((TouchPhase::Ended, at)));
    frame(&mut app, None);
    frame(&mut app, None);
    frame(&mut app, Some((TouchPhase::Started, at)));
    frame(&mut app, None);
    frame(&mut app, None);

    let clicks: Vec<_> = seen.iter().filter(|(clicked, ..)| *clicked).collect();
    assert_eq!(clicks.len(), 2, "one click per tap: {seen:?}");
    assert_eq!(
        *clicks[0],
        (true, true, false),
        "the first tap is a finger's, and opens nothing"
    );
    assert_eq!(
        *clicks[1],
        (true, true, true),
        "the second is the double tap"
    );
    // And the verdict is spent with the click: a later frame must not still
    // be claiming a press that has already been acted on.
    assert_eq!(
        seen.last().copied(),
        Some((false, false, false)),
        "{seen:?}"
    );
}

// ── a tap that begins and ends inside one frame ──────────────────────

/// The phone case, on the pure half: a finger present in neither the level
/// nor last frame's list is still a finger.
#[test]
fn a_finger_that_landed_and_lifted_in_one_frame_is_still_seen() {
    let at = Vec2::new(200.0, 300.0);
    assert_eq!(
        fingers_this_frame(&[], &[(0, at)], &[]),
        vec![(0, at)],
        "a tap inside one frame is the whole tap, not nothing"
    );
}

/// …and an ordinary lift is *not* extended by it, or every release on every
/// platform would arrive a frame late to fix a case none of them has.
#[test]
fn an_ordinary_lift_is_not_held_over() {
    let at = Vec2::new(200.0, 300.0);
    assert!(fingers_this_frame(&[], &[(0, at)], &[(0, at)]).is_empty());
}

/// A finger still down is never duplicated by its own stale release.
#[test]
fn a_live_finger_is_listed_once() {
    let at = Vec2::new(200.0, 300.0);
    assert_eq!(
        fingers_this_frame(&[(0, at)], &[(0, at)], &[]),
        vec![(0, at)]
    );
}

/// The bug as it reached the phone, end to end: Android delivers touch far
/// faster than a city renders, so both edges of a brisk tap land in one
/// frame's batch. Before this, `Touches::iter()` was empty by the time the
/// recogniser ran and the tap simply never happened — which is what made the
/// building card unreachable on a handset while every panel still worked.
#[test]
fn a_tap_whose_both_edges_land_in_one_frame_still_clicks() {
    use bevy::input::touch::{TouchInput, TouchPhase};
    use bevy::window::{PrimaryWindow, WindowPlugin};

    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        bevy::input::InputPlugin,
        WindowPlugin::default(),
    ))
    .init_resource::<TouchGestures>()
    .init_resource::<UiClaimRegions>()
    .add_systems(Update, recognize_touch_gestures);
    let window = app
        .world_mut()
        .query_filtered::<Entity, With<PrimaryWindow>>()
        .single(app.world())
        .expect("primary window");

    let at = Vec2::new(200.0, 300.0);
    let mut seen: Vec<(bool, bool, bool)> = Vec::new();
    // Both phases in one batch, twice — the double tap a person makes at the
    // speed a person makes it, on a device slow enough to see neither half.
    for _ in 0..2 {
        for phase in [TouchPhase::Started, TouchPhase::Ended] {
            app.world_mut().write_message(TouchInput {
                phase,
                position: at,
                window,
                force: None,
                id: 0,
            });
        }
        for _ in 0..3 {
            app.update();
            let clicked = app
                .world()
                .resource::<ButtonInput<MouseButton>>()
                .just_pressed(MouseButton::Left);
            let g = app.world().resource::<TouchGestures>();
            seen.push((clicked, g.touch_click(), g.double_tap()));
        }
    }

    let clicks: Vec<_> = seen.iter().filter(|(clicked, ..)| *clicked).collect();
    assert_eq!(clicks.len(), 2, "one click per tap: {seen:?}");
    assert_eq!(*clicks[0], (true, true, false), "{seen:?}");
    assert_eq!(
        *clicks[1],
        (true, true, true),
        "the second tap pairs, so the card opens: {seen:?}"
    );
}

/// A double tap at a handset's frame time still pairs.
///
/// The worry this answers is that the two dead frames between a lift and the
/// next adoption (the recogniser owes a release, and nothing may press on
/// top of it) eat [`DOUBLE_TAP_SECS`]. They do not, and the reason is worth
/// keeping: a *slow* second tap arrives long after the release was paid, and
/// a *fast* one has most of the window left over. It is only worth a test,
/// not a mechanism.
#[test]
fn a_double_tap_survives_a_handsets_frame_time() {
    const SLOW_DT: f32 = 0.08; // 12.5 fps
    let at = Vec2::new(200.0, 300.0);
    for gap_secs in [0.16_f32, 0.25] {
        let mut g = TouchGestures::default();
        emulate_mouse(&mut g, &[(0, at)], SLOW_DT); // tap one lands
        emulate_mouse(&mut g, &[], SLOW_DT); // …and lifts
        for _ in 0..((gap_secs / SLOW_DT) as usize) {
            emulate_mouse(&mut g, &[], SLOW_DT);
        }
        for _ in 0..3 {
            emulate_mouse(&mut g, &[(1, at)], SLOW_DT);
        }
        assert!(g.pending_double, "a {gap_secs}s gap is a double tap");
    }
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

/// Feed a pair's frames through [`recognize`] and [`lock_pair`], as the system
/// does, and add up what came out.
fn locked(frames: &[[Vec2; 2]]) -> (Vec2, f32, f32, f32) {
    let mut lock = PairLock::Off;
    let mut previous: Vec<(u64, Vec2)> = Vec::new();
    let (mut drag, mut pinch, mut twist, mut tilt) = (Vec2::ZERO, 1.0, 0.0, 0.0);
    for [a, b] in frames {
        let now = vec![(0, *a), (1, *b)];
        let read = lock_pair(&mut lock, &now, recognize(&previous, &now));
        drag += read.two_drag;
        pinch *= read.pinch;
        twist += read.twist;
        tilt += read.tilt;
        previous = now;
    }
    (drag, pinch, twist, tilt)
}

/// The fingers spread while they slide right: the zoom is steered, so the
/// slide arrives as the map gesture's pan — and none of it as a tilt.
#[test]
fn a_pinch_that_moves_pans_as_it_zooms() {
    let frames: Vec<[Vec2; 2]> = (0..10)
        .map(|i| {
            let (half, x) = (50.0 + 3.0 * i as f32, 200.0 + 4.0 * i as f32);
            [Vec2::new(x - half, 300.0), Vec2::new(x + half, 300.0)]
        })
        .collect();
    let (drag, pinch, _, tilt) = locked(&frames);
    assert!((drag.x - 36.0).abs() < 1e-3, "drag = {drag}");
    assert!((pinch - 77.0 / 50.0).abs() < 1e-3, "pinch = {pinch}");
    assert_eq!(tilt, 0.0);
}

/// A pinch moved vertically is still a pinch: a slide up while spreading must
/// not tilt the view, which is what made a pinch impossible to aim.
#[test]
fn a_pinch_that_moves_up_does_not_tilt() {
    let frames: Vec<[Vec2; 2]> = (0..10)
        .map(|i| {
            let (half, y) = (50.0 + 3.0 * i as f32, 300.0 - 4.0 * i as f32);
            [Vec2::new(200.0 - half, y), Vec2::new(200.0 + half, y)]
        })
        .collect();
    let (drag, _, _, tilt) = locked(&frames);
    assert_eq!(tilt, 0.0);
    assert!((drag.y + 36.0).abs() < 1e-3, "drag = {drag}");
}

/// Side by side, sliding down together without spreading: a tilt, handed over
/// whole once it is told — including the frames spent deciding.
#[test]
fn fingers_side_by_side_sliding_together_tilt() {
    let frames: Vec<[Vec2; 2]> = (0..10)
        .map(|i| {
            let y = 300.0 + 4.0 * i as f32;
            [Vec2::new(150.0, y), Vec2::new(250.0, y)]
        })
        .collect();
    let (drag, pinch, twist, tilt) = locked(&frames);
    assert!((tilt - 36.0).abs() < 1e-3, "tilt = {tilt}");
    assert_eq!((drag, pinch, twist), (Vec2::ZERO, 1.0, 0.0));
}

/// The verdict holds for the pair: a tilt that starts spreading later is still
/// a tilt, and lifting a finger lets go of it.
#[test]
fn the_verdict_holds_until_the_pair_breaks_up() {
    let mut lock = PairLock::Off;
    let mut previous: Vec<(u64, Vec2)> = Vec::new();
    for i in 0..6 {
        let y = 300.0 + 4.0 * i as f32;
        let now = vec![(0, Vec2::new(150.0, y)), (1, Vec2::new(250.0, y))];
        lock_pair(&mut lock, &now, recognize(&previous, &now));
        previous = now;
    }
    assert!(matches!(lock, PairLock::Tilt { .. }), "{lock:?}");
    let spread = vec![(0, Vec2::new(130.0, 320.0)), (1, Vec2::new(270.0, 320.0))];
    let read = lock_pair(&mut lock, &spread, recognize(&previous, &spread));
    assert_eq!(read.pinch, 1.0);
    let one = vec![(0, Vec2::new(130.0, 320.0))];
    lock_pair(&mut lock, &one, recognize(&spread, &one));
    assert_eq!(lock, PairLock::Off);
}
