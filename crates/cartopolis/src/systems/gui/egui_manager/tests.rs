use super::*;
use bevy::input::mouse::MouseButtonInput;

/// A UI waiting on input is reused forever; a UI that said it animates is
/// reused *until its deadline* and then redrawn.
///
/// The second half is the one worth pinning. Testing `repaint_delay` for
/// `Duration::MAX` instead — which is what this did — reads a finite delay as
/// "redraw now", and since an animating widget re-requests every pass it draws,
/// the skip goes off permanently the moment anything on screen animates. Both
/// failure modes are silent: one freezes a clock, the other quietly tessellates
/// the whole UI sixty times a second.
#[test]
fn animating_ui_is_redrawn_on_its_deadline_and_not_before() {
    let forever = std::time::Duration::MAX;
    assert!(geometry_still_current(forever, 0.0));
    assert!(geometry_still_current(forever, 3_600.0));

    assert!(geometry_still_current(ANIMATING_REDRAW, 0.0));
    assert!(geometry_still_current(ANIMATING_REDRAW, 0.2));
    assert!(!geometry_still_current(ANIMATING_REDRAW, 0.3));
}

/// Pins that stood still on the frames nobody touched the screen.
///
/// A world-anchored overlay has to defeat the skip on the frames it moved and
/// on no others: the first is the bug, the second would tessellate the whole
/// UI sixty times a second for as long as one pin is on a still map.
#[test]
fn an_overlay_on_the_world_is_redrawn_when_the_world_moves_under_it() {
    let ctx = egui::Context::default();
    let pass = |at: Option<egui::Pos2>| {
        ctx.begin_pass(egui::RawInput::default());
        let mut anchored = WorldAnchored::new(&ctx, "test");
        if let Some(p) = at {
            anchored.at(p);
        }
        drop(anchored);
        ctx.end_pass_unpainted()
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map_or(std::time::Duration::MAX, |v| v.repaint_delay)
    };
    let still = |at| {
        for _ in 0..4 {
            pass(at);
        }
        pass(at)
    };
    let here = egui::pos2(100.0, 100.0);

    assert_eq!(
        still(Some(here)),
        std::time::Duration::MAX,
        "a still map keeps the skip"
    );
    assert!(
        !geometry_still_current(pass(Some(here + egui::vec2(0.5, 0.0))), 0.0),
        "half a point of camera movement is a new picture"
    );
    assert_eq!(still(Some(here)), std::time::Duration::MAX);
    assert!(
        !geometry_still_current(pass(None), 0.0),
        "a pin that went has to be drawn gone"
    );
}

use bevy::input::InputPlugin;
use bevy::window::{
    CursorMoved, ExitCondition, PrimaryWindow, WindowCloseRequested, WindowEvent, WindowPlugin,
};

fn moved(x: f32, y: f32) -> WindowEvent {
    WindowEvent::CursorMoved(CursorMoved {
        window: Entity::PLACEHOLDER,
        position: Vec2::new(x, y),
        delta: None,
    })
}

fn button(pressed: bool) -> WindowEvent {
    WindowEvent::MouseButtonInput(MouseButtonInput {
        button: MouseButton::Left,
        state: if pressed {
            ButtonState::Pressed
        } else {
            ButtonState::Released
        },
        window: Entity::PLACEHOLDER,
    })
}

fn touch(id: u64, phase: bevy::input::touch::TouchPhase, x: f32, y: f32) -> WindowEvent {
    WindowEvent::TouchInput(bevy::input::touch::TouchInput {
        phase,
        position: Vec2::new(x, y),
        window: Entity::PLACEHOLDER,
        force: None,
        id,
    })
}

/// Run a window-event stream through the bridge, as `begin_egui_frame` does.
fn bridge(stream: &[WindowEvent]) -> Vec<egui::Event> {
    bridge_scaled(stream, 1.0)
}

/// As [`bridge`], with an explicit UI scale.
fn bridge_scaled(stream: &[WindowEvent], ui_scale: f32) -> Vec<egui::Event> {
    let mut state = PointerBridge::default();
    let mut out = Vec::new();
    for ev in stream {
        pointer_events(ev, ui_scale, &mut state, &mut out);
    }
    out
}

/// The double tap that never opened the building card.
///
/// A finger's click reaches `ButtonInput` a frame after egui saw the finger
/// land, and a brisk tap has lifted by then. On that frame egui has no button
/// down and `egui_wants_pointer_input()` answers `true` over the bare map, so
/// the world gate threw the tap away. [`press_over_ui`] answers from where the
/// finger landed: the map is the world's, a panel is egui's.
#[test]
fn a_brisk_tap_on_the_map_is_the_worlds_after_egui_saw_it_lift() {
    use bevy::input::touch::TouchPhase;

    let ctx = egui::Context::default();
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0));
    let mut map_rect = None;
    let mut pass = |stream: &[WindowEvent]| {
        let mut raw = egui::RawInput::default();
        raw.screen_rect = Some(screen);
        raw.events = bridge(stream);
        ctx.begin_pass(raw);
        // As `begin_egui_frame` builds it, with one docked panel on the right.
        let mut root = egui::Ui::new(
            ctx.clone(),
            egui::Id::new((ctx.viewport_id(), "__cartopolis_root_ui")),
            egui::UiBuilder::new()
                .layer_id(egui::LayerId::background())
                .max_rect(ctx.viewport_rect()),
        );
        responsive_right_panel("panel", false, 1280.0, 260.0).show(&mut root, |ui| {
            ui.label("panel");
        });
        map_rect = Some(root.available_rect_before_wrap());
        drop(root);
        let _ = ctx.end_pass_unpainted();
    };

    let on_map = egui::pos2(300.0, 300.0);
    let on_panel = egui::pos2(1200.0, 300.0);
    pass(&[]);
    pass(&[touch(1, TouchPhase::Started, on_map.x, on_map.y)]);
    // The frame the emulated click lands on, with the lift in its batch.
    pass(&[touch(1, TouchPhase::Ended, on_map.x, on_map.y)]);

    assert!(
        ctx.egui_wants_pointer_input(),
        "the old gate: egui claims a lifted tap over the bare map"
    );
    assert!(
        !press_over_ui(&ctx, map_rect, on_map),
        "the map is the world's"
    );
    assert!(press_over_ui(&ctx, map_rect, on_panel), "a panel is egui's");
}

/// A finger that goes down and is never lifted — the Android camera intent
/// taking the activity away with a thumb still on the glass — must not deafen
/// the bridge to every later touch.
///
/// Without the heal this is permanent: `state.touch` holds the dead id,
/// `pointer_events` returns early for every finger that is not it, and touch
/// ids only increase, so egui never receives another pointer event for the
/// rest of the run. See [`heal_lost_touch`].
#[test]
fn a_touch_that_is_never_lifted_does_not_deafen_the_bridge() {
    use bevy::input::touch::TouchPhase;

    let mut state = PointerBridge::default();
    let mut out = Vec::new();
    pointer_events(
        &touch(7, TouchPhase::Started, 10.0, 20.0),
        1.0,
        &mut state,
        &mut out,
    );
    assert_eq!(state.touch, Some(7), "the first finger takes the pointer");

    // The lift never arrives — the app was suspended. On resume Bevy's own
    // set is released (`quiesce_while_backgrounded`), which is the only
    // signal that finger 7 is gone.
    out.clear();
    heal_lost_touch(&mut state, |_| false, &mut out);
    assert_eq!(state.touch, None, "the dead finger was not released");
    assert!(
        matches!(
            out.as_slice(),
            [
                egui::Event::PointerButton { pressed: false, .. },
                egui::Event::PointerGone
            ]
        ),
        "a healed lift must look exactly like a real one, got {out:?}"
    );

    // …and the next finger is heard.
    out.clear();
    pointer_events(
        &touch(8, TouchPhase::Started, 30.0, 40.0),
        1.0,
        &mut state,
        &mut out,
    );
    assert!(
        out.iter().any(|e| matches!(
            e,
            egui::Event::PointerButton {
                pressed: true,
                button: egui::PointerButton::Primary,
                ..
            }
        )),
        "the finger after a lost lift never reached egui, got {out:?}"
    );
}

/// The heal must not fire on a finger that is genuinely still down, or every
/// drag would be cut short one frame after it started.
#[test]
fn a_finger_still_on_the_glass_is_left_alone() {
    use bevy::input::touch::TouchPhase;

    let mut state = PointerBridge::default();
    let mut out = Vec::new();
    pointer_events(
        &touch(3, TouchPhase::Started, 10.0, 20.0),
        1.0,
        &mut state,
        &mut out,
    );

    out.clear();
    heal_lost_touch(&mut state, |id| id == 3, &mut out);
    assert_eq!(state.touch, Some(3), "a live finger was released");
    assert!(out.is_empty(), "a live finger produced events, got {out:?}");
}

/// The click-then-flick frame: `[move→target, press, release, move→far]`
/// arrives as one batch, and the buttons must be stamped with the position
/// *at their point in the stream* (the target), not the end-of-frame cursor
/// position (the far side of the flick). The old `ButtonInput` +
/// `cursor_position()` bridge got exactly this wrong, which is why panel
/// checkboxes only responded to a motionless click.
#[test]
fn buttons_carry_the_position_at_their_point_in_the_stream() {
    let target = egui::Pos2::new(100.0, 100.0);
    let stream = [
        moved(100.0, 100.0),
        button(true),
        button(false),
        moved(400.0, 300.0),
    ];
    let events = bridge(&stream);

    assert_eq!(events.len(), 4);
    match (&events[1], &events[2]) {
        (
            egui::Event::PointerButton {
                pos: p1,
                pressed: true,
                ..
            },
            egui::Event::PointerButton {
                pos: p2,
                pressed: false,
                ..
            },
        ) => {
            assert_eq!(*p1, target, "press teleported away from the pointer");
            assert_eq!(*p2, target, "release teleported away from the pointer");
        }
        other => panic!("expected press then release, got {other:?}"),
    }
    assert!(
        matches!(events[3], egui::Event::PointerMoved(p) if p.x == 400.0),
        "the flick motion must still be delivered, after the click"
    );
}

/// A finger down-move-up must produce the full pointer sequence, ending in
/// `PointerGone` — a finger leaves no cursor behind it, and egui otherwise
/// keeps hovering (and claiming) whatever it last touched.
#[test]
fn a_finger_synthesises_a_whole_pointer_sequence() {
    use bevy::input::touch::TouchPhase as P;
    let events = bridge(&[
        touch(7, P::Started, 100.0, 100.0),
        touch(7, P::Moved, 120.0, 140.0),
        touch(7, P::Ended, 120.0, 140.0),
    ]);
    // Every phase also goes out verbatim, for egui's own multi-touch readers.
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, egui::Event::Touch { .. }))
            .count(),
        3
    );
    let pointer: Vec<_> = events
        .iter()
        .filter(|e| !matches!(e, egui::Event::Touch { .. }))
        .collect();
    let down = egui::Pos2::new(100.0, 100.0);
    assert!(
        matches!(pointer[0], egui::Event::PointerMoved(p) if *p == down),
        "the press must be preceded by a move, or egui hit-tests it at the \
             stale position: {pointer:?}"
    );
    assert!(matches!(
        pointer[1],
        egui::Event::PointerButton { pressed: true, .. }
    ));
    assert!(matches!(pointer[2], egui::Event::PointerMoved(_)));
    assert!(matches!(
        pointer[3],
        egui::Event::PointerButton { pressed: false, .. }
    ));
    assert!(matches!(pointer[4], egui::Event::PointerGone));
    assert_eq!(pointer.len(), 5);
}

/// egui has one pointer. The second finger of a pinch must not drag the UI
/// out from under the first, nor report the pointer gone when it lifts.
#[test]
fn only_the_first_finger_drives_the_egui_pointer() {
    use bevy::input::touch::TouchPhase as P;
    let events = bridge(&[
        touch(1, P::Started, 100.0, 100.0),
        touch(2, P::Started, 300.0, 100.0),
        touch(2, P::Moved, 400.0, 100.0),
        touch(2, P::Ended, 400.0, 100.0),
    ]);
    let pointer: Vec<_> = events
        .iter()
        .filter(|e| !matches!(e, egui::Event::Touch { .. }))
        .collect();
    // Only finger 1's move+press.
    assert_eq!(pointer.len(), 2, "{pointer:?}");
    assert!(
        !pointer
            .iter()
            .any(|e| matches!(e, egui::Event::PointerGone)),
        "the second finger lifting must not take the pointer with it"
    );
}

/// A cancelled touch (the browser taking the gesture, a call arriving) must
/// release the pointer exactly as a lift does — otherwise egui is left with
/// a button held down forever and every later tap reads as a drag.
#[test]
fn a_cancelled_touch_releases_the_pointer() {
    use bevy::input::touch::TouchPhase as P;
    let events = bridge(&[
        touch(1, P::Started, 100.0, 100.0),
        touch(1, P::Canceled, 100.0, 100.0),
        // The pointer is free again, so a fresh finger takes it.
        touch(2, P::Started, 200.0, 200.0),
    ]);
    let pressed: Vec<bool> = events
        .iter()
        .filter_map(|e| match e {
            egui::Event::PointerButton { pressed, .. } => Some(*pressed),
            _ => None,
        })
        .collect();
    assert_eq!(pressed, vec![true, false, true]);
}

/// End-to-end: a click-then-flick arriving as ONE frame's batch must still
/// toggle a checkbox, via the pass-splitting the live bridge applies
/// ([`take_pass_events`]). Without the split this exact scenario fails:
/// egui hit-tests once per pass at the batch's final pointer position, so
/// the click lands wherever the flick ended.
#[test]
fn click_then_flick_in_one_frame_still_clicks() {
    let ctx = egui::Context::default();
    crate::systems::theme::install(&ctx);
    let mut checked = true;
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0));

    let mut target: Option<egui::Pos2> = None;
    let mut pending: Vec<egui::Event> = Vec::new();
    for frame in 0..6 {
        let mut raw = egui::RawInput::default();
        raw.screen_rect = Some(screen);
        if let (1, Some(pos)) = (frame, target) {
            // One frame's batch: arrive at the checkbox, click, flick away.
            pending = bridge(&[
                moved(pos.x, pos.y),
                button(true),
                button(false),
                moved(pos.x + 300.0, pos.y + 200.0),
            ]);
        }
        raw.events = take_pass_events(&mut pending);
        let rect = std::cell::Cell::new(None);
        let checked_ref = &mut checked;
        // The pass output is the paint job; this test only reads widget state.
        let _ = ctx.run_ui_unpainted(raw, |ui| {
            egui::Panel::right("panel")
                .default_size(260.0)
                .show(ui, |ui| {
                    let resp = ui.checkbox(checked_ref, "a checkbox");
                    rect.set(Some(resp.rect));
                });
        });
        if target.is_none() {
            target = rect.get().map(|r| r.center());
        }
    }
    assert!(pending.is_empty(), "the carry never drained");
    assert!(!checked, "the click-then-flick frame lost the click");
}

/// Drive a checkbox through the **app's own** frame shape: the shared root
/// `Ui` that [`begin_egui_frame`] builds by hand, not the one `run_ui` hands
/// out. Returns whether the click landed.
///
/// `overlay` gets the map area (the root `Ui`'s leftover space, exactly what
/// `place_pins` culls against) after the panel has taken its width, so a test
/// can put a floating `Area` there and ask whether it eats the panel's clicks.
fn click_checkbox_through_root_ui(overlay: impl Fn(&egui::Context, egui::Rect)) -> bool {
    click_checkbox_with_drift(overlay, 0.0)
}

/// As above, but the pointer drifts `drift` points between press and release —
/// what a real hand does. The events interleave as
/// `[moved, press]` … `[moved, release]`, which is the shape the live bridge
/// hands egui.
fn click_checkbox_with_drift(overlay: impl Fn(&egui::Context, egui::Rect), drift: f32) -> bool {
    let ctx = egui::Context::default();
    crate::systems::theme::install(&ctx);
    let mut checked = false;
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0));

    // The panel slides open (`show_collapsible` animates), so a click aimed at
    // frame 0's rect would land on a moving target and prove nothing. Wait for
    // the checkbox to hold the same rect two passes running, then click that.
    let mut seen: Option<egui::Rect> = None;
    let mut settled: Option<egui::Pos2> = None;
    let mut clicked_at: Option<usize> = None;
    let mut pending: Vec<egui::Event> = Vec::new();
    for frame in 0..24 {
        let mut raw = egui::RawInput::default();
        raw.screen_rect = Some(screen);
        if let (None, Some(pos)) = (clicked_at, settled) {
            pending = bridge(&[
                moved(pos.x, pos.y),
                button(true),
                moved(pos.x + drift, pos.y),
                button(false),
            ]);
            clicked_at = Some(frame);
        }
        raw.events = take_pass_events(&mut pending);

        ctx.begin_pass(raw);
        // Verbatim from `begin_egui_frame` — the thing under test.
        let mut root = egui::Ui::new(
            ctx.clone(),
            egui::Id::new((ctx.viewport_id(), "__cartopolis_root_ui")),
            egui::UiBuilder::new()
                .layer_id(egui::LayerId::background())
                .max_rect(ctx.viewport_rect()),
        );
        let mut open = true;
        let rect = std::cell::Cell::new(None);
        let checked_ref = &mut checked;
        responsive_right_panel("layers_panel", false, 1280.0, 260.0).show_collapsible(
            &mut root,
            &mut open,
            |ui| {
                let resp = crate::systems::theme::checkbox(ui, checked_ref, "Buildings");
                rect.set(Some(resp.rect));
            },
        );
        overlay(&ctx, root.available_rect_before_wrap());
        drop(root);
        // The pass output is the paint job; this test only reads widget state.
        let _ = ctx.end_pass_unpainted();

        if settled.is_none() {
            if let Some(r) = rect.get() {
                if seen == Some(r) {
                    settled = Some(r.center());
                }
                seen = Some(r);
            }
        }
    }
    assert!(settled.is_some(), "the checkbox never settled into a rect");
    assert!(
        clicked_at.is_some_and(|f| f + 3 < 24),
        "the click was issued too late to be resolved"
    );
    checked
}

/// A checkbox in a right panel must respond to a click when nothing is over
/// it. This is the app's real frame shape, which `run_ui` does not reproduce.
#[test]
fn panel_checkbox_clicks_through_the_shared_root_ui() {
    assert!(
        click_checkbox_through_root_ui(|_, _| {}),
        "a panel checkbox in the shared root `Ui` ignored a clean click"
    );
}

/// A hand does not hold a mouse still. Sub-pixel drift between press and
/// release — measured at 0.67 and 0.89 pt in a live session, far inside egui's
/// 6 pt `max_click_dist` — must not lose the click. In that session the only
/// press that toggled anything was the one that drifted exactly 0.0 pt.
#[test]
fn a_drifting_hand_still_clicks() {
    for drift in [0.0, 0.5, 0.89, 2.0, 5.0] {
        assert!(
            click_checkbox_with_drift(|_, _| {}, drift),
            "{drift} pt of drift between press and release lost the click"
        );
    }
}

/// A floating `Area` covering the **map** must not eat clicks aimed at the
/// panel beside it. Panels live on the background layer and every `Area`
/// paints above them, so an overlay that fails to cull itself to the map area
/// steals the panel's input — the whole reason `place_pins` culls rather than
/// clips (see `docs/notes/egui-shared-root-ui.md`). This pins the contract
/// from the panel's side.
#[test]
fn a_map_overlay_does_not_eat_panel_clicks() {
    assert!(
        click_checkbox_through_root_ui(|ctx, map_rect| {
            egui::Area::new(egui::Id::new("test_map_overlay"))
                .fixed_pos(map_rect.left_top())
                .show(ctx, |ui| {
                    ui.allocate_response(map_rect.size(), egui::Sense::click());
                });
        }),
        "an overlay confined to the map area ate a click meant for the panel"
    );
}

/// A UI-bearing app with no render world (`EguiPlugin` skips its render-app
/// half when there is none, which is exactly what a test wants).
fn ui_app(primary_window: Option<Window>) -> App {
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        InputPlugin,
        WindowPlugin {
            primary_window,
            exit_condition: ExitCondition::OnAllClosed,
            close_when_requested: true,
            ..default()
        },
        EguiPlugin,
    ))
    .add_systems(Update, touch_root_ui);
    app
}

/// The first thing every panel system does.
fn touch_root_ui(mut egui: EguiMut) {
    let _ = egui.ui_mut();
}

/// The macOS shutdown crash (#2): Bevy despawns the window in `Last`, so the
/// frames between the close and the process actually exiting run the whole
/// `Update` schedule with no `Window` in the world. `begin_egui_frame` used to
/// bail out there, leaving every panel system to panic on `ui_mut()`.
#[test]
fn update_survives_the_window_being_closed() {
    let mut app = ui_app(Some(Window::default()));
    app.update();

    let window = app
        .world_mut()
        .query_filtered::<Entity, With<PrimaryWindow>>()
        .single(app.world())
        .expect("primary window");
    app.world_mut()
        .write_message(WindowCloseRequested { window });

    // `close_when_requested` marks on one frame and despawns on the next, so
    // give it both — then run the frames that used to panic.
    for _ in 0..4 {
        app.update();
    }
    assert!(
        app.world_mut()
            .query_filtered::<Entity, With<Window>>()
            .iter(app.world())
            .next()
            .is_none(),
        "the window should be gone — otherwise this test never reaches the case it is about"
    );
}

/// The same invariant from the other end: a windowless run whose
/// `HeadlessSurface` is missing — what a `--shot` capture looks like if the
/// harness fails to insert one. The UI has nothing to lay out against, which
/// is not a reason to take the process down.
#[test]
fn update_survives_a_run_with_no_surface_at_all() {
    let mut app = ui_app(None);
    app.update();
    app.update();
}

/// A zero/NaN scale factor divides every viewport size, so it is sanitised on
/// the way in rather than propagated as infinities through panel layout.
#[test]
fn a_degenerate_scale_factor_never_reaches_layout() {
    let mut mgr = EguiManager::default();
    for bad in [0.0, -1.0, f32::NAN] {
        mgr.set_surface(UVec2::new(1280, 720), bad);
        assert_eq!(mgr.viewport_pts(), egui::vec2(1280.0, 720.0));
    }
    mgr.set_surface(UVec2::new(2560, 1440), 2.0);
    assert_eq!(mgr.viewport_pts(), egui::vec2(1280.0, 720.0));
}

/// The UI scale multiplies the widgets by dividing the viewport: the same
/// screen holds fewer, larger points. Everything responsive keys off
/// `viewport_pts`, so a phone that was merely narrow becomes narrow *and*
/// compact from this one number.
#[test]
fn ui_scale_shrinks_the_viewport_it_magnifies() {
    let mut mgr = EguiManager::default();
    mgr.ui_scale = 1.5;
    // Android, where a phone's whole 1080 px width reaches Bevy: 393 pt of
    // viewport at a 2.75 scale factor, 262 pt of it after the magnification.
    mgr.set_surface(UVec2::new(1080, 2280), 2.75);
    assert!((mgr.viewport_pts().x - 261.8).abs() < 0.1);
    assert!(mgr.compact());
}

/// A compact panel never exceeds the screen it docks in — which the old
/// `viewport_w.max(240.0)` floor did on every web phone tier viewport under
/// 240 pt, i.e. on the one device class the compact layout exists for.
#[test]
fn a_compact_panel_is_exactly_the_viewport() {
    assert_eq!(compact_panel_width(214.0), 214.0);
    assert_eq!(compact_panel_width(390.0), 390.0);
    // Only a viewport that is not a width falls back.
    assert_eq!(compact_panel_width(0.0), 240.0);
    assert_eq!(compact_panel_width(f32::NAN), 240.0);
}

/// Why [`phone_ui_scale`] is 1.0 on the web: the render budget's stage
/// transform has already shrunk the surface Bevy sees, so magnifying again
/// would halve an already-tight viewport (it was also, until the fix above,
/// how a docked panel came out wider than the screen).
///
/// Same handset as above, but through the phone tier's k ≈ 1.83: winit
/// measures the *untransformed* box, so Bevy's surface is 1080/1.83 px.
#[test]
fn the_web_phone_tier_has_no_room_to_magnify() {
    let surface = UVec2::new(590, 1245); // 1080×2280 ÷ 1.83
    let mut unscaled = EguiManager::default();
    unscaled.ui_scale = 1.0;
    unscaled.set_surface(surface, 2.75);
    assert!(unscaled.viewport_pts().x > 200.0, "usable at 1.0");

    let mut scaled = EguiManager::default();
    scaled.ui_scale = 1.5;
    scaled.set_surface(surface, 2.75);
    assert!(
        scaled.viewport_pts().x < 240.0,
        "1.5× would dock a 240 pt panel in a {} pt viewport",
        scaled.viewport_pts().x
    );
}

/// Positions enter in the window's logical points and must leave in egui's,
/// or every widget is hit-tested at a position that drifts further from the
/// finger the further it is from the top-left corner. Converted at the
/// bridge so `state.last` — and the button events built from it — are
/// already in egui's space.
#[test]
fn the_pointer_bridge_converts_into_egui_points() {
    let events = bridge_scaled(&[moved(300.0, 600.0), button(true)], 1.5);
    let positions: Vec<egui::Pos2> = events
        .iter()
        .filter_map(|e| match e {
            egui::Event::PointerMoved(p) => Some(*p),
            egui::Event::PointerButton { pos, .. } => Some(*pos),
            _ => None,
        })
        .collect();
    assert_eq!(positions, vec![egui::pos2(200.0, 400.0); 2]);
}

/// The same conversion, for the world-anchored overlays that get their
/// positions from `Camera::world_to_viewport` rather than from an event.
#[test]
fn window_to_ui_matches_the_bridge() {
    let mut mgr = EguiManager::default();
    mgr.ui_scale = 1.5;
    assert_eq!(
        mgr.window_to_ui(Vec2::new(300.0, 600.0)),
        egui::pos2(200.0, 400.0)
    );
}

/// The seam the on-screen keyboard is built on: an event queued during
/// `Update` must turn up in the **next** frame's `RawInput`, and the queue
/// must be empty afterwards.
///
/// It cannot be the same frame — `begin_egui_frame` has already assembled
/// `RawInput` and begun the pass by the time any UI system runs — and a queue
/// that were not drained would replay the last character typed on every frame
/// forever, which is the failure this pins.
#[test]
fn injected_events_reach_the_next_frames_input() {
    let mut app = ui_app(Some(Window::default()));
    app.update();
    app.world_mut()
        .resource_mut::<EguiManager>()
        .inject_events([egui::Event::Text("q".to_owned())]);

    let before = app
        .world()
        .resource::<EguiManager>()
        .ctx
        .input(|i| i.events.len());
    app.update();

    let mgr = app.world().resource::<EguiManager>();
    assert!(
        mgr.injected.is_empty(),
        "the injection queue was not drained"
    );
    assert!(
        mgr.ctx.input(|i| i
            .events
            .iter()
            .any(|e| matches!(e, egui::Event::Text(t) if t == "q"))),
        "the queued character never reached egui (had {before} events before)"
    );

    // And exactly once: a second frame with nothing queued must be quiet.
    app.update();
    let mgr = app.world().resource::<EguiManager>();
    assert!(
        !mgr.ctx
            .input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Text(_)))),
        "the injected event was replayed on a later frame"
    );
}

/// The display scale and the UI magnification have to come apart again:
/// `set_surface` multiplies them into one number, and anything reasoning
/// about *physical* size (`onscreen_keyboard`'s 48 dp touch target) needs the
/// density on its own.
#[test]
fn display_scale_survives_being_folded_into_the_surface() {
    let mut mgr = EguiManager::default();
    mgr.ui_scale = 1.5;
    mgr.set_surface(UVec2::new(1080, 2280), 2.75);
    assert!((mgr.display_scale() - 2.75).abs() < 1e-4);
}

/// An out-of-range scale is clamped rather than obeyed: at 20× there is no
/// longer a button on screen to press to undo it.
#[test]
fn an_absurd_ui_scale_is_clamped() {
    for (raw, want) in [(20.0_f32, 3.0_f32), (0.01, 0.5), (1.4, 1.4)] {
        let clamped = raw.clamp(*UI_SCALE_RANGE.start(), *UI_SCALE_RANGE.end());
        assert_eq!(clamped, want, "clamping {raw}");
    }
}

/// Every system that draws into the **shared root `Ui`** must be in the panel
/// chain in `systems/gui/plugin.rs`, and this test is the guard the chain's comment used to
/// say did not exist.
///
/// Leaving one out does not misplace a panel — it makes two panels swap
/// composition order between frames, and because they share egui's single
/// `Background` layer the one that lands underneath stops receiving clicks on
/// those frames. The symptom is a control that ignores about half its
/// presses; `layer_id_at` still answers `Background`, so no layer-level
/// diagnostic sees it, and it cannot reproduce in a single-panel test.
///
/// This reads the sources rather than the schedule because the invariant is
/// "draws into the root `Ui`" — a property of a function body, which the type
/// system cannot express and a running `App` cannot be asked about.
#[test]
fn every_root_ui_panel_is_in_the_chain() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    let mut stack = vec![src.join("systems")];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir)
            .expect("systems/ is readable")
            .flatten()
        {
            let p = e.path();
            // A module's tests (`foo/tests.rs`) draw panels to test them,
            // and their functions sit at the top level there, where this
            // scan looks for systems.
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") && !p.ends_with("tests.rs") {
                files.push(p);
            }
        }
    }

    // The frame lifecycle brackets the panels by *schedule* (PreUpdate /
    // Update / PostUpdate + the render graph), not by ordering links, so it is
    // deliberately not in the chain.
    const LIFECYCLE: [&str; 3] = ["begin_egui_frame", "end_egui_frame", "egui_pass"];

    let mut drawers = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).expect("source is readable");
        // Split on top-level `fn ` starts; a body owns everything up to the next one.
        let mut idx: Vec<usize> = text
            .match_indices("\nfn ")
            .chain(text.match_indices("\npub fn "))
            .map(|(i, _)| i)
            .collect();
        idx.sort_unstable();
        idx.push(text.len());
        for w in idx.windows(2) {
            let chunk = &text[w[0]..w[1]];
            // A *system* that draws into the root `Ui` takes the manager
            // mutably; helpers like `responsive_right_panel` take plain
            // arguments and only build a widget, so they order nothing.
            // A call to `egui_manager::right_panel(` is the same act one
            // level up — the helper calls `ui_mut()` on the system's behalf,
            // so a panel using it still composes into the shared layer and
            // still needs its place in the chain.
            let draws_root_ui =
                chunk.contains("ui_mut()") || chunk.contains("egui_manager::right_panel(");
            if !draws_root_ui || !chunk.contains("EguiMut") {
                continue;
            }
            let name = chunk
                .trim_start_matches(['\n'])
                .trim_start_matches("pub ")
                .trim_start_matches("fn ")
                .split(['(', '<'])
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            if !name.is_empty() && !LIFECYCLE.contains(&name.as_str()) {
                drawers.push(name);
            }
        }
    }
    assert!(
        drawers.len() >= 5,
        "found only {drawers:?} root-`Ui` drawers — the scan has broken, not the chain"
    );

    // The chain is two chained halves (one per `ClientPhase`) chained to each
    // other, so it is found by its markers rather than by its first `.chain()`.
    let plugin = std::fs::read_to_string(src.join("systems/gui/plugin.rs"))
        .expect("systems/gui/plugin.rs is readable");
    let chain = plugin
        .split_once("// panel-chain:begin")
        .and_then(|(_, rest)| rest.split_once("// panel-chain:end"))
        .map(|(block, _)| block.to_string())
        .expect("the panel chain still exists in systems/gui/plugin.rs");
    assert!(
        chain.contains("render_main_menu") && chain.contains("render_place_pins"),
        "the chain block was extracted but looks wrong — the markers in \
             systems/gui/plugin.rs moved"
    );

    let missing: Vec<_> = drawers
        .iter()
        .filter(|d| !chain.contains(d.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "these draw into the shared root `Ui` but are not in the panel chain: \
             {missing:?}\nAdd them to that tuple — an unordered pair swaps composition \
             order between frames and the panel underneath stops taking clicks."
    );
}
