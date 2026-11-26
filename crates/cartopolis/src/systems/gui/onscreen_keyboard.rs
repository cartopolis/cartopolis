//! An in-app on-screen keyboard, because two of our platforms have no IME.
//!
//! Android's NativeActivity brings up no soft keyboard at all (there is no Java
//! `InputMethodManager` on the other end of it — `docs/notes/android-build.md`),
//! so every text field in the client can be *focused* on the phone and cannot be
//! *typed into*. That is not a rough edge on a side feature: it is the address
//! bar and the geo search, i.e. the front door of a routing client, plus bookmark
//! rename, nearby chat and the assistant.
//!
//! This module is the keyboard's state and its input: whether the keys are up
//! (a library text field has the focus — `UiInput::overlay_owns_keyboard`),
//! where they stand, which fingers are on them and what they typed. The keys
//! are drawn by the component library (`lib_keyboard`), which also hands what
//! was typed to the focused field (`mithril_bevy::KeyEdits`) and tells this
//! module where that field is, so the keys can get off it.
//!
//! ## Two things about this are not obvious
//!
//! **The keys read fingers, not clicks.** A key that typed on a click lost
//! keystrokes in the two ways fast thumbs make most: a pointer is one finger, so
//! a second thumb landing before the first lifted typed nothing; and a finger
//! that drifted a few points before lifting was a drag, not a click. A phone
//! keyboard types the key under each finger as it lifts, however it got there —
//! so this reads Bevy's `Touches` ([`Finger`], [`OnScreenKeyboard::track`]).
//! A click on a key with no finger behind it (the MCP server's `press`) still
//! types ([`OnScreenKeyboard::clicked`]).
//!
//! **The edge is decided once per field, a few frames after it takes the
//! focus.** The keys dock to the bottom, and flip to the top when they would
//! cover the field being typed into. But a library screen with a field at its
//! foot lifts above the keys by [`KeyboardInset`] — which is zero while the keys
//! are at the top — so a flip decided every frame would see the lifted field
//! clear, drop back, see it covered, and flip again. So the keys open at the
//! bottom, let the screens lift for [`SETTLE_FRAMES`], then decide where the
//! field ended up and keep that edge until the focus moves.
//!
//! ## What it does not do
//!
//! No IME, no prediction, no autocorrect, no long-press accents, no clipboard
//! keys. Nothing here is a substitute for a platform IME; it is what makes a
//! text field usable at all on a platform that has none.

// The keys' half — drawing them, reading what they typed — is `lib_keyboard`,
// which only a `ui-probe` build has; without it the keyboard is never enabled
// ([`platform_wants_keyboard`]) and that half's accessors have no caller.
#![cfg_attr(not(feature = "ui-probe"), allow(dead_code))]

use bevy::input::mouse::MouseButtonInput;
use bevy::input::touch::Touches;
use bevy::input::ButtonState;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::systems::theme;
use crate::systems::ui_input::{UiInput, UiMetrics};

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
/// the one detector of "coarse pointer on a small viewport" in this client
/// (`docs/notes/touch-input.md`).
///
/// Android is unconditional and does not consult the tier, because the reason
/// there is not ergonomics but the total absence of an IME.
///
/// Only a build that can draw the keys has them: they are the component
/// library's on `bevy_ui` (`ui-probe`). The DOM backend's fields are the
/// browser's own, which bring its IME.
pub fn platform_wants_keyboard() -> bool {
    cfg!(feature = "ui-probe")
        && resolve_enabled(
            crate::env_flag("CARTO_OSK"),
            crate::env_value("CARTO_OSK").is_some(),
            cfg!(target_os = "android"),
            cfg!(target_arch = "wasm32") && crate::quality::is_phone(),
        )
}

/// The shortest a key may be, in UI points — Material's 48 dp minimum touch
/// target converted through [`theme::dp_per_point`]. A key is held to the floor
/// a row is held to, because a key is pressed forty times in a row where a row
/// is pressed once.
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
    pub fn alt(self) -> bool {
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
pub enum Cap {
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
    /// Put the keyboard away, and the field's focus with it
    /// ([`OnScreenKeyboard::take_blur`]).
    Hide,
    Left,
    Right,
    /// Select the whole field.
    ///
    /// **The one editing verb a finger cannot otherwise reach.** Selecting text
    /// with a pointer is a press-drag-release across the glyphs, and on this
    /// client a finger held still on a field is the long press that opens the
    /// world context menu (`systems::touch`). So "replace what is in this field"
    /// — which on an address bar or a search box is *most* of the editing anyone
    /// does — had no gesture at all.
    SelectAll,
}

/// What a key typed, for the focused field. The library's `KeyEdit` in all but
/// name (`lib_keyboard` maps one onto the other): this module does not link
/// the library, so it can be tested and built without it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Stroke {
    /// Characters, at the caret (over the selection).
    Text(String),
    Backspace,
    /// The field's Enter: a newline in a textarea, a submit in a line.
    Enter,
    /// The caret a character left or right; `select` extends the selection.
    Left {
        select: bool,
    },
    Right {
        select: bool,
    },
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
/// [`layout`] has to know the height before [`rows`] has been asked for anything.
const ROW_COUNT: f32 = 4.0;

const LETTER_ROWS: [&str; 3] = ["qwertyuiop", "asdfghjkl", "zxcvbnm"];

/// The symbol layer, as (base, alternate) pairs by position. ASCII throughout,
/// so no key can come out as an empty box in a font without it.
const SYMBOL_ROWS: [(&str, &str); 3] = [
    ("1234567890", "!@#$%^&*()"),
    ("-/:();&@\"", "_\\|[]{}~'"),
    (".,?!'+=", "><\\/`{}"),
];

/// The rows for a given layer, top to bottom.
///
/// **Shift is not a parameter**, and that is deliberate: a key carries both its
/// forms, so the geometry is identical in every shift state and a key can never
/// move under a finger that is already on its way down.
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

/// The key at `row`, `col` on `layer` (the same slot on either layer is the
/// same size, so the drawn keys keep their place across a layer swap).
pub fn cap_at(layer: KeyLayer, row: usize, col: usize) -> Option<Cap> {
    rows(layer).get(row)?.get(col).map(|k| k.cap)
}

/// A key's accessible name: the character it types, or in words what it does
/// — the drawn keys carry no text of their own for most of them.
pub fn cap_name(cap: Cap, alt: bool) -> String {
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

// ─── Geometry ─────────────────────────────────────────────────────────────────
//
// All in UI points (the library's px), y down from the viewport's top-left.

/// Gap between two keys: the app's 6 px.
const GAP: f32 = 6.0;
/// Padding between the slab's edge and the outermost keys.
const PAD: f32 = 6.0;
/// Gap between the slab and the edge of the viewport it is docked to.
const EDGE_MARGIN: f32 = 6.0;
/// Ceiling on the slab, as a fraction of the viewport height. A keyboard that
/// eats more than this of a phone screen leaves nothing to type *into*; below
/// this the 48 dp key wins, above it the viewport does.
const MAX_HEIGHT_FRACTION: f32 = 0.45;
/// Absolute floor on a key: a touch row's 40. Only reachable on a viewport
/// short enough that [`MAX_HEIGHT_FRACTION`] has already bitten.
const MIN_KEY_HEIGHT_PTS: f32 = 40.0;

/// Which edge the keys are docked to.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Edge {
    #[default]
    Bottom,
    Top,
}

/// Where the keyboard sits and how big its keys are.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Slab {
    rect: Rect,
    key_h: f32,
    /// Width of one column unit, gaps included.
    unit_w: f32,
}

impl Slab {
    /// The rect of a key `units` wide starting at column offset `col` in row
    /// `row`. Each key is inset by half a [`GAP`] on every side, so the gaps
    /// between neighbours come out even without the caller counting them.
    fn key_rect(&self, row: usize, col: f32, units: f32) -> Rect {
        let x0 = self.rect.min.x + PAD + col * self.unit_w + GAP * 0.5;
        let y0 = self.rect.min.y + PAD + row as f32 * (self.key_h + GAP);
        Rect::new(x0, y0, x0 + units * self.unit_w - GAP, y0 + self.key_h)
    }
}

/// Lay the keyboard out against a viewport, docked to `edge`, given the
/// minimum key height the touch-target rule asks for.
fn layout(viewport: Rect, min_key_h: f32, edge: Edge) -> Slab {
    let n = ROW_COUNT;
    let max_h = viewport.height() * MAX_HEIGHT_FRACTION;
    let fits = ((max_h - 2.0 * PAD - (n - 1.0) * GAP) / n).max(MIN_KEY_HEIGHT_PTS);
    let key_h = min_key_h.min(fits).max(MIN_KEY_HEIGHT_PTS);
    let height = 2.0 * PAD + n * key_h + (n - 1.0) * GAP;

    let width = viewport.width();
    let unit_w = (width - 2.0 * PAD) / COLUMNS;

    let top = match edge {
        Edge::Bottom => viewport.max.y - EDGE_MARGIN - height,
        Edge::Top => viewport.min.y + EDGE_MARGIN,
    };
    Slab {
        rect: Rect::new(viewport.min.x, top, viewport.min.x + width, top + height),
        key_h,
        unit_w,
    }
}

/// The edge for a field at `field`: the bottom (where a keyboard belongs, and
/// where the map is expendable), flipping to the top when the bottom slab would
/// cover the field. If *both* edges would cover it — a field taller than the
/// free space — the bottom wins, because at least the caret is then above the
/// keyboard rather than below it.
fn choose_edge(viewport: Rect, min_key_h: f32, field: Option<Rect>) -> Edge {
    let covers = |edge: Edge| {
        let slab = layout(viewport, min_key_h, edge).rect;
        field.is_some_and(|f| !slab.intersect(f).is_empty())
    };
    if covers(Edge::Bottom) && !covers(Edge::Top) {
        Edge::Top
    } else {
        Edge::Bottom
    }
}

/// Each key and its rect.
fn key_rects(slab: &Slab, layer: KeyLayer) -> Vec<(usize, usize, Cap, Rect)> {
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

/// Squared distance from `p` to `rect` (zero inside it).
fn distance_sq(rect: Rect, p: Vec2) -> f32 {
    let dx = (rect.min.x - p.x).max(0.0).max(p.x - rect.max.x);
    let dy = (rect.min.y - p.y).max(0.0).max(p.y - rect.max.y);
    dx * dx + dy * dy
}

/// The key a finger at `pos` is on: the one under it, or — in the gaps
/// between keys, which a thumb lands in all the time — the nearest one.
/// `None` off the slab.
fn key_at(slab: &Slab, layer: KeyLayer, pos: Vec2) -> Option<(usize, usize, Cap, Rect)> {
    if !slab.rect.contains(pos) {
        return None;
    }
    key_rects(slab, layer)
        .into_iter()
        .min_by(|a, b| distance_sq(a.3, pos).total_cmp(&distance_sq(b.3, pos)))
}

/// How much of the bottom edge the keys will take on this viewport — **whether
/// or not they are currently up** — in UI points: the slab plus its edge inset.
///
/// For the one caller that cannot wait to be told (`chat_overlay::
/// keyboard_reserve`), which reserves the predicted height so its composer is
/// above the keys on the frame they open.
#[must_use]
pub fn expected_height_in(size: Vec2, ui_scale: f32, display_scale: f32) -> f32 {
    layout(
        Rect::from_corners(Vec2::ZERO, size),
        min_key_height_pts(ui_scale, display_scale),
        Edge::Bottom,
    )
    .rect
    .height()
        + EDGE_MARGIN
}

// ─── Fingers ──────────────────────────────────────────────────────────────────

/// What happened to one finger (or the mouse) this frame, in UI points.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct FingerEvent {
    pub id: u64,
    pub phase: FingerPhase,
    pub pos: Vec2,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FingerPhase {
    Start,
    Move,
    End,
    Cancel,
}

/// One finger on the keys, tracked by its own touch id.
#[derive(Clone, Copy, Debug)]
struct Finger {
    id: u64,
    pos: Vec2,
    /// When it went down, in seconds of real time.
    down_at: f64,
    /// When backspace last repeated under it.
    repeated_at: Option<f64>,
}

/// The pseudo touch id for a mouse driving the keys (`CARTO_OSK` on a desktop).
const MOUSE_FINGER: u64 = u64::MAX;
/// Held this long, backspace starts repeating, every [`REPEAT_EVERY_S`].
const REPEAT_AFTER_S: f64 = 0.45;
const REPEAT_EVERY_S: f64 = 0.06;
/// A click on a key within this long of a finger leaving the keys is that
/// finger's, already typed; one later is a click with no finger behind it (the
/// MCP server's `press`), which types.
const CLICK_GRACE_S: f64 = 1.0;
/// Frames the keys stand at the bottom before deciding their edge (module
/// note): one for the screens to read [`KeyboardInset`] and lift, one for the
/// layout to place them, one for `lib_keyboard` to measure the field.
const SETTLE_FRAMES: u8 = 3;

/// The character shown enlarged above a key under a finger — the phone
/// keyboard's key preview, because the thumb is covering the key itself.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Preview {
    /// In UI points, absolute.
    pub rect: Rect,
    pub ch: char,
}

/// One key as drawn: its place in the rows and its rect, in UI points.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct KeyPlace {
    pub row: usize,
    pub col: usize,
    pub rect: Rect,
}

// ─── Resource ─────────────────────────────────────────────────────────────────

/// The on-screen keyboard's whole state.
///
/// A resource so the rest of the UI can ask whether the keyboard is up and get
/// out from under it ([`Self::slab`], [`KeyboardInset`]).
#[derive(Resource)]
pub struct OnScreenKeyboard {
    /// Does this platform need us at all ([`platform_wants_keyboard`])? Resolved
    /// once, at construction: it cannot change during a run.
    enabled: bool,
    open: bool,
    /// Put away (the hide key, Back) while its field still holds the focus:
    /// stays down until the focus leaves, or the field's focus that is being
    /// cleared would bring the keys straight back.
    hidden: bool,
    /// The focused field is to lose its focus ([`Self::take_blur`]).
    blur: bool,
    layer: KeyLayer,
    shift: ShiftState,
    edge: Edge,
    /// Frames left before [`Self::edge`] is decided (module note).
    settle: u8,
    /// The focused field and its rect, last frame (`lib_keyboard`).
    field: Option<(Entity, Rect)>,
    /// This frame's slab, while the keys are up.
    slab: Option<Slab>,
    /// The fingers on the keys right now ([`Finger`]).
    fingers: Vec<Finger>,
    /// Has this device sent a touch? Once it has, the mouse presses the touch
    /// bridge synthesises for the primary finger are duplicates and ignored.
    touch_seen: bool,
    /// When a finger last left the keys ([`CLICK_GRACE_S`]).
    finger_left_at: f64,
    /// What the keys typed, for the focused field ([`Self::take_strokes`]).
    strokes: Vec<Stroke>,
}

/// How far the on-screen keyboard reaches up from the bottom edge, in UI
/// points (the library's px), and 0 while it is down or docked to the top. A
/// screen with a field at its foot (a composer) lifts by it. Written each
/// frame by [`drive_onscreen_keyboard`].
#[derive(Resource, Clone, Copy, PartialEq, Debug, Default)]
pub struct KeyboardInset(pub f32);

impl Default for OnScreenKeyboard {
    fn default() -> Self {
        let enabled = platform_wants_keyboard();
        if enabled {
            tracing::info!("on-screen keyboard enabled (no platform IME, or forced by CARTO_OSK)");
        }
        Self::new(enabled)
    }
}

impl OnScreenKeyboard {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            open: false,
            hidden: false,
            blur: false,
            layer: KeyLayer::default(),
            shift: ShiftState::default(),
            edge: Edge::default(),
            settle: 0,
            field: None,
            slab: None,
            fingers: Vec::new(),
            touch_seen: false,
            finger_left_at: f64::NEG_INFINITY,
            strokes: Vec::new(),
        }
    }

    /// Is the keyboard on screen right now?
    pub fn is_open(&self) -> bool {
        self.open && self.enabled
    }

    /// Put the keyboard away without typing anything, and the field's focus
    /// with it.
    ///
    /// Exists for Android's Back gesture, which has to dismiss the keyboard
    /// *before* it dismisses anything else — the system IME is the innermost
    /// thing Back closes on every other Android app, and a keyboard you cannot
    /// get rid of without submitting is worse than no keyboard.
    pub fn dismiss(&mut self) {
        if self.open {
            self.close();
            self.hidden = true;
            self.blur = true;
        }
    }

    fn close(&mut self) {
        self.open = false;
        self.slab = None;
        self.fingers.clear();
    }

    /// The rect the keyboard occupies, in UI points — the area a caller must
    /// consider unavailable.
    pub fn slab(&self) -> Option<Rect> {
        self.is_open()
            .then_some(self.slab)
            .flatten()
            .map(|s| s.rect)
    }

    /// Where the keys stand and each key's place, while they are up.
    pub fn keys(&self) -> Option<(Rect, Vec<KeyPlace>)> {
        if !self.is_open() {
            return None;
        }
        let slab = self.slab?;
        let keys = key_rects(&slab, self.layer)
            .into_iter()
            .map(|(row, col, _, rect)| KeyPlace { row, col, rect })
            .collect();
        Some((slab.rect, keys))
    }

    /// Which layer and shift the keys show.
    pub fn face(&self) -> (KeyLayer, ShiftState) {
        (self.layer, self.shift)
    }

    /// The keys under a finger right now, by row and column: drawn pressed.
    pub fn held(&self) -> Vec<(usize, usize)> {
        let Some(slab) = self.slab.filter(|_| self.is_open()) else {
            return Vec::new();
        };
        let mut out: Vec<(usize, usize)> = self
            .fingers
            .iter()
            .filter_map(|f| key_at(&slab, self.layer, f.pos).map(|(r, c, _, _)| (r, c)))
            .collect();
        out.dedup();
        out
    }

    /// The character under each finger on a character key, enlarged above the
    /// key (below it where above would leave the screen), kept within the slab
    /// at the outer columns.
    pub fn previews(&self) -> Vec<Preview> {
        let Some(slab) = self.slab.filter(|_| self.is_open()) else {
            return Vec::new();
        };
        self.fingers
            .iter()
            .filter_map(|f| {
                let (_, _, cap, key) = key_at(&slab, self.layer, f.pos)?;
                let Cap::Char { base, alt } = cap else {
                    return None;
                };
                let ch = if self.shift.alt() { alt } else { base };
                let size = Vec2::new(key.width().max(30.0) * 1.35, key.height() * 1.15);
                let x = (key.center().x - size.x / 2.0)
                    .max(slab.rect.min.x + 2.0)
                    .min(slab.rect.max.x - 2.0 - size.x);
                let above = key.min.y - 6.0 - size.y;
                let y = if above >= 0.0 { above } else { key.max.y + 6.0 };
                Some(Preview {
                    rect: Rect::from_corners(Vec2::new(x, y), Vec2::new(x, y) + size),
                    ch,
                })
            })
            .collect()
    }

    /// The focused field and its rect (last frame's layout), from the drawing
    /// side. Another field than before decides the edge afresh.
    pub fn set_field(&mut self, field: Option<(Entity, Rect)>) {
        if field.map(|f| f.0) != self.field.map(|f| f.0) && self.open {
            self.edge = Edge::Bottom;
            self.settle = SETTLE_FRAMES;
        }
        if self.field != field {
            self.field = field;
        }
    }

    /// What the keys typed since last asked, for the focused field.
    pub fn take_strokes(&mut self) -> Vec<Stroke> {
        std::mem::take(&mut self.strokes)
    }

    /// Did the keys go away (the hide key, Back)? Then the caller clears the
    /// field's focus; asked once.
    pub fn take_blur(&mut self) -> bool {
        std::mem::take(&mut self.blur)
    }

    /// Follow the focus: up while a text field has it, down when it leaves.
    fn follow_focus(&mut self, focused: bool) {
        if !self.enabled {
            return;
        }
        if !focused {
            self.hidden = false;
            if self.open {
                self.close();
            }
            return;
        }
        if self.open || self.hidden {
            return;
        }
        self.open = true;
        self.edge = Edge::Bottom;
        self.settle = SETTLE_FRAMES;
    }

    /// Lay the keys out for this frame, deciding the edge once the screens
    /// have had [`SETTLE_FRAMES`] to get out of the way.
    fn place(&mut self, viewport: Rect, min_key_h: f32) {
        if !self.is_open() {
            self.slab = None;
            return;
        }
        if self.settle > 0 {
            self.settle -= 1;
            if self.settle == 0 {
                self.edge = choose_edge(viewport, min_key_h, self.field.map(|f| f.1));
            }
        }
        self.slab = Some(layout(viewport, min_key_h, self.edge));
    }

    /// How far the keys reach up from the bottom of `viewport`: 0 unless they
    /// are up and docked there.
    fn inset(&self, viewport: Rect) -> f32 {
        match (self.slab(), self.edge) {
            (Some(slab), Edge::Bottom) => viewport.max.y - slab.min.y,
            _ => 0.0,
        }
    }

    /// Apply a key press: the strokes it types, and the layer, shift and open
    /// state it changes.
    ///
    /// Pure: the whole state machine is in here and nothing else, so it can be
    /// tested without a `World` or a screen.
    fn press(&mut self, cap: Cap) -> Vec<Stroke> {
        match cap {
            Cap::Char { base, alt } => {
                let ch = if self.shift.alt() { alt } else { base };
                self.shift = self.shift.consumed();
                vec![Stroke::Text(ch.to_string())]
            }
            Cap::Space => vec![Stroke::Text(" ".to_owned())],
            Cap::Backspace => vec![Stroke::Backspace],
            Cap::Enter => vec![Stroke::Enter],
            // **Shift turns the arrows into a selection**, the bargain a
            // physical keyboard makes. Shift is deliberately *not* consumed by
            // an arrow: a selection is grown one press at a time, and a shift
            // that fell off after the first would have to be re-armed for every
            // letter of the word being selected.
            Cap::Left => vec![Stroke::Left {
                select: self.shift.alt(),
            }],
            Cap::Right => vec![Stroke::Right {
                select: self.shift.alt(),
            }],
            Cap::SelectAll => vec![Stroke::SelectAll],
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
                self.dismiss();
                Vec::new()
            }
        }
    }

    /// Press `cap` and queue what it typed.
    fn type_cap(&mut self, cap: Cap) {
        if !self.is_open() {
            return;
        }
        let strokes = self.press(cap);
        self.strokes.extend(strokes);
    }

    /// A click on the key at `row`, `col`, from the drawn keys. Types only
    /// with no finger behind it ([`CLICK_GRACE_S`]): a finger's key has typed
    /// already, from [`Self::track`].
    pub fn clicked(&mut self, row: usize, col: usize, now: f64) {
        if !self.fingers.is_empty() || now - self.finger_left_at < CLICK_GRACE_S {
            return;
        }
        if let Some(cap) = cap_at(self.layer, row, col) {
            self.type_cap(cap);
        }
    }

    /// Follow every finger on the keys through this frame's events and type
    /// the keys they pressed, in order ([`Finger`] says why this is not a
    /// click).
    ///
    /// A key types as its finger **lifts**, and it is the key under the finger
    /// then — a thumb that lands on the edge of `e` and rolls onto `r` types `r`,
    /// as on a phone's own keyboard. Backspace is the exception both ways: it
    /// types as the finger lands and repeats while it stays, because deleting is
    /// the one key people hold.
    fn track(&mut self, events: &[FingerEvent], now: f64) {
        let Some(slab) = self.slab.filter(|_| self.is_open()) else {
            self.fingers.clear();
            return;
        };
        let mut caps = Vec::new();
        for e in events {
            match e.phase {
                FingerPhase::Start => {
                    let Some((_, _, cap, _)) = key_at(&slab, self.layer, e.pos) else {
                        continue;
                    };
                    self.fingers.retain(|f| f.id != e.id);
                    self.fingers.push(Finger {
                        id: e.id,
                        pos: e.pos,
                        down_at: now,
                        repeated_at: None,
                    });
                    if cap == Cap::Backspace {
                        caps.push(cap);
                    }
                }
                FingerPhase::Move => {
                    if let Some(f) = self.fingers.iter_mut().find(|f| f.id == e.id) {
                        f.pos = e.pos;
                    }
                }
                FingerPhase::End => {
                    let Some(i) = self.fingers.iter().position(|f| f.id == e.id) else {
                        continue;
                    };
                    self.fingers.remove(i);
                    self.finger_left_at = now;
                    // Lifted off the slab altogether: the finger changed its mind.
                    match key_at(&slab, self.layer, e.pos) {
                        Some((_, _, Cap::Backspace, _)) | None => {}
                        Some((_, _, cap, _)) => caps.push(cap),
                    }
                }
                FingerPhase::Cancel => {
                    if self.fingers.iter().any(|f| f.id == e.id) {
                        self.finger_left_at = now;
                    }
                    self.fingers.retain(|f| f.id != e.id);
                }
            }
        }
        let layer = self.layer;
        for f in &mut self.fingers {
            let on_backspace =
                matches!(key_at(&slab, layer, f.pos), Some((_, _, Cap::Backspace, _)));
            if !on_backspace || now - f.down_at < REPEAT_AFTER_S {
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
        self.fingers.retain(|f| now - f.down_at < 20.0);
        for cap in caps {
            // The hide key closes the keys; what follows it is not typed.
            self.type_cap(cap);
        }
    }
}

// ─── System ───────────────────────────────────────────────────────────────────

/// Raise and lower the keys with a library field's focus, place them, follow
/// the fingers on them and queue what they type; publish [`KeyboardInset`].
/// `lib_keyboard` draws the keys and hands the strokes to the field.
#[allow(clippy::too_many_arguments)]
pub fn drive_onscreen_keyboard(
    mut kb: ResMut<OnScreenKeyboard>,
    input: Res<UiInput>,
    metrics: Res<UiMetrics>,
    time: Res<Time<Real>>,
    touches: Option<Res<Touches>>,
    mut mouse: MessageReader<MouseButtonInput>,
    windows: Query<&Window, With<PrimaryWindow>>,
    inset: Option<ResMut<KeyboardInset>>,
    mut commands: Commands,
) {
    if !kb.enabled {
        mouse.clear();
        return;
    }
    kb.follow_focus(input.overlay_owns_keyboard());

    let viewport = Rect::from_corners(Vec2::ZERO, metrics.viewport_pts());
    let min_key_h = min_key_height_pts(metrics.ui_scale(), metrics.display_scale());
    kb.place(viewport, min_key_h);

    // This frame's fingers, in UI points: the touches first, then — until the
    // device has shown it has touches, after which they are the bridge's
    // copies of the first finger — the mouse as one more finger.
    let mut events = Vec::new();
    if let Some(touches) = touches.as_deref() {
        let to_ui = |t: &bevy::input::touch::Touch| metrics.window_to_ui(t.position());
        for t in touches.iter_just_pressed() {
            events.push(FingerEvent {
                id: t.id(),
                phase: FingerPhase::Start,
                pos: to_ui(t),
            });
        }
        for t in touches.iter() {
            events.push(FingerEvent {
                id: t.id(),
                phase: FingerPhase::Move,
                pos: to_ui(t),
            });
        }
        for t in touches.iter_just_released() {
            events.push(FingerEvent {
                id: t.id(),
                phase: FingerPhase::End,
                pos: to_ui(t),
            });
        }
        for t in touches.iter_just_canceled() {
            events.push(FingerEvent {
                id: t.id(),
                phase: FingerPhase::Cancel,
                pos: to_ui(t),
            });
        }
    }
    if !events.is_empty() {
        kb.touch_seen = true;
    }
    let cursor = windows
        .iter()
        .next()
        .and_then(Window::cursor_position)
        .map(|p| metrics.window_to_ui(p));
    for m in mouse.read() {
        if kb.touch_seen || m.button != MouseButton::Left {
            continue;
        }
        let Some(pos) = cursor else {
            continue;
        };
        events.push(FingerEvent {
            id: MOUSE_FINGER,
            phase: match m.state {
                ButtonState::Pressed => FingerPhase::Start,
                ButtonState::Released => FingerPhase::End,
            },
            pos,
        });
    }
    if !kb.touch_seen {
        if let Some(pos) = cursor {
            events.push(FingerEvent {
                id: MOUSE_FINGER,
                phase: FingerPhase::Move,
                pos,
            });
        }
    }
    kb.track(&events, time.elapsed_secs_f64());

    let reach = kb.inset(viewport);
    match inset {
        Some(mut inset) => {
            if inset.0 != reach {
                inset.0 = reach;
            }
        }
        None => commands.insert_resource(KeyboardInset(reach)),
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
