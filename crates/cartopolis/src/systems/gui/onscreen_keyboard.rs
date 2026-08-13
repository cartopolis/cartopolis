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
use crate::systems::theme;

// ─── Platform predicate ───────────────────────────────────────────────────────

/// Material's minimum touch target, in dp (density-independent pixels, 1/160 in
/// — about 7.6 mm, which is roughly a fingertip). `theme::style` sets
/// `interact_size` to 40×24 pt, and 24 pt is ~36 dp on the phone tier: fine for
/// a row you press once, too small for a key you press forty times in a row.
const MIN_TOUCH_DP: f32 = 48.0;

/// `index.html`'s `MAX_DPR` on the phone tier — the device pixels per CSS pixel
/// the render budget is willing to draw (`docs/notes/wasm-performance.md`). It
/// appears here because it is *also* the magnification the stage transform
/// applies to every egui point on the web, which is the thing this module needs
/// in order to know how big a point physically is. Duplicated rather than
/// plumbed because the page decides it before any Rust exists; if it changes
/// there, `dp_per_point`'s arithmetic changes with it.
const WEB_PHONE_MAX_DPR: f32 = 1.5;

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

/// How many dp one egui point covers, which is the only way to size a key in a
/// physical unit from inside this process.
///
/// The chain, per platform:
///
///   - **Native (and Android).** winit's scale factor *is* the display density,
///     so one window logical point is exactly one dp, and an egui point is
///     `ui_scale` window points ([`EguiManager::window_to_ui`]). → `ui_scale`.
///   - **Web.** A CSS pixel is a dp on a phone (a Fairphone 4 reports 393 CSS px
///     for 1080 device px at density 2.75), but the render budget's stage
///     transform sizes the canvas to viewport ÷ k and scales it back up by k, so
///     one *window* point covers k CSS px. → `ui_scale · k`.
///
/// `k` is `max(1, dpr / MAX_DPR, √(pixel budget))` in `index.html`, and only the
/// middle term is reconstructible from here. Using it alone therefore
/// **under**-estimates k, which over-estimates the number of points in 48 dp and
/// so makes the keys slightly *larger* than the guideline rather than smaller —
/// the safe direction for a floor, and the reason this is not plumbed through
/// from JS for a number that is already only accurate to a rounding.
fn dp_per_point(ui_scale: f32, display_scale: f32, web_phone: bool) -> f32 {
    let stage_k = if web_phone {
        (display_scale / WEB_PHONE_MAX_DPR).max(1.0)
    } else {
        1.0
    };
    (ui_scale * stage_k).max(0.1)
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
/// (`theme::first_drawable` documents how thin), and a `€` or a `¿` that renders
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
    slab: Option<egui::Rect>,
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
        self.open.then_some(self.slab).flatten()
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
fn restore_focus(ctx: &egui::Context, id: egui::Id) {
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

    // The trigger. `text_edit_focused()` and not `egui_wants_keyboard_input()`:
    // the latter is `memory.focused().is_some()`, true whenever *any* widget
    // holds focus, which in this client means a keyboard that pops up after
    // clicking a checkbox (CLAUDE.md, and the same trap that once swallowed WASD).
    let text_focus = ctx.text_edit_focused();
    if text_focus {
        kb.open = true;
        kb.target = ctx.memory(|m| m.focused());
    }
    if !kb.open {
        kb.slab = None;
        return;
    }

    let viewport = ctx.viewport_rect();
    let min_key_h = MIN_TOUCH_DP
        / dp_per_point(
            ui_scale,
            display_scale,
            cfg!(target_arch = "wasm32") && crate::quality::is_phone(),
        );
    // This frame's rect for the field, which we only have because this system is
    // ordered after the panels: `read_response` reads the *current* pass.
    let field = kb
        .target
        .and_then(|id| ctx.read_response(id))
        .map(|r| r.rect);
    let slab = layout(viewport, min_key_h, field);
    kb.slab = Some(slab.rect);

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

    // Read the state out before drawing: a `ResMut` deref inside a `match`
    // scrutinee lives until the end of the match, and the arms need it mutably.
    let (layer, shift, target) = (kb.layer, kb.shift, kb.target);
    let events = match draw(&ctx, &slab, layer, shift) {
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
        return;
    }

    if !text_focus {
        match (pointer_on_slab, target) {
            (true, Some(id)) => restore_focus(&ctx, id),
            _ => {
                kb.open = false;
                kb.target = None;
                kb.slab = None;
                return;
            }
        }
    }

    if !events.is_empty() {
        egui.inject_events(events);
    }
}

// ─── Painting ─────────────────────────────────────────────────────────────────

/// Draw the slab and return the key that was tapped this frame, if any.
fn draw(ctx: &egui::Context, slab: &Slab, layer: KeyLayer, shift: ShiftState) -> Option<Cap> {
    let id = egui::Id::new("onscreen_keyboard");
    let glyphs = Glyphs::resolve(ctx);
    let rows = rows(layer);

    let inner = egui::Area::new(id)
        // Above the panels (Background) and the pins (Middle), and level with the
        // address bar and the sign-in card — which is why `move_to_top` below is
        // not optional: within one `Order`, layers stack in the order they were
        // last raised, and the sign-in card raises itself every frame.
        .order(egui::Order::Foreground)
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
    ctx.move_to_top(egui::LayerId::new(egui::Order::Foreground, id));
    inner.inner
}

/// The glyphs the bundled font can actually draw for the non-character keys.
///
/// egui ships Ubuntu-Light plus a thin emoji subset, and `theme::first_drawable`
/// documents how thin: no `⌫`, no `⇧`, no `←`. Every list here ends in something
/// Latin-1 (or plain words) so a missing glyph degrades to a legible label
/// instead of a tofu box on the one control the user cannot get past.
struct Glyphs {
    backspace: &'static str,
    enter: &'static str,
    shift: &'static str,
    hide: &'static str,
    left: &'static str,
    right: &'static str,
}

impl Glyphs {
    fn resolve(ctx: &egui::Context) -> Self {
        Self {
            backspace: theme::first_drawable(ctx, &["⌫", "⌦", "del"]),
            enter: theme::first_drawable(ctx, &["⏎", "↵", "OK"]),
            shift: theme::first_drawable(ctx, &["⇧", "⏶", "^"]),
            hide: theme::first_drawable(ctx, &["⌄", "⏷", "v"]),
            left: theme::first_drawable(ctx, &["◀", "⏴", "<"]),
            right: theme::first_drawable(ctx, &["▶", "⏵", ">"]),
        }
    }
}

/// Paint one key.
///
/// Every colour comes off the ambient `Visuals`, which is `theme::style`'s — so
/// a key at rest is a raised surface with the theme's hairline, a key under a
/// finger is the same accent orange the rest of the app uses for "this is the live
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
            // A held state, which is what `AMBER` is reserved for across the app
            // (north-up lock, follow-and-rotate) — so caps lock reads as the same
            // kind of thing, and never collides with the accent.
            Cap::Shift if shift == ShiftState::Lock => (theme::AMBER, theme::AMBER, theme::INK),
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
        // too thin to gamble a glyph on (`theme::first_drawable`). A selection
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

    /// 48 dp has to become a number of points, and the conversion is different on
    /// each platform because the *magnification* is: Android magnifies with
    /// `ui_scale`, the web with `index.html`'s stage transform. Getting this
    /// backwards is how a key ends up 3.8 mm tall on the one platform that has no
    /// other keyboard.
    #[test]
    fn a_key_is_at_least_48_dp_tall_on_both_phone_platforms() {
        // Android: ui_scale 1.5, no stage transform.
        let android = dp_per_point(1.5, 2.75, false);
        assert!((android - 1.5).abs() < 1e-6);
        assert!((MIN_TOUCH_DP / android - 32.0).abs() < 0.01);

        // Web phone tier: ui_scale 1.0, k ≈ dpr / 1.5 = 1.83 on a Fairphone 4.
        let web = dp_per_point(1.0, 2.75, true);
        assert!((web - 2.75 / 1.5).abs() < 1e-6);
        assert!(MIN_TOUCH_DP / web > 25.0 && MIN_TOUCH_DP / web < 27.0);

        // A desktop forced on with `?osk`: a point is a point.
        assert!((dp_per_point(1.0, 1.0, false) - 1.0).abs() < 1e-6);
    }

    /// The phone this was sized for: 274 × 609 pt of egui viewport. The keyboard
    /// has to fit inside it, keep its 48 dp of key height, and leave most of the
    /// screen alone.
    #[test]
    fn the_keyboard_fits_a_phone_viewport() {
        let vp = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(274.0, 609.0));
        let min_key_h = MIN_TOUCH_DP / dp_per_point(1.5, 2.75, false);
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
