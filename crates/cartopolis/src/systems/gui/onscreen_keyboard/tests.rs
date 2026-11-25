use super::*;
use crate::systems::egui_manager::UnpaintedPasses as _;

/// The client's frame loop, minus Bevy: a sign-in card with a text field in
/// it, the keyboard drawn after it in the same pass, and a touch bridge that
/// produces the events `egui_manager::pointer_events` produces on a phone.
///
/// This exists because every bug this module has had lives in the *sequence*
/// — focus surrendered by one tap and restored by the next, a layer raised
/// after another one, a widget that stopped being drawn — and none of it is
/// reachable from a test of the `press` state machine, which is pure by
/// design.
struct Harness {
    ctx: egui::Context,
    kb: OnScreenKeyboard,
    text: String,
    /// What the keyboard typed last frame, waiting for the next `RawInput`
    /// exactly as `EguiManager::injected` holds it.
    injected: Vec<egui::Event>,
    screen: egui::Rect,
    field: egui::Rect,
    /// Android's touch ids only ever increase, and the bridge keys off them.
    next_touch: u64,
    /// How often the card's backdrop has taken a tap. Non-zero after a tap
    /// on a key means the keyboard is no longer the top layer where it draws.
    backdrop_clicks: usize,
    /// Is the card's full-viewport dim drawn? Switchable so a test can make
    /// the modal appear *after* the keyboard, which is the case that decides
    /// the `Order` rather than the raise.
    backdrop: bool,
    /// Where the card sits. Centred by default; a test pins it to the bottom
    /// edge to put the field where the keys land.
    anchor: (egui::Align2, egui::Vec2),
    /// Does the card claim [`field_makes_room`] for its field?
    makes_room: bool,
}

impl Harness {
    fn new() -> Self {
        let ctx = egui::Context::default();
        crate::systems::theme::install(&ctx);
        Self {
            ctx,
            // Closed, the way a run starts: the field being tapped is what
            // opens it.
            kb: OnScreenKeyboard {
                open: false,
                ..kb()
            },
            text: String::new(),
            injected: Vec::new(),
            // The phone this was sized for.
            screen: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(274.0, 609.0)),
            field: egui::Rect::NOTHING,
            next_touch: 1,
            backdrop_clicks: 0,
            backdrop: true,
            anchor: (egui::Align2::CENTER_CENTER, egui::Vec2::ZERO),
            makes_room: false,
        }
    }

    /// One frame: begin the pass with these events, draw the card, run the
    /// keyboard, end the pass.
    fn frame(&mut self, mut events: Vec<egui::Event>) {
        // The injected batch goes after the pointer batch, where
        // `begin_egui_frame` puts it.
        events.extend(std::mem::take(&mut self.injected));
        self.ctx.begin_pass(egui::RawInput {
            screen_rect: Some(self.screen),
            events,
            focused: true,
            ..Default::default()
        });

        // The sign-in card's dim backdrop: a full-viewport click-sensing
        // `Area` at the same `Order` as the card, raised every frame. It is
        // in the harness because it is the layer that eats the keys when the
        // keyboard loses its place in the stack — a bug that is invisible
        // without something covering the slab.
        let dim_id = egui::Id::new("sign_in_dim");
        let mut dismissed = false;
        if self.backdrop {
            egui::Area::new(dim_id)
                .order(egui::Order::Foreground)
                .fixed_pos(egui::pos2(0.0, 0.0))
                .show(&self.ctx, |ui| {
                    let (_, resp) =
                        ui.allocate_exact_size(self.screen.size(), egui::Sense::click_and_drag());
                    dismissed = resp.clicked();
                });
            self.ctx
                .move_to_top(egui::LayerId::new(egui::Order::Foreground, dim_id));
        }
        self.backdrop_clicks += usize::from(dismissed);

        let card_id = egui::Id::new("sign_in_card");
        let text = &mut self.text;
        let makes_room = self.makes_room;
        let mut field = egui::Rect::NOTHING;
        egui::Window::new("sign in")
            .id(card_id)
            .order(egui::Order::Foreground)
            .title_bar(false)
            .resizable(false)
            .movable(false)
            .collapsible(false)
            .anchor(self.anchor.0, self.anchor.1)
            .show(&self.ctx, |ui| {
                ui.set_width(220.0);
                egui::ScrollArea::vertical()
                    .max_height(220.0)
                    .show(ui, |ui| {
                        let resp =
                            ui.add(egui::TextEdit::singleline(text).desired_width(f32::INFINITY));
                        field = resp.rect;
                        if makes_room {
                            field_makes_room(ui.ctx(), resp.id);
                        }
                    });
            });
        // The card raises itself every frame; the keyboard has to raise itself
        // after that or its keys are under the card's layer.
        self.ctx
            .move_to_top(egui::LayerId::new(egui::Order::Foreground, card_id));
        self.field = field;

        self.injected = self.kb.pass(&self.ctx, 1.5, 2.75);
        let _ = self.ctx.end_pass_unpainted();
    }

    /// A finger down and up at one point, then a quiet frame — the shape
    /// `pointer_events` emits, including the `PointerGone` that a lifting
    /// finger sends and the split of press and release across two frames.
    fn tap(&mut self, pos: egui::Pos2) {
        let id = egui::TouchId(self.next_touch);
        self.next_touch += 1;
        let touch = |phase| egui::Event::Touch {
            device_id: egui::TouchDeviceId(0),
            id,
            phase,
            pos,
            force: None,
        };
        self.frame(vec![
            touch(egui::TouchPhase::Start),
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ]);
        self.frame(vec![
            touch(egui::TouchPhase::End),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerGone,
        ]);
        // A frame with nothing in it, of which a real run has many.
        self.frame(Vec::new());
    }

    /// Where the key carrying `cap` is on the slab the keyboard last drew.
    fn key_pos(&self, want: Cap) -> egui::Pos2 {
        let slab = self.kb.slab.expect("the keyboard is not on screen");
        for (r, row) in rows(self.kb.layer).iter().enumerate() {
            let used: f32 = row.iter().map(|k| k.units).sum();
            let mut col = (COLUMNS - used) * 0.5;
            for key in row {
                let rect = slab.key_rect(r, col, key.units);
                col += key.units;
                if key.cap == want {
                    return rect.center();
                }
            }
        }
        panic!("no {want:?} key on the {:?} layer", self.kb.layer);
    }

    fn tap_key(&mut self, cap: Cap) {
        self.tap(self.key_pos(cap));
    }
}

/// **The bug the phone found:** close the keyboard, tap the field again, and
/// the keys draw but type nothing.
#[test]
fn the_keyboard_types_again_after_being_closed_and_reopened() {
    let mut h = Harness::new();
    h.frame(Vec::new());
    h.frame(Vec::new());

    let field = h.field.center();
    h.tap(field);
    assert!(
        h.kb.is_open(),
        "tapping the field did not open the keyboard"
    );

    h.tap_key(a());
    assert_eq!(h.text, "a", "the keyboard did not type on first use");

    h.tap_key(Cap::Hide);
    assert!(!h.kb.is_open(), "the hide key did not close the keyboard");

    h.tap(field);
    assert!(h.kb.is_open(), "the keyboard did not come back");

    let backdrop_before = h.backdrop_clicks;
    h.tap_key(a());
    assert_eq!(
        h.backdrop_clicks, backdrop_before,
        "the tap went to the card's backdrop, not to the key: the keyboard's \
             layer is no longer on top"
    );
    assert_eq!(h.text, "aa", "the reopened keyboard typed nothing");
}

/// The other half of the same bug, and the reason the fix is a higher
/// `Order` rather than a more insistent `move_to_top`: a rival layer created
/// **after** the keyboard is on top from the moment it exists, and no amount
/// of re-raising displaces it. Type into the geo search, then open the
/// sign-in card over it, and the keys would go dead without a single one of
/// them having been touched.
#[test]
fn a_modal_opened_over_the_keyboard_does_not_take_its_keys() {
    let mut h = Harness::new();
    h.backdrop = false;
    h.frame(Vec::new());
    h.frame(Vec::new());

    h.tap(h.field.center());
    assert!(h.kb.is_open());
    h.tap_key(a());
    assert_eq!(h.text, "a");

    // The modal arrives, and it covers the whole viewport.
    h.backdrop = true;
    h.frame(Vec::new());

    let backdrop_before = h.backdrop_clicks;
    h.tap_key(a());
    assert_eq!(
        h.backdrop_clicks, backdrop_before,
        "a modal opened later took the keyboard's taps"
    );
    assert_eq!(h.text, "aa", "the keyboard went dead under a new modal");
}

fn touch(id: u64, phase: egui::TouchPhase, pos: egui::Pos2) -> egui::Event {
    egui::Event::Touch {
        device_id: egui::TouchDeviceId(0),
        id: egui::TouchId(id),
        phase,
        pos,
        force: None,
    }
}

/// Two thumbs: the second lands before the first lifts. Only the first
/// finger drives egui's pointer, so a key that typed on `clicked()` lost
/// the second letter every time.
#[test]
fn overlapping_thumbs_type_both_keys() {
    use egui::TouchPhase::{End, Start};
    let mut h = Harness::new();
    h.frame(Vec::new());
    h.frame(Vec::new());
    h.tap(h.field.center());
    let q = h.key_pos(Cap::Char {
        base: 'q',
        alt: 'Q',
    });
    let p = h.key_pos(Cap::Char {
        base: 'p',
        alt: 'P',
    });
    h.frame(vec![touch(50, Start, q)]);
    h.frame(vec![touch(51, Start, p)]);
    h.frame(vec![touch(50, End, q)]);
    h.frame(vec![touch(51, End, p)]);
    h.frame(Vec::new());
    assert_eq!(h.text, "qp");
}

/// A finger that rolls a few points before lifting is a drag to egui, and
/// still a keystroke here: the key under it as it lifts.
#[test]
fn a_drifting_finger_types_the_key_it_lifts_from() {
    use egui::TouchPhase::{End, Move, Start};
    let mut h = Harness::new();
    h.frame(Vec::new());
    h.frame(Vec::new());
    h.tap(h.field.center());
    let a = h.key_pos(a());
    let drift = a + egui::vec2(3.0, 9.0);
    h.frame(vec![touch(60, Start, a)]);
    h.frame(vec![touch(60, Move, drift)]);
    h.frame(vec![touch(60, End, drift)]);
    h.frame(Vec::new());
    assert_eq!(h.text, "a");
}

fn kb() -> OnScreenKeyboard {
    OnScreenKeyboard {
        enabled: true,
        open: true,
        target: None,
        layer: KeyLayer::default(),
        shift: ShiftState::default(),
        slab: None,
        fingers: Vec::new(),
        touch_seen: false,
        lib: false,
        lib_field: false,
        lib_blur: false,
    }
}

fn a() -> Cap {
    Cap::Char {
        base: 'a',
        alt: 'A',
    }
}

/// Shift is the whole reason this is a state machine: one tap arms it for a
/// single character, a second locks it, a third clears it — and typing a
/// character consumes the armed state but not the locked one. A boolean here
/// makes every capital two extra taps.
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
    assert_eq!(k.press(a()), vec![egui::Event::Text("A".to_owned())]);
    assert_eq!(k.shift, ShiftState::Off, "one-shot shift did not release");
    assert_eq!(k.press(a()), vec![egui::Event::Text("a".to_owned())]);
}

#[test]
fn caps_lock_survives_every_character() {
    let mut k = kb();
    k.press(Cap::Shift);
    k.press(Cap::Shift);
    for _ in 0..3 {
        assert_eq!(k.press(a()), vec![egui::Event::Text("A".to_owned())]);
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
/// `$`. This is why a key carries both forms rather than calling
/// `to_uppercase` at press time.
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
    assert_eq!(k.press(four.cap), vec![egui::Event::Text("$".to_owned())]);
}

/// The hide key closes the keyboard and emits nothing; the system then drops
/// the field's focus so the next tap on it can bring the keyboard back.
#[test]
fn hide_closes_and_types_nothing() {
    let mut k = kb();
    assert!(k.press(Cap::Hide).is_empty());
    assert!(!k.is_open());
}

/// The editing keys are `Event::Key` **only** (a real keyboard's `text` for
/// them is a control character the bridge filters out) and each is a press
/// *and* its release, so `keys_down` never accumulates a key no finger is
/// holding.
#[test]
fn editing_keys_emit_a_press_and_a_release_and_no_text() {
    let mut k = kb();
    for (cap, want) in [
        (Cap::Backspace, egui::Key::Backspace),
        (Cap::Enter, egui::Key::Enter),
        (Cap::Left, egui::Key::ArrowLeft),
        (Cap::Right, egui::Key::ArrowRight),
    ] {
        let evs = k.press(cap);
        assert_eq!(evs.len(), 2, "{cap:?} should be press+release: {evs:?}");
        let pressed: Vec<bool> = evs
            .iter()
            .map(|e| match e {
                egui::Event::Key {
                    key,
                    pressed,
                    repeat,
                    modifiers,
                    physical_key,
                } => {
                    assert_eq!(*key, want);
                    assert!(!*repeat);
                    assert_eq!(*modifiers, egui::Modifiers::NONE);
                    assert_eq!(*physical_key, None);
                    *pressed
                }
                other => panic!("{cap:?} emitted {other:?}, expected a Key event"),
            })
            .collect();
        assert_eq!(pressed, vec![true, false]);
    }
}

/// Select-all is egui's own chord (`Key::A` + command), which is what makes
/// it the same code path a desktop Ctrl+A takes instead of a second
/// implementation of "select everything".
#[test]
fn select_all_is_the_command_a_chord() {
    let mut k = kb();
    let evs = k.press(Cap::SelectAll);
    assert_eq!(evs.len(), 2);
    let egui::Event::Key { key, modifiers, .. } = evs[0] else {
        panic!("expected a Key event, got {:?}", evs[0]);
    };
    assert_eq!(key, egui::Key::A);
    assert!(
        modifiers.command,
        "egui selects all on `command`, not `ctrl`"
    );
    // It types nothing and disturbs nothing — a selection is a state of the
    // field, not an edit of it.
    assert_eq!(k.shift, ShiftState::Off);
}

/// Shift turns the arrows into a selection, and — unlike a character — an
/// arrow does **not** consume a one-shot shift. A selection is grown one
/// press at a time, so a shift that fell off after the first would have to be
/// re-armed for every letter of the word being selected.
#[test]
fn shift_makes_the_arrows_extend_a_selection_without_consuming_itself() {
    let mut k = kb();
    k.press(Cap::Shift);
    for _ in 0..3 {
        let evs = k.press(Cap::Right);
        let egui::Event::Key { modifiers, .. } = evs[0] else {
            panic!("expected a Key event");
        };
        assert!(modifiers.shift, "arrow did not carry shift");
        assert_eq!(k.shift, ShiftState::Once, "shift fell off mid-selection");
    }
    // Cleared, the same arrow moves the caret instead — which is what egui
    // distinguishes on (`text_selection::cursor_range::on_key_press`).
    k.press(Cap::Shift);
    k.press(Cap::Shift);
    let evs = k.press(Cap::Right);
    let egui::Event::Key { modifiers, .. } = evs[0] else {
        panic!("expected a Key event");
    };
    assert!(!modifiers.shift);
}

/// Space is `Text(" ")` and **not** `Key::Space`: egui activates the focused
/// widget on Space, so a space bar that also emitted the key would
/// occasionally press a button instead of typing.
#[test]
fn space_is_text_only() {
    let mut k = kb();
    assert_eq!(k.press(Cap::Space), vec![egui::Event::Text(" ".to_owned())]);
}

/// The cursor keys have to survive `TextEdit`'s own event filter, which is
/// the thing that decides whether an arrow edits the text or moves focus to
/// the next widget. Pinning it against egui's own type means an upstream
/// change to `EventFilter::matches` fails here rather than on glass.
#[test]
fn the_arrows_pass_a_text_edits_event_filter() {
    let filter = egui::EventFilter {
        horizontal_arrows: true,
        vertical_arrows: true,
        tab: false,
        escape: false,
    };
    let mut k = kb();
    for cap in [Cap::Left, Cap::Right, Cap::Backspace, Cap::Enter] {
        for ev in k.press(cap) {
            assert!(
                filter.matches(&ev),
                "{cap:?} produced {ev:?}, which a focused TextEdit filters out"
            );
        }
    }
}

/// End to end through egui itself: focus a single-line field, hand it the
/// events the keyboard would have produced for a shifted `a`, and read the
/// string back. This is the assertion that the *shape* of the events — one
/// bare `Event::Text` carrying the already-shifted character, no modifier —
/// is what egui expects, as opposed to merely what we decided to send.
#[test]
fn a_shifted_character_reaches_a_text_edit() {
    let ctx = egui::Context::default();
    crate::systems::theme::install(&ctx);
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0));
    let mut text = String::new();

    let mut k = kb();
    k.press(Cap::Shift);
    let mut queued = k.press(a());
    queued.extend(k.press(Cap::Char {
        base: 'b',
        alt: 'B',
    }));
    queued.extend(k.press(Cap::Backspace));
    queued.extend(k.press(Cap::Char {
        base: 'c',
        alt: 'C',
    }));

    for frame in 0..3 {
        let events = if frame == 1 {
            std::mem::take(&mut queued)
        } else {
            Vec::new()
        };
        let text_ref = &mut text;
        let _ = ctx.run_ui_unpainted(
            egui::RawInput {
                screen_rect: Some(screen),
                events,
                focused: true,
                ..Default::default()
            },
            |ui| {
                let resp = ui.add(egui::TextEdit::singleline(text_ref));
                if frame == 0 {
                    resp.request_focus();
                }
            },
        );
    }
    assert_eq!(text, "Ac", "typed {text:?}");
}

/// The phone this was sized for: 274 × 609 pt of egui viewport. The keyboard
/// has to fit inside it, keep its 48 dp of key height, and leave most of the
/// screen alone.
#[test]
fn the_keyboard_fits_a_phone_viewport() {
    let vp = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(274.0, 609.0));
    let min_key_h = theme::min_touch_target_pts(theme::dp_per_point(1.5, 2.75, false));
    let slab = layout(vp, min_key_h, None);

    assert!(vp.contains_rect(slab.rect), "slab {:?} escaped", slab.rect);
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
    // Docked to the bottom when nothing is in the way.
    assert!(slab.rect.bottom() > vp.bottom() - 2.0 * EDGE_MARGIN);
}

/// No row exceeds [`COLUMNS`] units, in either layer, so the keys line up in
/// a grid and no key can hang off the slab. A row that does not add up is
/// invisible in a screenshot and obvious here. It also pins the two layers to
/// the *same* key counts per row, which is what lets a layer swap leave every
/// key where the finger last found it.
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
    // `layout` sizes the slab from `ROW_COUNT` before `rows` has been asked
    // for anything, so the two must agree or the keyboard is the wrong height.
    assert_eq!(letters.len() as f32, ROW_COUNT);
}

/// Every key rect stays inside the slab, which is what makes the
/// backdrop-first hit-test guarantee hold: a key hanging over the edge would
/// take presses the world thinks are its own.
#[test]
fn every_key_stays_inside_the_slab() {
    let vp = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(274.0, 609.0));
    let slab = layout(vp, 32.0, None);
    for (r, row) in rows(KeyLayer::Letters).iter().enumerate() {
        let used: f32 = row.iter().map(|k| k.units).sum();
        let mut col = (COLUMNS - used) * 0.5;
        for key in row {
            let rect = slab.key_rect(r, col, key.units);
            col += key.units;
            assert!(
                slab.rect.contains_rect(rect),
                "{:?} at row {r} escaped the slab",
                key.cap
            );
            assert!(rect.width() > 0.0 && rect.height() > 0.0);
        }
    }
}

/// The one layout rule that is not cosmetic: a keyboard over the map is
/// fine, a keyboard over the field you are typing into is useless. When the
/// field is low on the screen the slab moves to the top instead.
#[test]
fn the_keyboard_moves_off_the_field_it_is_serving() {
    let vp = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(274.0, 609.0));

    let high = egui::Rect::from_min_size(egui::pos2(10.0, 60.0), egui::vec2(250.0, 24.0));
    let low = egui::Rect::from_min_size(egui::pos2(10.0, 540.0), egui::vec2(250.0, 24.0));

    let with_high = layout(vp, 32.0, Some(high));
    assert!(
        !with_high.rect.intersects(high),
        "a field near the top should leave the keyboard at the bottom"
    );
    assert!(with_high.rect.top() > vp.center().y);

    let with_low = layout(vp, 32.0, Some(low));
    assert!(
        !with_low.rect.intersects(low),
        "the keyboard covered the field it exists to serve"
    );
    assert!(
        with_low.rect.top() < vp.center().y,
        "it should flip to the top"
    );
}

/// The chat thread's composer, pinned to the bottom of its panel: tapping it
/// put the keys at the **top**, over the conversation, because on the frame
/// the keyboard opens the field has not lifted yet. A field that claims
/// [`field_makes_room`] keeps the keys on the bottom edge; one that does not
/// still gets the flip, which is right for a field that cannot move.
#[test]
fn a_field_that_makes_room_keeps_the_keys_at_the_bottom() {
    for makes_room in [false, true] {
        let mut h = Harness::new();
        h.backdrop = false;
        h.makes_room = makes_room;
        h.anchor = (egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -8.0));
        h.frame(Vec::new());
        h.frame(Vec::new());

        h.tap(h.field.center());
        assert!(
            h.kb.is_open(),
            "tapping the field did not open the keyboard"
        );
        let slab = h.kb.slab().expect("open keyboard has no slab");
        let at_bottom = slab.top() > h.screen.center().y;
        assert_eq!(
            at_bottom, makes_room,
            "makes_room = {makes_room}: keyboard at {slab:?}"
        );
    }
}

/// A viewport too short for four 48 dp rows must still produce a usable
/// keyboard rather than one taller than the screen — a landscape phone, or a
/// desktop window forced on with `?osk` and then dragged small.
#[test]
fn a_short_viewport_shrinks_the_keys_instead_of_overflowing() {
    let vp = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(640.0, 260.0));
    let slab = layout(vp, 48.0, None);
    assert!(vp.contains_rect(slab.rect), "slab {:?} escaped", slab.rect);
    assert!(slab.key_h < 48.0 && slab.key_h >= MIN_KEY_HEIGHT_PTS);
}

/// The platform policy, without an environment. Android always (it has no
/// IME at all), the web only on the phone tier (one binary serves phones and
/// desktops), and the override wins both ways — `?osk` to see it on a
/// workstation, `?osk=0` to get rid of it on a phone when it misbehaves.
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
