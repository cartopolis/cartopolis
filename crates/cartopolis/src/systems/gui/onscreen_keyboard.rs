//! An in-app on-screen keyboard, because two of our platforms have no IME.
//!
//! Android's NativeActivity brings up no soft keyboard at all (there is no Java
//! `InputMethodManager` on the other end of it — `docs/notes/android-build.md`),
//! so every `egui::TextEdit` in the client can be *focused* on the phone and
//! cannot be *typed into*. That is not a rough edge on a side feature: it is the
//! address bar and the geo search, i.e. the front door of a routing client, plus
//! bookmark rename, nearby chat and the assistant. The web build on a phone has
//! the same problem from the other direction — the browser IME does come up, but
//! it resizes the visual viewport out from under a WebGL canvas that winit sized
//! from the layout viewport, and the field it is supposed to be serving ends up
//! behind it.
//!
//! So the keys are drawn by us, and the characters are pushed into egui's own
//! input queue as if a keyboard had produced them.
//!
//! ## Three things about this are not obvious
//!
//! **Tapping a key takes focus away from the field.** egui surrenders a focused
//! widget's focus on any click that isn't on it (`SurrenderFocusOn::Clicks`, the
//! default, evaluated inside the `TextEdit`'s own `interact`), and our keys are
//! very much "not on it". So the field loses focus on the *first* key press, the
//! injected character then has nowhere to go, and the keyboard — which triggers
//! on `text_edit_focused()` — hides itself. The fix is to put focus back, in the
//! same frame, right after the tap: [`restore_focus`]. Two consequences follow
//! and both are load-bearing:
//!
//!   - **This system must run after every system that draws a text field**, or
//!     the surrender happens *after* our restore and undoes it. See the
//!     registration note in `gui::plugin`. It is also what makes `read_response` on
//!     the focused field return this frame's rect rather than nothing.
//!   - `Memory::request_focus` installs a **default** `EventFilter`, and
//!     `Focus::begin_pass` consults that filter *before* the `TextEdit` gets a
//!     chance to re-lock it. An injected `ArrowLeft` would therefore be read as
//!     "move focus left" for exactly one frame — cursor keys that eject you from
//!     the field you are editing. [`restore_focus`] re-locks the filter itself.
//!
//! **The events arrive one frame late, and that is the correct design.**
//! `begin_egui_frame` assembles `RawInput` in `PreUpdate`; this system runs in
//! `Update`, after the pass has already begun. So a tap enqueues onto
//! [`EguiManager::inject_events`] and `begin_egui_frame` drains it into the
//! *next* frame's events, alongside the ones the physical keyboard bridge
//! produces and after the pointer batch — so the stream-order contract that
//! `take_pass_events` exists to protect (`docs/notes/touch-input.md`) is
//! untouched. It also gives the Enter key a free correctness property: the
//! surrender-and-restore happens on the tap frame, the `Key::Enter` lands on the
//! next one, and a single-line `TextEdit` answering it with "lose focus" is not
//! then fought by a restore that already ran.
//!
//! **It lives above `Order::Foreground`, where the rest of the chrome is.** A
//! keyboard that comes and goes cannot share an order with layers that stay:
//! `move_to_top` is a flag read by a *stable* sort, so a layer that skips a frame
//! sorts under everything that didn't and can never climb back. That is what made
//! every reopened keyboard draw its keys over a sign-in backdrop that was quietly
//! taking all the taps. [`draw`] has the mechanism.
//!
//! **It is an `Area`, not a root-`Ui` panel.** It draws over everything, and a
//! panel would reserve space in a layout that has already been computed by the
//! time we run. That also keeps it *out* of the panel `.chain()`, which is
//! only about systems sharing the root `Ui` — `dock.rs` and `hud.rs`'s bar are
//! the precedent, and `egui_manager`'s `every_root_ui_panel_is_in_the_chain`
//! test keys off `ui_mut()`, which nothing here calls.
//!
//! ## What it does not do
//!
//! No IME, no prediction, no autocorrect, no long-press accents, no clipboard
//! keys (Ctrl+V has no finger equivalent and paste is reachable from egui's own
//! context menu on a long press). Nothing here is a substitute for a platform
//! IME; it is what makes a text field usable at all on a platform that has none.

use crate::systems::egui_manager::EguiMut;
use bevy::prelude::*;

use crate::systems::icons;
use crate::systems::theme;

// ─── Platform predicate ───────────────────────────────────────────────────────

/// Resolve "does this platform need an on-screen keyboard", given the pieces.
///
/// Split out from [`platform_wants_keyboard`] so the policy is testable without
/// an environment, a browser or an Android device.
///
/// The override is checked first and is a *tri-state*, not a switch: `?osk` /
/// `CARTO_OSK=1` forces it on (which is the only way to see it on a
/// workstation, since neither of the two platforms that need it can be attached
/// to a debugger comfortably), and `?osk=0` forces it off (which is what you
/// want on a phone the day this module has a bug in it and the address bar is
/// the thing you are trying to reach).
fn resolve_enabled(force_on: bool, override_present: bool, android: bool, web_phone: bool) -> bool {
    if force_on {
        return true;
    }
    if override_present {
        return false;
    }
    android || web_phone
}

/// Does this run need the on-screen keyboard?
///
/// **Deliberately not a `cfg!` alone.** The web build runs on phones *and* on
/// desktops from the same binary, so a `cfg!(target_arch = "wasm32")` arm would
/// put a thumb keyboard under a mouse. The tier comes from [`crate::quality`],
/// which `docs/notes/touch-input.md` is explicit about being the *one* detector
/// in this client: it already answers "coarse pointer on a small viewport",
/// decided by `index.html` before the module is fetched, and adding a second
/// media query here is exactly how the UI ends up finger-sized on a machine
/// rendering at desktop resolution.
///
/// Android is unconditional and does not consult the tier, because the reason
/// there is not ergonomics but the total absence of an IME: a hypothetical
/// Android tablet that `quality` calls `Desktop` still cannot type.
pub fn platform_wants_keyboard() -> bool {
    resolve_enabled(
        crate::env_flag("CARTO_OSK"),
        crate::env_value("CARTO_OSK").is_some(),
        cfg!(target_os = "android"),
        cfg!(target_arch = "wasm32") && crate::quality::is_phone(),
    )
}

/// The shortest a key may be, in egui points — Material's 48 dp minimum touch
/// target converted through [`theme::dp_per_point`].
///
/// The conversion itself lives in `theme`, not here: the dock discs, the close
/// button and the driver rail ask the same question, and this module having
/// carried its own copy of the constant and the arithmetic is how a second
/// answer got into the tree. What stays here is the one thing that is the
/// keyboard's own — that a key is held to the *floor* a row is held to, because
/// a key is pressed forty times in a row where a row is pressed once.
fn min_key_height_pts(ui_scale: f32, display_scale: f32) -> f32 {
    theme::min_touch_target_pts(theme::dp_per_point(
        ui_scale,
        display_scale,
        cfg!(target_arch = "wasm32") && crate::quality::is_phone(),
    ))
}

// ─── Key vocabulary ───────────────────────────────────────────────────────────

/// Which set of characters the keys are showing.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum KeyLayer {
    /// QWERTY.
    #[default]
    Letters,
    /// Digits and punctuation.
    Symbols,
}

/// The shift key's three states.
///
/// `Once` is the phone convention (and the reason shift is a state machine and
/// not a boolean): one tap capitalises the next character and then releases,
/// which is what typing a sentence needs, while a second tap locks it for an
/// acronym or a postcode. A plain toggle makes the common case two taps per
/// word — one to set it, one to clear it — and people stop using it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ShiftState {
    #[default]
    Off,
    /// Armed for exactly one character.
    Once,
    /// Held until tapped off.
    Lock,
}

impl ShiftState {
    /// Is the alternate (upper) form of a key showing?
    fn alt(self) -> bool {
        self != Self::Off
    }

    /// Tapping the shift key: off → once → lock → off.
    fn tapped(self) -> Self {
        match self {
            Self::Off => Self::Once,
            Self::Once => Self::Lock,
            Self::Lock => Self::Off,
        }
    }

    /// What shift becomes after a character has been typed with it.
    fn consumed(self) -> Self {
        match self {
            Self::Once => Self::Off,
            other => other,
        }
    }
}

/// What a key does. The character keys carry *both* forms, so shift is one
/// lookup rather than a `to_uppercase` special case that only works for letters
/// — on the symbol layer the alternate of `4` is `$`, which no case mapping
/// produces.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Cap {
    Char {
        base: char,
        alt: char,
    },
    Space,
    Backspace,
    Enter,
    Shift,
    /// Swap between [`KeyLayer::Letters`] and [`KeyLayer::Symbols`].
    Layer,
    /// Put the keyboard away (and drop the field's focus with it — see
    /// [`OnScreenKeyboard::press`]).
    Hide,
    Left,
    Right,
    /// Select the whole field.
    ///
    /// **The one editing verb a finger cannot otherwise reach.** Selecting text
    /// with a pointer is a press-drag-release across the glyphs, and on this
    /// client that gesture is already spoken for twice over: the touch bridge
    /// holds the emulated left button down for the whole gesture, and a finger
    /// held still on a field for half a second is the long-press that opens the
    /// world context menu (`systems::touch`). So "replace what is in this field"
    /// — which on an address bar or a search box is *most* of the editing anyone
    /// does — had no gesture at all, and the alternative was tapping backspace
    /// once per character.
    SelectAll,
}

/// One key: what it does and how many column units wide it is. A row is always
/// exactly [`COLUMNS`] units, so the geometry never has to measure anything.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Key {
    cap: Cap,
    units: f32,
}

impl Key {
    const fn new(cap: Cap, units: f32) -> Self {
        Self { cap, units }
    }
}

/// Column units in a row. Ten, because the top row of a QWERTY has ten letters
/// and everything else is measured against it.
const COLUMNS: f32 = 10.0;

/// Rows: three of characters plus the action row. Kept as a constant because
/// [`layout`] has to know the height before [`rows`] has been asked for anything
/// — the dock edge depends on the total height, and the total height on the row
/// count.
const ROW_COUNT: f32 = 4.0;

const LETTER_ROWS: [&str; 3] = ["qwertyuiop", "asdfghjkl", "zxcvbnm"];

/// The symbol layer, as (base, alternate) pairs by position. Deliberately ASCII
/// throughout: egui's bundled font is Ubuntu-Light plus a thin emoji subset
/// (`systems::icons` documents how thin, and why the icons left it), and a `€`
/// or a `¿` that renders
/// as an empty box is worse than not offering it.
const SYMBOL_ROWS: [(&str, &str); 3] = [
    ("1234567890", "!@#$%^&*()"),
    ("-/:();&@\"", "_\\|[]{}~'"),
    (".,?!'+=", "><\\/`{}"),
];

/// The rows for a given layer, top to bottom.
///
/// **Shift is not a parameter**, and that is deliberate: a key carries both its
/// forms, so the geometry is identical in every shift state and a key can never
/// move under a finger that is already on its way down. Shift only decides which
/// of the two forms [`OnScreenKeyboard::press`] emits and [`paint_key`] draws.
///
/// Row 3 is the same on both layers on purpose too: the keys that are not
/// letters — layer swap, cursor arrows, space, enter, hide — do not move when you
/// reach for the digits, so the muscle memory for "put this away" survives a
/// layer switch.
fn rows(layer: KeyLayer) -> Vec<Vec<Key>> {
    let mut out: Vec<Vec<Key>> = Vec::with_capacity(4);

    let chars = |base: &str, alts: &str| -> Vec<Key> {
        base.chars()
            .zip(alts.chars())
            .map(|(b, a)| Key::new(Cap::Char { base: b, alt: a }, 1.0))
            .collect()
    };

    let (r0, r1, r2): (Vec<Key>, Vec<Key>, Vec<Key>) = match layer {
        KeyLayer::Letters => {
            let upper: Vec<String> = LETTER_ROWS.iter().map(|r| r.to_ascii_uppercase()).collect();
            (
                chars(LETTER_ROWS[0], &upper[0]),
                chars(LETTER_ROWS[1], &upper[1]),
                chars(LETTER_ROWS[2], &upper[2]),
            )
        }
        KeyLayer::Symbols => (
            chars(SYMBOL_ROWS[0].0, SYMBOL_ROWS[0].1),
            chars(SYMBOL_ROWS[1].0, SYMBOL_ROWS[1].1),
            chars(SYMBOL_ROWS[2].0, SYMBOL_ROWS[2].1),
        ),
    };

    out.push(r0);
    out.push(r1);

    let mut third = vec![Key::new(Cap::Shift, 1.5)];
    third.extend(r2);
    third.push(Key::new(Cap::Backspace, 1.5));
    out.push(third);

    // Ten units exactly, like every other row. Select-all is paid for out of the
    // space bar and the layer key rather than out of a new row: the slab already
    // takes the bottom third of a phone screen, and the field being edited has to
    // stay visible above it.
    out.push(vec![
        Key::new(Cap::Layer, 1.25),
        Key::new(Cap::SelectAll, 1.0),
        Key::new(Cap::Left, 1.0),
        Key::new(Cap::Space, 3.25),
        Key::new(Cap::Right, 1.0),
        Key::new(Cap::Enter, 1.5),
        Key::new(Cap::Hide, 1.0),
    ]);
    out
}

// ─── Geometry ─────────────────────────────────────────────────────────────────

/// Gap between two keys, in points. Small: on a 274 pt viewport every point of
/// gap is a point the keys do not get.
const GAP: f32 = 3.0;
/// Padding between the slab's edge and the outermost keys.
const PAD: f32 = 5.0;
/// Gap between the slab and the edge of the viewport it is docked to.
const EDGE_MARGIN: f32 = 6.0;
/// Ceiling on the slab, as a fraction of the viewport height. A keyboard that
/// eats more than this of a phone screen leaves nothing to type *into*; below
/// this the 48 dp key wins, above it the viewport does.
const MAX_HEIGHT_FRACTION: f32 = 0.45;
/// Absolute floor on a key, in points. Only reachable on a viewport short enough
/// that [`MAX_HEIGHT_FRACTION`] has already bitten; a key smaller than this is
/// not a key.
const MIN_KEY_HEIGHT_PTS: f32 = 18.0;

/// Where the keyboard sits and how big its keys are.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Slab {
    rect: egui::Rect,
    key_h: f32,
    /// Width of one column unit, gaps included.
    unit_w: f32,
}

impl Slab {
    /// The rect of a key `units` wide starting at column offset `col` in row
    /// `row`. Each key is inset by half a [`GAP`] on every side, so the gaps
    /// between neighbours come out even without the caller counting them.
    fn key_rect(&self, row: usize, col: f32, units: f32) -> egui::Rect {
        let x0 = self.rect.left() + PAD + col * self.unit_w + GAP * 0.5;
        let y0 = self.rect.top() + PAD + row as f32 * (self.key_h + GAP);
        egui::Rect::from_min_max(
            egui::pos2(x0, y0),
            egui::pos2(x0 + units * self.unit_w - GAP, y0 + self.key_h),
        )
    }
}

/// Lay the keyboard out against a viewport, given the minimum key height the
/// touch-target rule asks for and (if known) the rect of the field being typed
/// into.
///
/// **The dock edge is chosen by the field, not fixed to the bottom.** A phone
/// panel fills the viewport, and the field can be anywhere in it; we cannot
/// scroll the panel or shrink its layout, because by the time this runs the
/// panel has already composed. So the one lever left is which edge to cover:
/// bottom by default (where a keyboard belongs, and where the map is
/// expendable), flipping to the top when the bottom slab would land on the
/// field. If *both* edges would cover it — a field taller than the free space —
/// bottom wins, because at least the caret is then above the keyboard rather
/// than below it.
///
/// A field that gets out of the way itself says so with [`field_makes_room`],
/// and the caller then passes `None` here: the flip is for fields that cannot
/// move, and for one that can it only puts the keys over what it was lifted
/// to reveal.
fn layout(viewport: egui::Rect, min_key_h: f32, field: Option<egui::Rect>) -> Slab {
    let n = ROW_COUNT;
    let max_h = viewport.height() * MAX_HEIGHT_FRACTION;
    let fits = ((max_h - 2.0 * PAD - (n - 1.0) * GAP) / n).max(MIN_KEY_HEIGHT_PTS);
    let key_h = min_key_h.min(fits).max(MIN_KEY_HEIGHT_PTS);
    let height = 2.0 * PAD + n * key_h + (n - 1.0) * GAP;

    let width = viewport.width();
    let unit_w = (width - 2.0 * PAD) / COLUMNS;

    let bottom = egui::Rect::from_min_size(
        egui::pos2(viewport.left(), viewport.bottom() - EDGE_MARGIN - height),
        egui::vec2(width, height),
    );
    let top = egui::Rect::from_min_size(
        egui::pos2(viewport.left(), viewport.top() + EDGE_MARGIN),
        egui::vec2(width, height),
    );

    let covers = |slab: egui::Rect| field.is_some_and(|f| slab.intersects(f));
    let rect = if covers(bottom) && !covers(top) {
        top
    } else {
        bottom
    };

    Slab {
        rect,
        key_h,
        unit_w,
    }
}

/// How much of the bottom edge the keys will take on this viewport — **whether
/// or not they are currently up**.
///
/// For the one caller that cannot wait to be told. [`layout`] chooses its edge
/// from where the focused field landed *this* frame, flipping to the top when
/// the bottom slab would cover it, and there is no second chance: a floating
/// composer that reserves nothing spends its first frame at the bottom, gets
/// covered, and pushes the keyboard onto the top edge — over everything it was
/// meant to leave visible — for as long as that field keeps focus. Reserving the
/// predicted height keeps the field above the keys on the frame that decides.
///
/// Answers the height of the slab plus its edge inset, in points. The prediction
/// is exact whenever the keys land at the bottom, which is the case it exists
/// for: [`layout`] only varies the slab's *position* with the field, never its
/// size.
#[must_use]
pub fn expected_height(viewport: egui::Rect, ui_scale: f32, display_scale: f32) -> f32 {
    layout(viewport, min_key_height_pts(ui_scale, display_scale), None)
        .rect
        .height()
        + EDGE_MARGIN
}

/// Declare, for this pass, that `field` lifts itself above the keys while they
/// are up, so the keyboard stays on the bottom edge instead of flipping to the
/// top to dodge it.
///
/// For a composer pinned to the bottom of a panel — the chat thread's. It can
/// only lift *after* the keyboard is open (a panel takes its size from the
/// previous frame), so on the frame the field is tapped it still sits where
/// the keys will land. Without this that one frame flips the keyboard to the
/// top, over the conversation, and the flip is re-decided every frame, so the
/// keys stay up there until the field has moved. Call it every frame the field
/// is drawn; the claim lapses with the pass it was made in.
pub fn field_makes_room(ctx: &egui::Context, field: egui::Id) {
    let pass = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| d.insert_temp(makes_room_key(), (field, pass)));
}

fn makes_room_key() -> egui::Id {
    egui::Id::new("onscreen_keyboard_makes_room")
}

/// Has `field` claimed [`field_makes_room`] in the current pass?
fn makes_room(ctx: &egui::Context, field: egui::Id) -> bool {
    ctx.data(|d| d.get_temp::<(egui::Id, u64)>(makes_room_key()))
        .is_some_and(|(id, pass)| id == field && pass == ctx.cumulative_pass_nr())
}

// ─── Resource ─────────────────────────────────────────────────────────────────

/// The on-screen keyboard's whole state.
///
/// A resource rather than a `Local` so the rest of the UI can ask whether the
/// keyboard is up (and, eventually, get out from under it) without this system
/// having to tell it.
#[derive(Resource)]
pub struct OnScreenKeyboard {
    /// Does this platform need us at all ([`platform_wants_keyboard`])? Resolved
    /// once, at construction: it cannot change during a run, and `quality`'s
    /// answer is a `OnceLock` anyway.
    enabled: bool,
    open: bool,
    /// The field we are typing into. Held across the frame in which egui takes
    /// focus away from it (see the module note), which is exactly why the
    /// trigger cannot just be `text_edit_focused()` read fresh every frame.
    target: Option<egui::Id>,
    layer: KeyLayer,
    shift: ShiftState,
    /// Last frame's slab, for [`Self::slab`].
    slab: Option<Slab>,
    /// The fingers on the keys right now ([`Finger`]).
    fingers: Vec<Finger>,
    /// Has this device sent a touch? Once it has, the pointer events the touch
    /// bridge synthesises for the primary finger are duplicates and are ignored.
    touch_seen: bool,
}

/// One finger on the keys, tracked by its own touch id.
///
/// **Why the keys read touches rather than egui clicks.** A key used to type
/// on `Response::clicked()`, and that lost keystrokes in the two ways fast
/// thumbs make most: egui's pointer is one finger (`egui_manager::
/// pointer_events` lets only the first finger down drive it), so a second thumb
/// landing before the first lifted typed nothing; and a finger that drifted
/// more than egui's ~6 pt click distance before lifting was a drag, not a
/// click. A phone keyboard types the key under each finger as it lifts,
/// however it got there — so this does.
#[derive(Clone, Copy, Debug)]
struct Finger {
    id: u64,
    pos: egui::Pos2,
    /// When it went down, in `InputState::time`.
    down_at: f64,
    /// When backspace last repeated under it.
    repeated_at: Option<f64>,
}

/// The pseudo touch id for a mouse driving the keys (`CARTO_OSK` on a desktop).
const MOUSE_FINGER: u64 = u64::MAX;
/// Held this long, backspace starts repeating, every [`REPEAT_EVERY_S`].
const REPEAT_AFTER_S: f64 = 0.45;
const REPEAT_EVERY_S: f64 = 0.06;

impl Default for OnScreenKeyboard {
    fn default() -> Self {
        let enabled = platform_wants_keyboard();
        if enabled {
            tracing::info!("on-screen keyboard enabled (no platform IME, or forced by CARTO_OSK)");
        }
        Self {
            enabled,
            open: false,
            target: None,
            layer: KeyLayer::default(),
            shift: ShiftState::default(),
            slab: None,
            fingers: Vec::new(),
            touch_seen: false,
        }
    }
}

impl OnScreenKeyboard {
    /// Is the keyboard on screen right now?
    pub fn is_open(&self) -> bool {
        self.open && self.enabled
    }

    /// Put the keyboard away without typing anything.
    ///
    /// Exists for Android's Back gesture, which has to dismiss the keyboard
    /// *before* it dismisses anything else — the system IME is the innermost
    /// thing Back closes on every other Android app, and a keyboard you cannot
    /// get rid of without submitting is worse than no keyboard.
    pub fn dismiss(&mut self) {
        self.open = false;
    }

    // There was a `pub fn enabled()` here too — "does this platform get a keyboard
    // at all", as distinct from "is it up right now". Nothing consumed it, and
    // `[workspace.lints.rust] unused = "deny"` makes an accessor without a caller a
    // build failure rather than a warning. It comes back with the thing that wants
    // it: the natural one is a "tap to type" hint on the address bar, shown only
    // where a field is actually typeable.

    /// The rect the keyboard occupied on the last frame it drew — the area a
    /// caller must consider unavailable.
    pub fn slab(&self) -> Option<egui::Rect> {
        self.open.then_some(self.slab).flatten().map(|s| s.rect)
    }

    /// Apply a key press, returning the egui events it produces.
    ///
    /// Pure: the whole state machine (layer, shift, open) is in here and nothing
    /// else, so it can be tested without a `Context`, a `World` or a screen.
    ///
    /// The event shapes are **not** invented here — they are the ones
    /// `egui_manager::begin_egui_frame`'s physical-keyboard bridge already
    /// produces, so a character typed on glass and a character typed on a USB
    /// keyboard are indistinguishable downstream:
    ///
    ///   - a character (including space) is a bare `Event::Text`, with **no**
    ///     accompanying `Event::Key`. That is not an omission: `bevy_key_to_egui`
    ///     maps no letter or digit, so a physical `a` reaches egui as `Text("a")`
    ///     alone, and shift is expressed by the *character* being `A` rather than
    ///     by a modifier. Space is `Text(" ")` for the same reason plus one of
    ///     its own — `Key::Space` is egui's "activate the focused widget", and a
    ///     space bar that occasionally presses a button is a bad space bar.
    ///   - Backspace, Enter and the cursor arrows are `Event::Key` only, since a
    ///     real keyboard's `text` for them is a control character that the bridge
    ///     filters out. Each is emitted as a **press followed by a release** in
    ///     one batch: egui's `key_pressed` reads the event list, so the press
    ///     registers, and the release keeps `keys_down` from accumulating a key
    ///     that no finger is holding.
    fn press(&mut self, cap: Cap) -> Vec<egui::Event> {
        match cap {
            Cap::Char { base, alt } => {
                let ch = if self.shift.alt() { alt } else { base };
                self.shift = self.shift.consumed();
                vec![egui::Event::Text(ch.to_string())]
            }
            Cap::Space => vec![egui::Event::Text(" ".to_owned())],
            Cap::Backspace => key_stroke(egui::Key::Backspace),
            Cap::Enter => key_stroke(egui::Key::Enter),
            // **Shift turns the arrows into a selection**, which is the same
            // bargain a physical keyboard makes and the only one available here:
            // egui extends the selection for an arrow carrying `shift` and moves
            // the caret for one that does not (`text_selection::cursor_range`).
            //
            // Shift is deliberately *not* consumed by an arrow. It is a one-shot
            // for characters because a sentence needs exactly one capital; a
            // selection is grown one press at a time, and a shift that fell off
            // after the first character would mean re-arming it for every letter
            // of the word being selected.
            Cap::Left => key_stroke_with(egui::Key::ArrowLeft, self.selection_modifiers()),
            Cap::Right => key_stroke_with(egui::Key::ArrowRight, self.selection_modifiers()),
            // egui's own select-all chord, so this is the same code path a
            // desktop Ctrl+A takes rather than a second implementation.
            Cap::SelectAll => key_stroke_with(egui::Key::A, egui::Modifiers::COMMAND),
            Cap::Shift => {
                self.shift = self.shift.tapped();
                Vec::new()
            }
            Cap::Layer => {
                self.layer = match self.layer {
                    KeyLayer::Letters => KeyLayer::Symbols,
                    KeyLayer::Symbols => KeyLayer::Letters,
                };
                Vec::new()
            }
            Cap::Hide => {
                self.open = false;
                Vec::new()
            }
        }
    }

    /// What the cursor arrows carry: `SHIFT` while shift is armed (extend the
    /// selection), nothing otherwise (move the caret).
    fn selection_modifiers(&self) -> egui::Modifiers {
        if self.shift.alt() {
            egui::Modifiers::SHIFT
        } else {
            egui::Modifiers::NONE
        }
    }
}

/// A press and its release, in one batch. See [`OnScreenKeyboard::press`].
fn key_stroke(key: egui::Key) -> Vec<egui::Event> {
    key_stroke_with(key, egui::Modifiers::NONE)
}

/// [`key_stroke`] with modifiers held down for it.
fn key_stroke_with(key: egui::Key, modifiers: egui::Modifiers) -> Vec<egui::Event> {
    [true, false]
        .into_iter()
        .map(|pressed| egui::Event::Key {
            key,
            // The bridge in `egui_manager` passes `None` too: Bevy's `KeyCode` is
            // already a physical key, and egui only wants this for games that
            // bind by position.
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        })
        .collect()
}

// ─── Focus ────────────────────────────────────────────────────────────────────

/// Put focus back on the field after a key tap took it away.
///
/// The filter restore is the non-obvious half. `TextEdit` calls
/// `set_focus_lock_filter` with `horizontal_arrows: true, vertical_arrows: true`
/// so that a cursor key edits the text instead of moving focus to the next
/// widget — but that filter lives on the *focus record*, and `request_focus`
/// builds a fresh record with the default (all-false) filter. `Focus::begin_pass`
/// reads the filter at the very top of the next pass, before any widget has run,
/// so a `TextEdit` re-locking it during its own draw is a frame too late. Without
/// this, the first ◀ or ▶ after a restore ejects you from the field.
pub(crate) fn restore_focus(ctx: &egui::Context, id: egui::Id) {
    ctx.memory_mut(|mem| {
        mem.request_focus(id);
        mem.set_focus_lock_filter(
            id,
            egui::EventFilter {
                horizontal_arrows: true,
                vertical_arrows: true,
                tab: false,
                escape: false,
            },
        );
    });
}

// ─── System ───────────────────────────────────────────────────────────────────

/// Draw the on-screen keyboard and feed what it types back into egui.
///
/// **Must be registered after every system that draws a text field** — see the
/// module note; the ordering is what makes the focus restore work at all.
pub fn render_onscreen_keyboard(mut egui: EguiMut, mut kb: ResMut<OnScreenKeyboard>) {
    if !kb.enabled {
        return;
    }

    let ui_scale = egui.ui_scale();
    let display_scale = egui.display_scale();
    // `Context` is an `Arc` handle, so this clone is a refcount bump and it frees
    // the `ResMut` borrow for the `inject_events` call at the end.
    let ctx = egui.ctx_mut().clone();

    let events = kb.pass(&ctx, ui_scale, display_scale);
    if !events.is_empty() {
        egui.inject_events(events);
    }
}

impl OnScreenKeyboard {
    /// One pass of the keyboard: draw it, work out what it typed, and hand back
    /// the events for the caller to inject into the next frame's `RawInput`.
    ///
    /// Split from [`render_onscreen_keyboard`] so the whole open → type → close →
    /// reopen machine can be driven against a real `egui::Context` in a test,
    /// without Bevy. That is not a convenience: everything hard about this module
    /// is an interaction with egui's focus and *layer* state across frames, and
    /// no test of the pure `press` state machine can reach any of it — the bug
    /// that made every reopened keyboard inert lived entirely there.
    pub(crate) fn pass(
        &mut self,
        ctx: &egui::Context,
        ui_scale: f32,
        display_scale: f32,
    ) -> Vec<egui::Event> {
        let kb = self;
        // The trigger. `text_edit_focused()` and not `egui_wants_keyboard_input()`:
        // the latter is `memory.focused().is_some()`, true whenever *any* widget
        // holds focus, which in this client means a keyboard that pops up after
        // clicking a checkbox (`docs/notes/egui-shared-root-ui.md`, and the same
        // trap that once swallowed WASD).
        let text_focus = ctx.text_edit_focused();
        if text_focus {
            kb.open = true;
            kb.target = ctx.memory(|m| m.focused());
        }
        if !kb.open {
            kb.slab = None;
            // So the next open rises again (`draw`'s `motion::presence`).
            crate::systems::motion::forget(ctx, egui::Id::new("onscreen_keyboard_rise"));
            return Vec::new();
        }

        let viewport = ctx.viewport_rect();
        let min_key_h = min_key_height_pts(ui_scale, display_scale);
        // This frame's rect for the field, which we only have because this system
        // is ordered after the panels: `read_response` reads the *current* pass.
        let field = kb
            .target
            .filter(|&id| !makes_room(ctx, id))
            .and_then(|id| ctx.read_response(id))
            .map(|r| r.rect);
        let slab = layout(viewport, min_key_h, field);
        kb.slab = Some(slab);

        // Was this frame's pointer interaction ours? This is what tells "the field
        // lost focus because a key was tapped" (put it back) apart from "the field
        // lost focus because the user went somewhere else" (put the keyboard away),
        // and it is deliberately not `resp.clicked()` on some key: a finger that
        // drifts a few points between touch-down and lift produces a *drag*, not a
        // click, and would otherwise dismiss the keyboard mid-word. `interact_pos`
        // is the right field of the three — it is seeded from `latest_pos` at the
        // top of the pass and survives the `PointerGone` a lifting finger sends,
        // where `press_origin` is already cleared by the release.
        let pointer_on_slab = ctx
            .input(|i| {
                i.pointer
                    .interact_pos()
                    .or_else(|| i.pointer.press_origin())
            })
            .is_some_and(|p| slab.rect.contains(p));

        // Read the state out before drawing: the arms below need `kb` mutably.
        let (layer, shift, target) = (kb.layer, kb.shift, kb.target);
        let caps = track_fingers(ctx, &slab, layer, &mut kb.fingers, &mut kb.touch_seen);
        draw(ctx, &slab, layer, shift, &kb.fingers);
        let mut events = Vec::new();
        for cap in caps {
            events.extend(kb.press(cap));
        }

        if !kb.is_open() {
            kb.fingers.clear();
            // The hide key. Drop the field's focus along with the keyboard, so
            // tapping the same field again is a fresh focus event and brings the
            // keyboard back — leaving it focused would make that one field
            // permanently untypable until you had touched a different one.
            if let Some(id) = target {
                ctx.memory_mut(|m| m.surrender_focus(id));
            }
            kb.target = None;
            kb.slab = None;
            return Vec::new();
        }

        if !text_focus {
            match (pointer_on_slab, target) {
                (true, Some(id)) => restore_focus(ctx, id),
                _ => {
                    kb.open = false;
                    kb.target = None;
                    kb.slab = None;
                    return Vec::new();
                }
            }
        }

        events
    }
}

// ─── Fingers ──────────────────────────────────────────────────────────────────

/// The keyboard's own layer, at the `Order` [`draw`] explains.
fn keyboard_layer() -> egui::LayerId {
    egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("onscreen_keyboard"))
}

/// Each key and its rect, in the layout [`draw`] paints.
fn key_rects(slab: &Slab, layer: KeyLayer) -> Vec<(usize, usize, Cap, egui::Rect)> {
    let mut out = Vec::new();
    for (r, row) in rows(layer).iter().enumerate() {
        let used: f32 = row.iter().map(|k| k.units).sum();
        let mut col = (COLUMNS - used) * 0.5;
        for (c, key) in row.iter().enumerate() {
            out.push((r, c, key.cap, slab.key_rect(r, col, key.units)));
            col += key.units;
        }
    }
    out
}

/// The key a finger at `pos` is on: the one under it, or — in the gaps
/// between keys, which a thumb lands in all the time — the nearest one.
/// `None` off the slab.
fn key_at(slab: &Slab, layer: KeyLayer, pos: egui::Pos2) -> Option<Cap> {
    if !slab.rect.contains(pos) {
        return None;
    }
    key_rects(slab, layer)
        .into_iter()
        .map(|(_, _, cap, rect)| (cap, rect.distance_sq_to_pos(pos)))
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(cap, _)| cap)
}

/// Follow every finger on the keys through this pass's events and answer the
/// keys they typed, in order ([`Finger`] says why this is not `clicked()`).
///
/// A key types as its finger **lifts**, and it is the key under the finger
/// then — a thumb that lands on the edge of `e` and rolls onto `r` types `r`,
/// as on a phone's own keyboard. Backspace is the exception both ways: it
/// types as the finger lands and repeats while it stays, because deleting is
/// the one key people hold.
fn track_fingers(
    ctx: &egui::Context,
    slab: &Slab,
    layer: KeyLayer,
    fingers: &mut Vec<Finger>,
    touch_seen: &mut bool,
) -> Vec<Cap> {
    let (events, now) = ctx.input(|i| (i.events.clone(), i.time));
    if events
        .iter()
        .any(|e| matches!(e, egui::Event::Touch { .. }))
    {
        *touch_seen = true;
    }
    let ours = keyboard_layer();
    let mut caps = Vec::new();
    let mut handle = |id: u64, phase: egui::TouchPhase, pos: egui::Pos2| match phase {
        egui::TouchPhase::Start => {
            // Only where the keyboard is what is drawn on top: a key under a
            // tooltip or a popup is not being pressed.
            let on_top = ctx.layer_id_at(pos).is_none_or(|l| l == ours);
            if !on_top {
                return;
            }
            let Some(cap) = key_at(slab, layer, pos) else {
                return;
            };
            fingers.retain(|f| f.id != id);
            fingers.push(Finger {
                id,
                pos,
                down_at: now,
                repeated_at: None,
            });
            if cap == Cap::Backspace {
                caps.push(cap);
            }
        }
        egui::TouchPhase::Move => {
            if let Some(f) = fingers.iter_mut().find(|f| f.id == id) {
                f.pos = pos;
            }
        }
        egui::TouchPhase::End => {
            let Some(i) = fingers.iter().position(|f| f.id == id) else {
                return;
            };
            fingers.remove(i);
            // Lifted off the slab altogether: the finger changed its mind.
            match key_at(slab, layer, pos) {
                Some(Cap::Backspace) | None => {}
                Some(cap) => caps.push(cap),
            }
        }
        egui::TouchPhase::Cancel => fingers.retain(|f| f.id != id),
    };
    for event in &events {
        match *event {
            egui::Event::Touch { id, phase, pos, .. } => handle(id.0, phase, pos),
            // A mouse (`CARTO_OSK` on a desktop) is one finger. On a touch
            // device these are the bridge's copies of the first finger's
            // touches, already handled above.
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                ..
            } if !*touch_seen => handle(
                MOUSE_FINGER,
                if pressed {
                    egui::TouchPhase::Start
                } else {
                    egui::TouchPhase::End
                },
                pos,
            ),
            egui::Event::PointerMoved(pos) if !*touch_seen => {
                handle(MOUSE_FINGER, egui::TouchPhase::Move, pos);
            }
            _ => {}
        }
    }
    for f in fingers.iter_mut() {
        if key_at(slab, layer, f.pos) != Some(Cap::Backspace) || now - f.down_at < REPEAT_AFTER_S {
            continue;
        }
        let last = f
            .repeated_at
            .unwrap_or(f.down_at + REPEAT_AFTER_S - REPEAT_EVERY_S);
        if now - last >= REPEAT_EVERY_S {
            caps.push(Cap::Backspace);
            f.repeated_at = Some(now);
        }
    }
    // A lift that never arrived (the app went to the background mid-press)
    // must not leave a key held for ever.
    fingers.retain(|f| now - f.down_at < 20.0);
    caps
}

// ─── Painting ─────────────────────────────────────────────────────────────────

/// Draw the slab, with every key a finger is on pressed, and the character
/// under each finger shown enlarged above it.
fn draw(ctx: &egui::Context, slab: &Slab, layer: KeyLayer, shift: ShiftState, fingers: &[Finger]) {
    let id = egui::Id::new("onscreen_keyboard");
    let glyphs = Glyphs::resolve();
    // The slab rises from the edge it docks to, in the time a surface takes to
    // *leave* rather than arrive: it answers a tap in a field, and nobody
    // wants to wait for it. Forgotten on close (`pass`), so every open rises.
    let rise = crate::systems::motion::presence_timed(
        ctx,
        egui::Id::new("onscreen_keyboard_rise"),
        true,
        crate::systems::motion::LEAVE,
        crate::systems::motion::LEAVE,
    );
    let lift = if slab.rect.top() <= 1.0 {
        -slab.rect.height()
    } else {
        slab.rect.height()
    };

    egui::Area::new(id)
        // **Above `Foreground`, where the rest of the app's chrome lives, and
        // that is not a preference — it is the only place a layer that comes and
        // goes can survive.**
        //
        // `move_to_top` does not mean "put me on top now". It sets a flag that
        // `Areas::end_pass` consults in a *stable* sort by `(order,
        // wants_to_be_on_top)`, so two layers that both raise themselves every
        // frame keep whatever relative order they already had — the call order
        // within the frame decides nothing. A layer that stops drawing loses the
        // flag for that frame, sorts below everyone who kept it, and can never
        // climb back: re-raising only restores the tie, and a stable sort leaves
        // a tie where it found it.
        //
        // The keyboard is exactly that kind of layer, and the sign-in card is
        // exactly that kind of rival: a full-viewport dim backdrop plus the card,
        // both `Foreground`, both raised every frame. So the first open worked and
        // every reopen after it drew a keyboard whose taps went to the backdrop
        // underneath — keys that paint, highlight nothing and type nothing, with
        // the rest of the UI perfectly responsive. Being in a *higher* order is
        // what makes this unconditional: no `Foreground` layer can outrank us
        // however it was created, including one opened after we were.
        //
        // `move_to_top` stays anyway, for the same contest among `Tooltip` layers
        // (egui puts hover text there, and a long touch produces hover text).
        .order(egui::Order::Tooltip)
        .fixed_pos(slab.rect.min)
        // We own the geometry; letting egui pull it back inside the content rect
        // would fight the top/bottom flip `layout` just made.
        .constrain(false)
        .show(ctx, |ui| {
            crate::systems::motion::fade(ui, rise);
            ui.set_clip_rect(slab.rect.expand(2.0));
            let visuals = ui.visuals().clone();
            let radius = visuals.widgets.inactive.corner_radius;

            // The slab, in the frame every box floating over the map wears. Drawn
            // by hand rather than through `Frame::show` because the key rects are
            // absolute — `layout` had to know the outer size to decide which edge
            // to dock to, so there is nothing left for a layout pass to do.
            let frame = theme::overlay_frame();
            ui.painter().rect(
                slab.rect,
                frame.corner_radius,
                frame.fill,
                frame.stroke,
                egui::StrokeKind::Inside,
            );

            // A click-sensing backdrop over the whole slab, allocated **first** so
            // every key (allocated after, and therefore on top — egui's hit test
            // picks the last widget in a tie) still wins its own press.
            //
            // This is not decoration. The world-click bridge emulates a mouse from
            // the primary finger and gates each consumer on
            // `egui_wants_pointer_input()`, which is `is_using_pointer()` while a
            // button is down — true only if *some* egui widget claimed the press.
            // A tap landing in the gap between two keys would otherwise fall
            // through to picking, the gizmo or the globe's zoom-to-cursor, i.e.
            // typing would move the map.
            // unnamed: the backdrop only claims taps between keys; it does nothing.
            let _backdrop = ui.allocate_rect(slab.rect, egui::Sense::click());

            let held: Vec<Cap> = fingers
                .iter()
                .filter_map(|f| key_at(slab, layer, f.pos))
                .collect();
            for (r, c, cap, rect) in key_rects(slab, layer) {
                // Still a widget per key, sensing clicks, so the primary finger's
                // press is claimed by egui and never reaches the map. *What it
                // typed* comes from [`track_fingers`]. An explicit id per slot,
                // not `allocate_rect`'s auto-id: the auto-ids shift by one every
                // time the layer swaps to a row with a different key count.
                let key = ui.interact(rect, id.with((r, c)), egui::Sense::click());
                let _ = theme::named(ui, key, &cap_name(cap, shift.alt()));
                paint_key(
                    ui,
                    rect,
                    radius,
                    cap,
                    shift,
                    &glyphs,
                    held.contains(&cap),
                    &visuals,
                );
            }
        });

    // The phone keyboard's key preview: the character under each finger,
    // enlarged above it, because the thumb is covering the key itself. Painted
    // on the top layer so it can rise past the slab's own edge.
    let preview = ctx.layer_painter(egui::LayerId::new(egui::Order::Debug, id.with("preview")));
    for f in fingers {
        let Some(cap) = key_at(slab, layer, f.pos) else {
            continue;
        };
        let Some((_, _, _, rect)) = key_rects(slab, layer).into_iter().find(|k| k.2 == cap) else {
            continue;
        };
        let Cap::Char { base, alt } = cap else {
            continue;
        };
        let ch = if shift.alt() { alt } else { base };
        let size = egui::vec2(rect.width().max(30.0) * 1.35, rect.height() * 1.15);
        let bubble = egui::Rect::from_center_size(
            egui::pos2(rect.center().x, rect.top() - 6.0 - size.y / 2.0),
            size,
        )
        .translate(egui::vec2(
            // Kept on screen at the outer columns.
            (slab.rect.left() + 2.0 - (rect.center().x - size.x / 2.0)).max(0.0)
                - ((rect.center().x + size.x / 2.0) - (slab.rect.right() - 2.0)).max(0.0),
            0.0,
        ));
        let radius = egui::CornerRadius::same(theme::RADIUS);
        preview.add(
            egui::epaint::Shadow {
                offset: [0, 4],
                blur: 14,
                spread: 0,
                color: egui::Color32::from_black_alpha(120),
            }
            .as_shape(bubble, radius),
        );
        preview.rect_filled(bubble, radius, theme::p().cream);
        preview.text(
            bubble.center(),
            egui::Align2::CENTER_CENTER,
            ch,
            egui::FontId::proportional(size.y * 0.55),
            theme::p().ink,
        );
    }

    // Keep the keyboard above everything else at this `Order`, every frame.
    // The picture rises; the keys' hit boxes are where they will rest from the
    // first frame, so a finger already on its way to one lands on it.
    crate::systems::motion::shift_painted(
        ctx,
        egui::LayerId::new(egui::Order::Tooltip, id),
        rise,
        lift,
    );
    ctx.move_to_top(egui::LayerId::new(egui::Order::Tooltip, id));
}

/// The symbols on the non-character keys, out of the bundled subset
/// ([`crate::systems::icons`]).
///
/// These were preference ladders — `["⌫", "⌦", "del"]` — because egui's own font
/// stack has no `⌫`, no `⇧` and no `←`, and the fallback on the one control the
/// user cannot get past had to be a word. The icon font draws all six, so the
/// struct is a plain table now; it stays a struct so the key layout keeps
/// reading `g.backspace` rather than naming a module in every cell.
struct Glyphs {
    backspace: &'static str,
    enter: &'static str,
    shift: &'static str,
    hide: &'static str,
    left: &'static str,
    right: &'static str,
}

impl Glyphs {
    fn resolve() -> Self {
        Self {
            backspace: icons::BACKSPACE,
            enter: icons::ENTER,
            shift: icons::SHIFT,
            hide: icons::HIDE_KEYBOARD,
            left: icons::CHEVRON_LEFT,
            right: icons::CHEVRON_RIGHT,
        }
    }
}

/// Paint one key.
///
/// Every colour comes off the ambient `Visuals`, which is `theme::style`'s — so
/// a key at rest is a raised surface with the theme's hairline, a key under a
/// finger is the same pressed fill the rest of the app uses for "this is the live
/// thing", and none of it can drift from the palette without the palette moving.
#[allow(clippy::too_many_arguments)]
fn paint_key(
    ui: &egui::Ui,
    rect: egui::Rect,
    radius: egui::CornerRadius,
    cap: Cap,
    shift: ShiftState,
    glyphs: &Glyphs,
    down: bool,
    visuals: &egui::Visuals,
) {
    let active = &visuals.widgets.active;

    // Three resting looks, and the split is by role rather than by taste:
    // characters are what you are aiming at, so they get the lighter surface;
    // the modifier keys sit back on the panel colour; enter is the one
    // committing action and wears the accent it shares with every other "do it"
    // in the app.
    let (fill, stroke, ink) = if down {
        (
            active.weak_bg_fill,
            active.bg_stroke.color,
            theme::p().cream,
        )
    } else {
        match cap {
            Cap::Enter => (
                theme::p().accent_muted,
                theme::p().accent_bright,
                theme::p().cream,
            ),
            // A held state, drawn the way every held state in the app is: the
            // engaged plate, plus the brightest rim. Caps lock then reads as the
            // same kind of thing as a north-up lock, and as a clear step past
            // `ShiftState::Once` below, which only gets the pressed fill.
            Cap::Shift if shift == ShiftState::Lock => (
                theme::p().accent,
                theme::p().accent_bright,
                theme::p().on_accent,
            ),
            Cap::Shift if shift == ShiftState::Once => (
                theme::p().accent_muted,
                theme::p().accent_bright,
                theme::p().cream,
            ),
            Cap::Char { .. } => (
                visuals.widgets.inactive.bg_fill,
                visuals.widgets.inactive.bg_stroke.color,
                visuals.widgets.inactive.fg_stroke.color,
            ),
            _ => (theme::p().surface, theme::p().line, theme::p().text_dim),
        }
    };

    let p = ui.painter();
    p.rect(
        rect,
        radius,
        fill,
        egui::Stroke::new(1.0, stroke),
        egui::StrokeKind::Inside,
    );

    let font = egui::TextStyle::Button.resolve(ui.style());
    let label = match cap {
        Cap::Char { base, alt } => {
            let ch = if shift.alt() { alt } else { base };
            ch.to_string()
        }
        // The space bar is a bar. A word ("space") is a translation problem and a
        // legibility problem at this size; the bar is neither.
        Cap::Space => {
            let bar = rect.shrink2(egui::vec2(rect.width() * 0.28, rect.height() * 0.45));
            p.rect_filled(bar, egui::CornerRadius::same(1), ink);
            return;
        }
        // Drawn, not lettered: "All" is a translation problem, `⌘A` names a key
        // this device does not have, and the bundled font's symbol coverage is
        // too thin to gamble a glyph on (`systems::icons`). A selection
        // band across a line of text is what the key *does*, at any size.
        Cap::SelectAll => {
            let band = rect.shrink2(egui::vec2(rect.width() * 0.22, rect.height() * 0.34));
            p.rect_filled(band, egui::CornerRadius::same(2), theme::p().accent_muted);
            let line = band.shrink2(egui::vec2(band.width() * 0.12, band.height() * 0.36));
            p.rect_filled(line, egui::CornerRadius::same(1), ink);
            return;
        }
        Cap::Backspace => glyphs.backspace.to_owned(),
        Cap::Enter => glyphs.enter.to_owned(),
        Cap::Shift => glyphs.shift.to_owned(),
        Cap::Hide => glyphs.hide.to_owned(),
        Cap::Left => glyphs.left.to_owned(),
        Cap::Right => glyphs.right.to_owned(),
        // Plain ASCII, and the only label that says what the *other* layer is
        // rather than what this key is — which is how every phone keyboard reads.
        Cap::Layer => "123".to_owned(),
    };
    p.text(rect.center(), egui::Align2::CENTER_CENTER, label, font, ink);
}

/// A key's name for egui's widget info (`theme::named`): the character it
/// types, or in words what it does — the drawn keys carry no text of their own.
fn cap_name(cap: Cap, alt: bool) -> String {
    match cap {
        Cap::Char { base, alt: shifted } => (if alt { shifted } else { base }).to_string(),
        Cap::Space => "Space".into(),
        Cap::Backspace => "Backspace".into(),
        Cap::Enter => "Enter".into(),
        Cap::Shift => "Shift".into(),
        Cap::Layer => "123".into(),
        Cap::Hide => "Hide the keyboard".into(),
        Cap::Left => "Left".into(),
        Cap::Right => "Right".into(),
        Cap::SelectAll => "Select all".into(),
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
