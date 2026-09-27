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
//!     registration note in `lib.rs`. It is also what makes `read_response` on
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
//! time we run. That also keeps it *out* of the Phase 7b `.chain()`, which is
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

use bevy::prelude::*;

use crate::systems::egui_manager::EguiManager;
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
}

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
pub fn render_onscreen_keyboard(mut egui: ResMut<EguiManager>, mut kb: ResMut<OnScreenKeyboard>) {
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
        // clicking a checkbox (CLAUDE.md, and the same trap that once swallowed
        // WASD).
        let text_focus = ctx.text_edit_focused();
        if text_focus {
            kb.open = true;
            kb.target = ctx.memory(|m| m.focused());
        }
        if !kb.open {
            kb.slab = None;
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
        let events = match draw(ctx, &slab, layer, shift) {
            Some(cap) => kb.press(cap),
            None => Vec::new(),
        };

        if !kb.is_open() {
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

// ─── Painting ─────────────────────────────────────────────────────────────────

/// Draw the slab and return the key that was tapped this frame, if any.
fn draw(ctx: &egui::Context, slab: &Slab, layer: KeyLayer, shift: ShiftState) -> Option<Cap> {
    let id = egui::Id::new("onscreen_keyboard");
    let glyphs = Glyphs::resolve();
    let rows = rows(layer);

    let inner = egui::Area::new(id)
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
            let _backdrop = ui.allocate_rect(slab.rect, egui::Sense::click());

            let mut hit = None;
            for (r, row) in rows.iter().enumerate() {
                let used: f32 = row.iter().map(|k| k.units).sum();
                let mut col = (COLUMNS - used) * 0.5;
                for (c, key) in row.iter().enumerate() {
                    let rect = slab.key_rect(r, col, key.units);
                    col += key.units;
                    // An explicit id per slot, not `allocate_rect`'s auto-id: the
                    // auto-ids shift by one every time the layer swaps to a row
                    // with a different key count, and a widget whose id changes
                    // between frames loses whatever interaction state it had
                    // mid-press (the same trap `bookmarks::rename_row` documents).
                    let resp = ui.interact(rect, id.with((r, c)), egui::Sense::click());
                    paint_key(ui, rect, radius, key.cap, shift, &glyphs, &resp, &visuals);
                    if resp.clicked() {
                        hit = Some(key.cap);
                    }
                }
            }
            hit
        });

    // Keep the keyboard above everything else at this `Order`, every frame.
    ctx.move_to_top(egui::LayerId::new(egui::Order::Tooltip, id));
    inner.inner
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
    resp: &egui::Response,
    visuals: &egui::Visuals,
) {
    let down = resp.is_pointer_button_down_on();
    let active = &visuals.widgets.active;

    // Three resting looks, and the split is by role rather than by taste:
    // characters are what you are aiming at, so they get the lighter surface;
    // the modifier keys sit back on the panel colour; enter is the one
    // committing action and wears the accent it shares with every other "do it"
    // in the app.
    let (fill, stroke, ink) = if down {
        (active.weak_bg_fill, active.bg_stroke.color, theme::CREAM)
    } else {
        match cap {
            Cap::Enter => (theme::ACCENT_MUTED, theme::ACCENT_BRIGHT, theme::CREAM),
            // A held state, drawn the way every held state in the app is: the
            // engaged plate, plus the brightest rim. Caps lock then reads as the
            // same kind of thing as a north-up lock, and as a clear step past
            // `ShiftState::Once` below, which only gets the pressed fill.
            Cap::Shift if shift == ShiftState::Lock => {
                (theme::ACCENT, theme::ACCENT_BRIGHT, theme::INK)
            }
            Cap::Shift if shift == ShiftState::Once => {
                (theme::ACCENT_MUTED, theme::ACCENT_BRIGHT, theme::CREAM)
            }
            Cap::Char { .. } => (
                visuals.widgets.inactive.bg_fill,
                visuals.widgets.inactive.bg_stroke.color,
                visuals.widgets.inactive.fg_stroke.color,
            ),
            _ => (theme::SURFACE, theme::LINE, theme::TEXT_DIM),
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
            p.rect_filled(band, egui::CornerRadius::same(2), theme::ACCENT_MUTED);
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

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
                        let (_, resp) = ui
                            .allocate_exact_size(self.screen.size(), egui::Sense::click_and_drag());
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
                            let resp = ui
                                .add(egui::TextEdit::singleline(text).desired_width(f32::INFINITY));
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
            let _ = self.ctx.end_pass();
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

    fn kb() -> OnScreenKeyboard {
        OnScreenKeyboard {
            enabled: true,
            open: true,
            target: None,
            layer: KeyLayer::default(),
            shift: ShiftState::default(),
            slab: None,
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
            let _ = ctx.run_ui(
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
}
