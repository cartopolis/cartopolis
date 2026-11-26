use super::*;

/// The phone this was sized for: 274 × 609 UI points.
fn phone() -> Rect {
    Rect::new(0.0, 0.0, 274.0, 609.0)
}

/// Up, with nothing focused yet: a keyboard as a field's focus leaves it.
fn kb() -> OnScreenKeyboard {
    let mut k = OnScreenKeyboard::new(true);
    k.follow_focus(true);
    k
}

/// A keyboard up and placed on the phone, at the bottom, edge decided.
fn placed() -> OnScreenKeyboard {
    let mut k = kb();
    for _ in 0..SETTLE_FRAMES {
        k.place(phone(), 48.0);
    }
    k
}

fn a() -> Cap {
    Cap::Char {
        base: 'a',
        alt: 'A',
    }
}

/// Where the key carrying `cap` is on the slab placed last.
fn key_pos(k: &OnScreenKeyboard, want: Cap) -> Vec2 {
    let slab = k.slab.expect("the keyboard is not on screen");
    key_rects(&slab, k.layer)
        .into_iter()
        .find(|(_, _, cap, _)| *cap == want)
        .map(|(_, _, _, rect)| rect.center())
        .unwrap_or_else(|| panic!("no {want:?} key on the {:?} layer", k.layer))
}

fn finger(id: u64, phase: FingerPhase, pos: Vec2) -> FingerEvent {
    FingerEvent { id, phase, pos }
}

/// A finger down on `cap` on one frame and up on the next.
fn tap(k: &mut OnScreenKeyboard, cap: Cap, id: u64, now: f64) {
    let pos = key_pos(k, cap);
    k.track(&[finger(id, FingerPhase::Start, pos)], now);
    k.track(&[finger(id, FingerPhase::End, pos)], now + 0.05);
}

#[test]
fn a_tap_types_the_key_as_the_finger_lifts() {
    let mut k = placed();
    let pos = key_pos(&k, a());
    k.track(&[finger(1, FingerPhase::Start, pos)], 0.0);
    assert!(k.take_strokes().is_empty(), "typed on the way down");
    assert_eq!(k.held().len(), 1, "the key under the finger is not pressed");
    k.track(&[finger(1, FingerPhase::End, pos)], 0.1);
    assert_eq!(k.take_strokes(), vec![Stroke::Text("a".into())]);
    assert!(k.held().is_empty());
}

/// Two thumbs: the second lands before the first lifts. A key that typed on
/// a click (one pointer) lost the second letter every time.
#[test]
fn overlapping_thumbs_type_both_keys() {
    use FingerPhase::{End, Start};
    let mut k = placed();
    let q = key_pos(
        &k,
        Cap::Char {
            base: 'q',
            alt: 'Q',
        },
    );
    let p = key_pos(
        &k,
        Cap::Char {
            base: 'p',
            alt: 'P',
        },
    );
    k.track(&[finger(50, Start, q)], 0.0);
    k.track(&[finger(51, Start, p)], 0.02);
    assert_eq!(k.previews().len(), 2, "one preview per thumb");
    k.track(&[finger(50, End, q)], 0.04);
    k.track(&[finger(51, End, p)], 0.06);
    let typed: String = k
        .take_strokes()
        .into_iter()
        .map(|s| match s {
            Stroke::Text(t) => t,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(typed, "qp");
}

/// A finger that rolls a few points before lifting is a drag to a pointer,
/// and still a keystroke here: the key under it as it lifts.
#[test]
fn a_drifting_finger_types_the_key_it_lifts_from() {
    use FingerPhase::{End, Move, Start};
    let mut k = placed();
    let at = key_pos(&k, a());
    let drift = at + Vec2::new(3.0, 9.0);
    k.track(&[finger(60, Start, at)], 0.0);
    k.track(&[finger(60, Move, drift)], 0.02);
    k.track(&[finger(60, End, drift)], 0.04);
    assert_eq!(k.take_strokes(), vec![Stroke::Text("a".into())]);
}

/// Lifted off the slab altogether, the finger changed its mind.
#[test]
fn a_finger_lifted_off_the_keys_types_nothing() {
    use FingerPhase::{End, Move, Start};
    let mut k = placed();
    let at = key_pos(&k, a());
    let off = Vec2::new(at.x, 20.0);
    k.track(&[finger(7, Start, at)], 0.0);
    k.track(&[finger(7, Move, off)], 0.02);
    k.track(&[finger(7, End, off)], 0.04);
    assert!(k.take_strokes().is_empty());
}

/// Backspace types as the finger lands, then — held — repeats, and stops
/// as it lifts.
#[test]
fn a_held_backspace_repeats() {
    use FingerPhase::{End, Start};
    let mut k = placed();
    let bs = key_pos(&k, Cap::Backspace);
    k.track(&[finger(9, Start, bs)], 0.0);
    assert_eq!(k.take_strokes(), vec![Stroke::Backspace], "not on landing");
    k.track(&[], REPEAT_AFTER_S * 0.5);
    assert!(k.take_strokes().is_empty(), "repeated before the delay");
    // A second of holding, sampled every 10 ms: one repeat per interval,
    // give or take the sampling.
    let mut t = REPEAT_AFTER_S;
    let mut repeats = 0;
    while t < REPEAT_AFTER_S + 1.0 {
        k.track(&[], t);
        repeats += k.take_strokes().len();
        t += 0.01;
    }
    let want = (1.0 / REPEAT_EVERY_S) as usize;
    assert!(
        (want - 3..=want + 1).contains(&repeats),
        "{repeats} repeats in a second, want about {want}"
    );
    k.track(&[finger(9, End, bs)], t);
    k.track(&[], t + 1.0);
    assert!(
        k.take_strokes().is_empty(),
        "the lift typed or kept repeating"
    );
}

/// The preview sits above the key, shows the character in the shift state,
/// and stays on the slab at the outer columns.
#[test]
fn the_key_preview_shows_the_character_above_the_key() {
    let mut k = placed();
    k.shift = ShiftState::Once;
    let q = Cap::Char {
        base: 'q',
        alt: 'Q',
    };
    let at = key_pos(&k, q);
    k.track(&[finger(3, FingerPhase::Start, at)], 0.0);
    let previews = k.previews();
    assert_eq!(previews.len(), 1);
    let p = previews[0];
    assert_eq!(p.ch, 'Q');
    assert!(p.rect.max.y < at.y, "the preview is not above its key");
    let slab = k.slab().unwrap();
    assert!(p.rect.min.x >= slab.min.x, "the preview left the slab");
    // No preview for a key that is not a character.
    k.track(&[finger(3, FingerPhase::Cancel, at)], 0.1);
    let space = key_pos(&k, Cap::Space);
    k.track(&[finger(4, FingerPhase::Start, space)], 0.2);
    assert!(k.previews().is_empty());
}

/// A click with a finger behind it is that finger's, already typed; one with
/// none (the MCP server's `press`) types.
#[test]
fn a_click_types_only_without_a_finger() {
    let mut k = placed();
    tap(&mut k, a(), 1, 0.0);
    assert_eq!(k.take_strokes().len(), 1);
    // The tap's own click, on the next frame.
    k.clicked(1, 0, 0.1);
    assert!(k.take_strokes().is_empty(), "the tap typed twice");
    // Row 1, column 0 is `a`.
    k.clicked(1, 0, 5.0);
    assert_eq!(k.take_strokes(), vec![Stroke::Text("a".into())]);
}

/// The hide key puts the keys away and the field's focus with it, and they
/// stay down while that focus is being cleared; a fresh focus brings them
/// back — the reopened keys type.
#[test]
fn the_keys_type_again_after_being_hidden_and_reopened() {
    let mut k = placed();
    tap(&mut k, a(), 1, 0.0);
    assert_eq!(k.take_strokes().len(), 1);
    tap(&mut k, Cap::Hide, 2, 0.2);
    assert!(!k.is_open(), "the hide key did not close the keyboard");
    assert!(k.take_blur(), "the field keeps its focus");
    assert!(k.take_strokes().is_empty());
    // The focus is still the field's for a frame: the keys stay down.
    k.follow_focus(true);
    assert!(!k.is_open(), "the keys came straight back");
    // The focus left, then the field was tapped again.
    k.follow_focus(false);
    k.follow_focus(true);
    k.place(phone(), 48.0);
    assert!(k.is_open(), "the keyboard did not come back");
    tap(&mut k, a(), 3, 2.0);
    assert_eq!(k.take_strokes(), vec![Stroke::Text("a".into())]);
}

/// Back (`dismiss`) is the hide key from outside.
#[test]
fn dismiss_closes_and_blurs() {
    let mut k = placed();
    k.dismiss();
    assert!(!k.is_open());
    assert!(k.take_blur());
    assert!(k.slab().is_none());
}

/// Shift is the whole reason this is a state machine: one tap arms it for a
/// single character, a second locks it, a third clears it — and typing a
/// character consumes the armed state but not the locked one.
#[test]
fn shift_arms_then_locks_then_clears() {
    let mut k = kb();
    assert_eq!(k.shift, ShiftState::Off);
    k.press(Cap::Shift);
    assert_eq!(k.shift, ShiftState::Once);
    k.press(Cap::Shift);
    assert_eq!(k.shift, ShiftState::Lock);
    k.press(Cap::Shift);
    assert_eq!(k.shift, ShiftState::Off);
}

#[test]
fn a_one_shot_shift_survives_exactly_one_character() {
    let mut k = kb();
    k.press(Cap::Shift);
    assert_eq!(k.press(a()), vec![Stroke::Text("A".to_owned())]);
    assert_eq!(k.shift, ShiftState::Off, "one-shot shift did not release");
    assert_eq!(k.press(a()), vec![Stroke::Text("a".to_owned())]);
}

#[test]
fn caps_lock_survives_every_character() {
    let mut k = kb();
    k.press(Cap::Shift);
    k.press(Cap::Shift);
    for _ in 0..3 {
        assert_eq!(k.press(a()), vec![Stroke::Text("A".to_owned())]);
    }
    assert_eq!(k.shift, ShiftState::Lock);
}

/// The layer key swaps both ways and does not disturb shift — a lock set to
/// type an acronym must still be a lock after reaching for a digit in it.
#[test]
fn the_layer_key_toggles_and_leaves_shift_alone() {
    let mut k = kb();
    k.press(Cap::Shift);
    k.press(Cap::Shift);
    k.press(Cap::Layer);
    assert_eq!(k.layer, KeyLayer::Symbols);
    assert_eq!(k.shift, ShiftState::Lock);
    k.press(Cap::Layer);
    assert_eq!(k.layer, KeyLayer::Letters);
}

/// Shift on the symbol layer is not a case mapping: the alternate of `4` is
/// `$`. This is why a key carries both forms.
#[test]
fn shift_on_the_symbol_layer_picks_the_alternate_not_the_uppercase() {
    let rows = rows(KeyLayer::Symbols);
    let four = rows[0][3];
    assert_eq!(
        four.cap,
        Cap::Char {
            base: '4',
            alt: '$'
        }
    );
    let mut k = kb();
    k.layer = KeyLayer::Symbols;
    k.shift = ShiftState::Once;
    assert_eq!(k.press(four.cap), vec![Stroke::Text("$".to_owned())]);
}

#[test]
fn hide_closes_and_types_nothing() {
    let mut k = kb();
    assert!(k.press(Cap::Hide).is_empty());
    assert!(!k.is_open());
}

/// The editing keys are strokes of their own, never text.
#[test]
fn the_editing_keys_are_their_strokes() {
    let mut k = kb();
    assert_eq!(k.press(Cap::Backspace), vec![Stroke::Backspace]);
    assert_eq!(k.press(Cap::Enter), vec![Stroke::Enter]);
    assert_eq!(k.press(Cap::Left), vec![Stroke::Left { select: false }]);
    assert_eq!(k.press(Cap::Right), vec![Stroke::Right { select: false }]);
    assert_eq!(k.press(Cap::SelectAll), vec![Stroke::SelectAll]);
    // Select-all is a state of the field, not an edit: it disturbs nothing.
    assert_eq!(k.shift, ShiftState::Off);
}

/// Shift turns the arrows into a selection, and — unlike a character — an
/// arrow does **not** consume a one-shot shift.
#[test]
fn shift_makes_the_arrows_extend_a_selection_without_consuming_itself() {
    let mut k = kb();
    k.press(Cap::Shift);
    for _ in 0..3 {
        assert_eq!(k.press(Cap::Right), vec![Stroke::Right { select: true }]);
        assert_eq!(k.shift, ShiftState::Once, "shift fell off mid-selection");
    }
    k.press(Cap::Shift);
    k.press(Cap::Shift);
    assert_eq!(k.press(Cap::Right), vec![Stroke::Right { select: false }]);
}

#[test]
fn space_is_text() {
    let mut k = kb();
    assert_eq!(k.press(Cap::Space), vec![Stroke::Text(" ".to_owned())]);
}

/// The keyboard has to fit the phone, keep its 48 dp of key height, and
/// leave most of the screen alone.
#[test]
fn the_keyboard_fits_a_phone_viewport() {
    let vp = phone();
    let min_key_h =
        theme::min_touch_target_pts(theme::dp_per_point(1.5, 2.75, false)).max(MIN_KEY_HEIGHT_PTS);
    let slab = layout(vp, min_key_h, Edge::Bottom);

    assert!(vp.contains(slab.rect.min) && vp.contains(slab.rect.max));
    assert!(
        (slab.key_h - min_key_h).abs() < 1e-3,
        "the 48 dp key height was clamped away: {}",
        slab.key_h
    );
    assert!(
        slab.rect.height() < vp.height() * MAX_HEIGHT_FRACTION,
        "the keyboard took {} pt of a {} pt screen",
        slab.rect.height(),
        vp.height()
    );
    // Docked to the bottom.
    assert!(slab.rect.max.y > vp.max.y - 2.0 * EDGE_MARGIN);
}

/// No row exceeds [`COLUMNS`] units, in either layer, and the two layers
/// have the same key slots, so a layer swap leaves every key where the finger
/// last found it.
#[test]
fn every_row_spans_the_full_width() {
    for layer in [KeyLayer::Letters, KeyLayer::Symbols] {
        for (i, row) in rows(layer).iter().enumerate() {
            let used: f32 = row.iter().map(|k| k.units).sum();
            assert!(
                used <= COLUMNS + 1e-3,
                "{layer:?} row {i} is {used} units wide"
            );
        }
    }
    let letters = rows(KeyLayer::Letters);
    let symbols = rows(KeyLayer::Symbols);
    let shape = |rs: &[Vec<Key>]| rs.iter().map(Vec::len).collect::<Vec<_>>();
    assert_eq!(
        shape(&letters),
        shape(&symbols),
        "the layers must have the same key slots, or swapping moves every key"
    );
    assert_eq!(letters.len() as f32, ROW_COUNT);
}

/// Every key rect stays inside the slab: a key hanging over the edge would
/// take presses the world thinks are its own.
#[test]
fn every_key_stays_inside_the_slab() {
    let slab = layout(phone(), 48.0, Edge::Bottom);
    for (r, c, cap, rect) in key_rects(&slab, KeyLayer::Letters) {
        assert!(
            slab.rect.contains(rect.min) && slab.rect.contains(rect.max),
            "{cap:?} at {r},{c} escaped the slab"
        );
        assert!(rect.width() > 0.0 && rect.height() > 0.0);
    }
}

/// The one layout rule that is not cosmetic: a keyboard over the field you
/// are typing into is useless. When the field is low on the screen the slab
/// moves to the top instead.
#[test]
fn the_keyboard_moves_off_the_field_it_is_serving() {
    let vp = phone();
    let high = Rect::new(10.0, 60.0, 260.0, 84.0);
    let low = Rect::new(10.0, 540.0, 260.0, 564.0);

    assert_eq!(choose_edge(vp, 48.0, Some(high)), Edge::Bottom);
    assert_eq!(choose_edge(vp, 48.0, None), Edge::Bottom);
    let edge = choose_edge(vp, 48.0, Some(low));
    assert_eq!(edge, Edge::Top, "it should flip to the top");
    let slab = layout(vp, 48.0, edge).rect;
    assert!(slab.intersect(low).is_empty(), "the keys cover their field");
}

/// The edge is decided once the screens have had their frames to lift, from
/// where the field is then, and held while that field has the focus: a field
/// that lifted clear keeps the keys at the bottom, one pinned under them
/// flips them to the top — and a move of the field afterwards (it dropping
/// back as the inset goes to zero) does not flip them again.
#[test]
fn the_edge_is_decided_after_the_screens_settle_and_then_held() {
    let low = Rect::new(10.0, 560.0, 260.0, 590.0);
    let lifted = Rect::new(10.0, 300.0, 260.0, 330.0);
    let field = Entity::from_raw_u32(7).unwrap();

    // A composer that lifts above the keys: still at the bottom.
    let mut k = kb();
    k.set_field(Some((field, low)));
    k.place(phone(), 48.0);
    assert!(
        k.inset(phone()) > 0.0,
        "the keys publish no inset to lift by"
    );
    k.set_field(Some((field, lifted)));
    for _ in 0..SETTLE_FRAMES {
        k.place(phone(), 48.0);
    }
    assert_eq!(k.edge, Edge::Bottom);

    // A field pinned to the bottom: the keys flip, and stay flipped.
    let mut k = kb();
    k.set_field(Some((field, low)));
    for _ in 0..SETTLE_FRAMES {
        k.place(phone(), 48.0);
    }
    assert_eq!(k.edge, Edge::Top);
    assert_eq!(k.inset(phone()), 0.0, "keys at the top lift nothing");
    k.set_field(Some((field, lifted)));
    k.place(phone(), 48.0);
    assert_eq!(k.edge, Edge::Top, "the edge flapped");

    // Another field decides afresh.
    let other = Entity::from_raw_u32(8).unwrap();
    k.set_field(Some((other, lifted)));
    for _ in 0..SETTLE_FRAMES {
        k.place(phone(), 48.0);
    }
    assert_eq!(k.edge, Edge::Bottom);
}

/// A viewport too short for four 48 dp rows must still produce a usable
/// keyboard rather than one taller than the screen.
#[test]
fn a_short_viewport_shrinks_the_keys_instead_of_overflowing() {
    let vp = Rect::new(0.0, 0.0, 640.0, 260.0);
    let slab = layout(vp, 48.0, Edge::Bottom);
    assert!(
        vp.contains(slab.rect.min) && vp.contains(slab.rect.max),
        "slab {:?} escaped",
        slab.rect
    );
    assert!(slab.key_h < 48.0 && slab.key_h >= MIN_KEY_HEIGHT_PTS);
}

/// The prediction a composer reserves is the slab it gets.
#[test]
fn the_expected_height_is_the_slab_at_the_bottom() {
    let size = Vec2::new(274.0, 609.0);
    let mut k = kb();
    let min_key_h = min_key_height_pts(1.5, 2.75);
    k.place(Rect::from_corners(Vec2::ZERO, size), min_key_h);
    let want = expected_height_in(size, 1.5, 2.75);
    assert!((k.inset(Rect::from_corners(Vec2::ZERO, size)) - want).abs() < 1e-3);
}

/// The platform policy, without an environment. Android always (it has no
/// IME at all), the web only on the phone tier (one binary serves phones and
/// desktops), and the override wins both ways.
#[test]
fn the_platform_predicate_is_not_a_cfg() {
    // (force_on, override_present, android, web_phone) -> enabled
    for (on, present, android, web_phone, want) in [
        (false, false, true, false, true),   // Android
        (false, false, false, true, true),   // web phone
        (false, false, false, false, false), // desktop
        (true, true, false, false, true),    // ?osk on a desktop
        (false, true, true, false, false),   // ?osk=0 on Android
    ] {
        assert_eq!(
            resolve_enabled(on, present, android, web_phone),
            want,
            "({on}, {present}, {android}, {web_phone})"
        );
    }
}
